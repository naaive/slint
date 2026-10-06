// SPDX-License-Identifier: MIT

//! The UI: binds the Slint `AppWindow` to the [`ConfigStore`] and the data [`Sources`].

mod bluetooth;
mod data;
mod displays;
mod network;
mod picker;
mod shortcuts;
mod sync;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use nimbus_config::{Appearance, Config};
use nimbus_theme::ThemeSettings;
use slint::{ComponentHandle, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::dispatch::Dispatch;
use crate::page::Page;
use crate::settings::{self, Key, Value};
use crate::sources::{Sources, SystemSources};
use crate::store::{ConfigStore, SaveStatus};
use crate::wallpapers::Wallpaper;
use crate::{AppWindow, Nav, Prefs, Theme};

pub use picker::PickerKind;

/// How the color scheme is resolved for `ColorScheme::System`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemScheme {
    /// Ask the XDG desktop portal, on a background thread.
    Portal,
    /// Use a fixed answer, synchronously; for screenshots and tests.
    Fixed { dark: bool },
}

/// Everything [`App::new`] needs.
pub struct AppOptions {
    pub config_path: PathBuf,
    pub page: Page,
    pub sources: Arc<dyn Sources>,
    pub dispatch: Dispatch,
    pub system_scheme: SystemScheme,
    /// Watch the configuration file for changes made by other programs.
    pub watch: bool,
    pub debounce: Duration,
}

impl AppOptions {
    /// Options for the real app editing `config_path`.
    pub fn system(config_path: PathBuf, page: Page) -> Self {
        Self {
            config_path,
            page,
            sources: Arc::new(SystemSources::default()),
            dispatch: Dispatch::Threaded,
            system_scheme: SystemScheme::Portal,
            watch: true,
            debounce: crate::store::DEBOUNCE,
        }
    }
}

/// Results that arrive from background threads.
pub(crate) enum Message {
    ExternalConfig(Box<Config>),
    Saved(SaveStatus),
    Theme(ThemeSettings),
    Fonts(Vec<String>),
    Rules(crate::xkb::Rules),
    Wallpapers(Vec<Wallpaper>),
    Thumbnail(String, SharedPixelBuffer<Rgba8Pixel>),
    About(Box<crate::about::SystemInfo>, Option<SharedPixelBuffer<Rgba8Pixel>>),
    Outputs(Result<Vec<nimbus_ipc::OutputInfo>, String>),
    Display(crate::displays::DisplayEvent),
    Network(nimbus_services::nm::Event),
    Bluetooth(nimbus_services::bluez::Event),
}

thread_local! {
    /// The app on the UI thread, which background results are delivered to.
    static APP: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
}

/// Runs `f` with the app; must run on the UI thread.
pub(crate) fn with_app(f: impl FnOnce(&Inner)) {
    let app = APP.with(|app| app.borrow().upgrade());
    match app {
        Some(app) => f(&app),
        None => tracing::debug!("dropping a callback: the app is gone"),
    }
}

/// Delivers `message` to the app; must run on the UI thread.
pub(crate) fn deliver(message: Message) {
    with_app(|app| app.handle(message));
}

/// Delivers `message` from any thread.
pub(crate) fn post(message: Message) {
    if let Err(error) = slint::invoke_from_event_loop(move || deliver(message)) {
        tracing::debug!("dropping a background result: {error}");
    }
}

/// UI state that isn't in the configuration.
#[derive(Default)]
pub(crate) struct State {
    pub rules: Option<Rc<crate::xkb::Rules>>,
    pub fonts: Vec<String>,
    pub wallpapers: Vec<Wallpaper>,
    pub picker: Option<picker::Open>,
    pub capture: Option<shortcuts::Capture>,
    pub lock_values: Vec<u32>,
    /// The user chose "Custom" for the clock, so the custom field stays visible even for a preset format.
    pub custom_clock: bool,
    pub displays: displays::Displays,
    pub network: network::Network,
    pub bluetooth: bluetooth::Bluetooth,
    pub applied_appearance: Option<Appearance>,
    /// Plain copies of the last pushed list models, to skip pushes that change nothing.
    pub pushed_sources: Vec<(String, String, bool)>,
    pub pushed_options: Vec<usize>,
    pub pushed_shortcuts: Option<(String, nimbus_config::Keybindings)>,
    pub save_failed: bool,
}

pub(crate) struct Inner {
    pub ui: slint::Weak<AppWindow>,
    pub store: RefCell<ConfigStore>,
    pub sources: Arc<dyn Sources>,
    pub dispatch: Dispatch,
    pub system_scheme: SystemScheme,
    pub state: RefCell<State>,
    pub theme_requests: Option<std::sync::mpsc::Sender<Appearance>>,
    pub wallpaper_model: Rc<slint::VecModel<crate::WallpaperItem>>,
}

/// The running settings window.
pub struct App {
    ui: AppWindow,
    inner: Rc<Inner>,
    _watcher: Option<nimbus_config::ConfigWatcher>,
}

impl App {
    pub fn new(options: AppOptions) -> Result<App, slint::PlatformError> {
        let ui = AppWindow::new()?;
        let (store, load_error) =
            ConfigStore::open(&options.config_path, options.debounce, |status| {
                post(Message::Saved(status));
            });
        let theme_requests = match (options.system_scheme, options.dispatch) {
            (SystemScheme::Portal, Dispatch::Threaded) => data::start_theme_worker(),
            _ => None,
        };
        let inner = Rc::new(Inner {
            ui: ui.as_weak(),
            store: RefCell::new(store),
            sources: options.sources,
            dispatch: options.dispatch,
            system_scheme: options.system_scheme,
            state: RefCell::new(State::default()),
            theme_requests,
            wallpaper_model: Rc::new(slint::VecModel::default()),
        });
        APP.with(|app| *app.borrow_mut() = Rc::downgrade(&inner));

        // Resolve the theme before the window first shows, so it doesn't flash the wrong scheme.
        let appearance = inner.store.borrow().config().appearance.clone();
        let theme = match options.system_scheme {
            SystemScheme::Portal => ThemeSettings::from_config(&appearance),
            SystemScheme::Fixed { dark } => {
                ThemeSettings::from_config_with_system(&appearance, || dark)
            }
        };
        nimbus_theme::apply_theme!(ui, theme);
        inner.state.borrow_mut().applied_appearance = Some(appearance);

        ui.global::<Nav>().set_page(options.page.index() as i32);
        ui.global::<Prefs>().set_wallpapers(inner.wallpaper_model.clone().into());
        if let Some(error) = load_error {
            inner.show_banner(
                &format!("Your settings file couldn't be read, so defaults are shown. It's backed up before the first change. ({error})"),
                true,
            );
        }
        wire(&ui, &inner);
        inner.sync_all();
        inner.load_data();
        inner.page_shown(options.page);

        let watcher = if options.watch {
            let path = inner.store.borrow().path().to_path_buf();
            match nimbus_config::watch(&path, |config| {
                post(Message::ExternalConfig(Box::new(config)))
            }) {
                Ok(watcher) => Some(watcher),
                Err(error) => {
                    tracing::warn!("not watching {} for changes: {error}", path.display());
                    None
                }
            }
        } else {
            None
        };
        Ok(App { ui, inner, _watcher: watcher })
    }

    pub fn window(&self) -> &AppWindow {
        &self.ui
    }

    pub fn config(&self) -> Config {
        self.inner.store.borrow().config().clone()
    }

    /// Writes pending changes now.
    pub fn flush(&self) {
        self.inner.store.borrow().flush(Duration::from_secs(5));
    }

    /// Shows the window and runs the event loop until it closes.
    pub fn run(&self) -> Result<(), slint::PlatformError> {
        self.ui.run()?;
        self.flush();
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        APP.with(|app| {
            let mut app = app.borrow_mut();
            if app.ptr_eq(&Rc::downgrade(&self.inner)) {
                *app = Weak::new();
            }
        });
    }
}

impl Inner {
    pub fn with_ui(&self, f: impl FnOnce(&AppWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            f(&ui);
        }
    }

    pub fn show_banner(&self, text: &str, error: bool) {
        self.with_ui(|ui| {
            let nav = ui.global::<Nav>();
            nav.set_banner(text.into());
            nav.set_banner_is_error(error);
        });
    }

    /// Applies one edit from the UI and refreshes what depends on it.
    pub fn edit(&self, key: &str, value: Value) {
        let key: Key = match key.parse() {
            Ok(key) => key,
            Err(error) => {
                tracing::error!("{error}");
                return;
            }
        };
        let mut result = Ok(());
        let changed =
            self.store.borrow_mut().update(|config| result = settings::apply(config, key, value));
        if let Err(error) = &result {
            tracing::debug!("rejected edit: {error}");
        }
        if changed || result.is_err() {
            self.sync_prefs();
        }
    }

    /// Replaces the configuration through `edit`, then refreshes everything.
    pub fn update(&self, edit: impl FnOnce(&mut Config)) {
        if self.store.borrow_mut().update(edit) {
            self.sync_all();
        }
    }

    /// Refreshes what a page shows when it opens: networks are scanned, and Bluetooth looks for devices.
    pub fn page_shown(&self, page: Page) {
        if page == Page::Network {
            self.send_network(nimbus_services::nm::Command::Scan);
        }
        self.show_bluetooth_page(page == Page::Bluetooth);
    }

    pub fn handle(&self, message: Message) {
        match message {
            Message::ExternalConfig(config) => {
                let changed = self.store.borrow_mut().external_change(*config);
                if changed {
                    self.sync_all();
                }
            }
            Message::Saved(SaveStatus::Saved) => {
                let failed = std::mem::take(&mut self.state.borrow_mut().save_failed);
                if failed {
                    self.with_ui(|ui| ui.global::<Nav>().set_banner("".into()));
                }
            }
            Message::Saved(SaveStatus::Failed(error)) => {
                self.state.borrow_mut().save_failed = true;
                self.show_banner(&format!("Settings couldn't be saved: {error}"), true);
            }
            Message::Theme(theme) => self.with_ui(|ui| nimbus_theme::apply_theme!(ui, theme)),
            other => self.handle_data(other),
        }
    }
}

fn text_of(value: &slint::SharedString) -> String {
    value.as_str().to_string()
}

fn wire(ui: &AppWindow, inner: &Rc<Inner>) {
    let weak = Rc::downgrade(inner);
    let with: With = Rc::new(move |f| {
        if let Some(inner) = weak.upgrade() {
            f(&inner);
        }
    });

    let nav = ui.global::<Nav>();
    let h = with.clone();
    nav.on_search_edited(move |text| h(&|i| i.sync_search(text.as_str())));
    let h = with.clone();
    nav.on_page_shown(move |page| {
        h(&|i| {
            if let Some(page) = Page::from_index(page) {
                i.page_shown(page);
            }
        })
    });
    let h = with.clone();
    nav.on_dismiss_banner(move || h(&|i| i.with_ui(|ui| ui.global::<Nav>().set_banner("".into()))));
    let ui_weak = ui.as_weak();
    nav.on_quit(move || {
        if let Some(ui) = ui_weak.upgrade()
            && let Err(error) = ui.hide()
        {
            tracing::warn!("cannot close the window: {error}");
        }
    });

    let prefs = ui.global::<Prefs>();
    let h = with.clone();
    prefs.on_set_bool(move |key, value| h(&|i| i.edit(&key, Value::Bool(value))));
    let h = with.clone();
    prefs.on_set_int(move |key, value| h(&|i| i.edit(&key, Value::Int(i64::from(value)))));
    let h = with.clone();
    prefs.on_set_float(move |key, value| h(&|i| i.edit(&key, Value::Float(f64::from(value)))));
    let h = with.clone();
    prefs.on_set_text(move |key, value| h(&|i| i.edit(&key, Value::Text(text_of(&value)))));
    prefs.on_is_color(|text| nimbus_theme::parse_hex_color(&text).is_some());
    let h = with.clone();
    prefs.on_choose_clock_preset(move |index| h(&|i| i.choose_clock_preset(index)));
    let h = with.clone();
    prefs.on_choose_timeout(move |key, index| h(&|i| i.choose_timeout(&key, index)));
    let h = with.clone();
    prefs.on_choose_option(move |group, index| h(&|i| i.choose_option(group, index)));
    let h = with.clone();
    prefs.on_pick_font(move || h(&|i| i.open_picker(PickerKind::Font)));
    let h = with.clone();
    prefs.on_add_source(move || h(&|i| i.open_picker(PickerKind::Layout)));
    let h = with.clone();
    prefs.on_pick_variant(move |index| h(&|i| i.open_picker(PickerKind::Variant(index_of(index)))));
    let h = with.clone();
    prefs.on_remove_source(move |index| {
        h(&|i| i.edit_sources(|sources| crate::xkb::remove_source(sources, index_of(index))));
    });
    let h = with.clone();
    prefs.on_move_source_up(move |index| {
        h(&|i| i.edit_sources(|sources| crate::xkb::move_up(sources, index_of(index))));
    });

    picker::wire(ui, &with);
    shortcuts::wire(ui, &with);
    displays::wire(ui, &with);
    network::wire(ui, &with);
    bluetooth::wire(ui, &with);
}

/// A UI index as `usize`; negative indices, which name nothing, become `usize::MAX`.
pub(crate) fn index_of(index: i32) -> usize {
    usize::try_from(index).unwrap_or(usize::MAX)
}

/// The inverse of [`index_of`]: `None` becomes -1.
pub(super) fn to_index(found: Option<usize>) -> i32 {
    found.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1)
}

pub(super) fn strings(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.into_iter().map(SharedString::from).collect::<Vec<_>>()))
}

/// Shared handle type for the callbacks above.
pub(crate) type With = Rc<dyn Fn(&dyn Fn(&Inner))>;

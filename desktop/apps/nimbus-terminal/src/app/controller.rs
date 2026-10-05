// SPDX-License-Identifier: MIT

//! The window's state and its reactions to UI callbacks and [`AppEvent`]s.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::Event;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape as TermCursorShape, CursorStyle};
use futures_channel::mpsc::UnboundedSender;
use slint::{ComponentHandle, Model, SharedString, Timer, TimerMode, VecModel};

use super::workers::{self, PrefsWriter};
use super::{AppEvent, input, models};
use crate::clipboard::{Clipboard, ClipboardKind};
use crate::engine::{EventSink, GridSize, Session, SessionEvent, SessionId, SpawnOptions, feed};
use crate::fonts::FontSet;
use crate::glyphs::GlyphCache;
use crate::mouse::{ClickCounter, ScrollAccumulator};
use crate::palette::{self, ColorTable};
use crate::prefs::{self, CursorShape, Prefs};
use crate::render::{Renderer, ViewOptions};
use crate::search::{Search, SearchDirection};
use crate::shortcuts::Action;
use crate::{AppWindow, selection};

/// The shortest time between frames, to coalesce bursts of output.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);
const BLINK_INTERVAL: Duration = Duration::from_millis(600);
/// The cursor stops blinking after this long without input, to save power.
const BLINK_TIMEOUT: Duration = Duration::from_secs(30);
const BELL_FLASH: Duration = Duration::from_millis(150);
/// Points added or removed by one zoom step.
const ZOOM_STEP: f32 = 1.0;

/// What the window starts with.
pub struct Options {
    pub prefs: Prefs,
    /// Where to save preference changes; nowhere when `None`.
    pub prefs_path: Option<PathBuf>,
    pub appearance: nimbus_config::Appearance,
    /// A fixed window title that replaces the titles programs set.
    pub title: Option<String>,
}

/// How a tab's program ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitReason {
    Code(i32),
    Signal(i32),
    /// The terminal hung up without an exit status.
    Unknown,
}

impl ExitReason {
    fn of(status: std::process::ExitStatus) -> Self {
        if let Some(code) = status.code() {
            return Self::Code(code);
        }
        #[cfg(unix)]
        if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
            return Self::Signal(signal);
        }
        Self::Unknown
    }

    /// What went wrong, for the note in the tab, or `None` when the program finished cleanly.
    fn failure(self) -> Option<String> {
        match self {
            Self::Code(0) | Self::Unknown => None,
            Self::Code(code) => Some(format!("exited with code {code}")),
            Self::Signal(signal) => Some(format!("was terminated by signal {signal}")),
        }
    }

    /// The exit code a shell reports, which is 128 plus the number of a fatal signal.
    fn code(self) -> i32 {
        match self {
            Self::Code(code) => code,
            Self::Signal(signal) => 128 + signal,
            Self::Unknown => 0,
        }
    }
}

pub(super) struct Tab {
    pub(super) session: Session,
    /// The title set by the program, or its name.
    pub(super) title: String,
    default_title: String,
    pub(super) bell: bool,
    /// The exit code, once the program has exited.
    pub(super) exited: Option<i32>,
    pub(super) search: Search,
}

/// What a confirmation dialog would close.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    CloseTab(SessionId),
    Quit,
}

#[derive(Default)]
pub(super) struct MouseState {
    /// The button held down, when it started a selection or a report to the program.
    pub(super) pressed: Option<crate::mouse::Button>,
    pub(super) selecting: bool,
    pub(super) clicks: ClickCounter,
    pub(super) scroll: ScrollAccumulator,
    pub(super) zoom: ScrollAccumulator,
    /// The last cell reported to the program, to skip motion within a cell.
    pub(super) last_cell: Option<(usize, usize)>,
}

pub(super) struct State {
    pub(super) tabs: Vec<Tab>,
    pub(super) current: usize,
    next_id: SessionId,
    pub(super) prefs: Prefs,
    prefs_writer: PrefsWriter,
    /// Points added to the preferred font size by zooming.
    zoom: f32,
    fonts: Option<Arc<FontSet>>,
    pub(super) renderer: Option<Renderer>,
    pub(super) scale: f32,
    pub(super) grid: GridSize,
    pub(super) theme_dark: bool,
    pub(super) clipboard: Clipboard,
    title_override: Option<String>,
    pub(super) focused: bool,
    blink_on: bool,
    pub(super) last_input: Instant,
    render_scheduled: bool,
    last_render: Instant,
    pending: Option<Pending>,
    quit_confirmed: bool,
    pub(super) mouse: MouseState,
}

/// The window's controller. UI callbacks and events from other threads end up here, on the UI thread.
pub struct Controller {
    me: Weak<Controller>,
    pub(super) window: slint::Weak<AppWindow>,
    pub(super) events: UnboundedSender<AppEvent>,
    pub(super) state: RefCell<State>,
    tabs_model: Rc<VecModel<crate::ui::TabInfo>>,
    blink_timer: Timer,
    bell_timer: Timer,
    render_timer: Timer,
}

fn program_name(command: Option<&(String, Vec<String>)>) -> String {
    let program = match command {
        Some((program, _)) => program.clone(),
        None => std::env::var("SHELL").unwrap_or_else(|_| "Terminal".into()),
    };
    program.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("Terminal").to_string()
}

pub(super) fn term_config(prefs: &Prefs) -> Config {
    let shape = match prefs.cursor_shape {
        CursorShape::Block => TermCursorShape::Block,
        CursorShape::Beam => TermCursorShape::Beam,
        CursorShape::Underline => TermCursorShape::Underline,
    };
    Config {
        scrolling_history: prefs.scrollback_lines,
        default_cursor_style: CursorStyle { shape, blinking: prefs.cursor_blink },
        ..Config::default()
    }
}

impl Controller {
    pub fn new(
        window: &AppWindow,
        events: UnboundedSender<AppEvent>,
        options: Options,
    ) -> Rc<Self> {
        let initial_theme =
            nimbus_theme::ThemeSettings::from_config_with_system(&options.appearance, || true);
        nimbus_theme::apply_theme!(window, initial_theme, crate::ui::Theme);
        let tabs_model = Rc::new(VecModel::default());
        window.set_tabs(tabs_model.clone().into());
        let controller = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            window: window.as_weak(),
            events,
            state: RefCell::new(State {
                tabs: Vec::new(),
                current: 0,
                next_id: 1,
                prefs_writer: PrefsWriter::start(options.prefs_path),
                prefs: options.prefs,
                zoom: 0.0,
                fonts: None,
                renderer: None,
                scale: window.window().scale_factor(),
                grid: GridSize::DEFAULT,
                theme_dark: initial_theme.dark,
                clipboard: Clipboard::start(),
                title_override: options.title,
                focused: true,
                blink_on: true,
                last_input: Instant::now(),
                render_scheduled: false,
                last_render: Instant::now(),
                pending: None,
                quit_confirmed: false,
                mouse: MouseState::default(),
            }),
            tabs_model,
            blink_timer: Timer::default(),
            bell_timer: Timer::default(),
            render_timer: Timer::default(),
        });
        models::init_preferences(window, &controller.state.borrow().prefs);
        controller.connect(window);
        controller.restart_blink();
        controller
    }

    /// Runs `f` on the state, unless the state is already borrowed further up the stack.
    pub(super) fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        match self.state.try_borrow_mut() {
            Ok(mut state) => Some(f(&mut state)),
            Err(_) => {
                tracing::error!("re-entrant terminal state access was skipped");
                None
            }
        }
    }

    fn connect(self: &Rc<Self>, window: &AppWindow) {
        let weak = Rc::downgrade(self);
        let with = move |f: fn(&Rc<Controller>)| {
            let weak = weak.clone();
            move || {
                if let Some(controller) = weak.upgrade() {
                    f(&controller);
                }
            }
        };
        window.on_new_tab(with(|c| c.action(Action::NewTab)));
        window.on_geometry_changed(with(|c| c.update_geometry()));
        window.on_search_older(with(|c| c.search(Some(SearchDirection::Older))));
        window.on_search_newer(with(|c| c.search(Some(SearchDirection::Newer))));
        window.on_search_closed(with(|c| c.close_search()));
        window.on_confirm_accepted(with(|c| c.confirm(true)));
        window.on_confirm_cancelled(with(|c| c.confirm(false)));

        let weak = Rc::downgrade(self);
        window.on_close_tab(move |index| {
            if let (Some(c), Ok(index)) = (weak.upgrade(), usize::try_from(index)) {
                c.close_tab(index);
            }
        });
        let weak = Rc::downgrade(self);
        window.on_select_tab(move |index| {
            if let (Some(c), Ok(index)) = (weak.upgrade(), usize::try_from(index)) {
                c.select_tab(index);
            }
        });
        let weak = Rc::downgrade(self);
        window.on_search_edited(move |text| {
            if let Some(c) = weak.upgrade() {
                c.search_edited(&text);
            }
        });
        let weak = Rc::downgrade(self);
        window.on_menu_action(move |id| {
            let action = match id.as_str() {
                "copy" => Action::Copy,
                "paste" => Action::Paste,
                "select-all" => Action::SelectAll,
                "new-tab" => Action::NewTab,
                "find" => Action::Find,
                "preferences" => Action::Preferences,
                other => {
                    tracing::warn!("unknown menu entry {other:?}");
                    return;
                }
            };
            if let Some(c) = weak.upgrade() {
                c.action(action);
            }
        });
        let weak = Rc::downgrade(self);
        window.on_focus_changed(move |focused| {
            if let Some(c) = weak.upgrade() {
                c.focus_changed(focused);
            }
        });
        input::connect(self, window);
        models::connect_preferences(self, window);
    }

    /// The preferred monospace family, empty for automatic.
    pub fn font_family(&self) -> String {
        self.state.borrow().prefs.font_family.clone()
    }

    fn sink(&self) -> EventSink {
        let events = self.events.clone();
        Arc::new(move |id, event| workers::send(&events, AppEvent::Session(id, event)))
    }

    /// Opens a tab running `options` and makes it current.
    pub fn open_tab(self: &Rc<Self>, options: SpawnOptions) {
        let sink = self.sink();
        self.with_state(|st| {
            let id = st.next_id;
            st.next_id += 1;
            let size = st.grid.window_size(cell_size(st).0, cell_size(st).1);
            let default_title = program_name(options.command.as_ref());
            let session = Session::spawn(id, options, size, term_config(&st.prefs), sink);
            st.tabs.push(Tab {
                session,
                title: default_title.clone(),
                default_title,
                bell: false,
                exited: None,
                search: Search::default(),
            });
            st.current = st.tabs.len() - 1;
            if let Some(renderer) = st.renderer.as_mut() {
                renderer.invalidate();
            }
        });
        self.after_tab_change();
    }

    /// Adds a tab showing a session without a process; for samples and tests.
    pub fn open_detached_tab(self: &Rc<Self>, title: &str, content: &[u8]) {
        let sink = self.sink();
        self.with_state(|st| {
            let id = st.next_id;
            st.next_id += 1;
            let session = Session::detached(id, st.grid, term_config(&st.prefs), sink);
            feed(&mut *session.term.lock(), content);
            st.tabs.push(Tab {
                session,
                title: title.to_string(),
                default_title: title.to_string(),
                bell: false,
                exited: None,
                search: Search::default(),
            });
            st.current = st.tabs.len() - 1;
        });
        self.after_tab_change();
    }

    /// The number of open tabs.
    pub fn tab_count(&self) -> usize {
        self.state.borrow().tabs.len()
    }

    /// Selects the tab at `index` and redraws.
    pub fn select_tab(&self, index: usize) {
        let changed = self.with_state(|st| {
            if index >= st.tabs.len() || index == st.current {
                return false;
            }
            st.current = index;
            st.tabs[index].bell = false;
            if let Some(renderer) = st.renderer.as_mut() {
                renderer.invalidate();
            }
            true
        });
        if changed == Some(true) {
            self.after_tab_change();
        }
    }

    fn after_tab_change(&self) {
        self.sync_tabs();
        self.sync_search_bar();
        self.update_geometry();
        self.render_now();
    }

    /// Refreshes the tab strip and the window title.
    pub(super) fn sync_tabs(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let Ok(st) = self.state.try_borrow() else { return };
        let infos: Vec<crate::ui::TabInfo> = st
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| crate::ui::TabInfo {
                title: SharedString::from(match (&st.title_override, i) {
                    (Some(title), 0) => title.as_str(),
                    _ => tab.title.as_str(),
                }),
                bell: tab.bell,
                exited: tab.exited.is_some(),
            })
            .collect();
        if self.tabs_model.row_count() != infos.len()
            || infos
                .iter()
                .enumerate()
                .any(|(i, info)| self.tabs_model.row_data(i).as_ref() != Some(info))
        {
            self.tabs_model.set_vec(infos.clone());
        }
        window.set_current_tab(st.current as i32);
        let title =
            infos.get(st.current).map(|i| i.title.clone()).unwrap_or_else(|| "Terminal".into());
        window.set_window_title(title);
    }

    fn sync_search_bar(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let Ok(st) = self.state.try_borrow() else { return };
        let pattern =
            st.tabs.get(st.current).map(|t| t.search.pattern().to_string()).unwrap_or_default();
        if window.get_search_open() && window.get_search_text() != pattern.as_str() {
            window.set_search_text(pattern.into());
            window.set_search_status(SharedString::new());
        }
    }

    /// Asks to close the tab at `index`, confirming first when a program runs in it.
    pub fn close_tab(&self, index: usize) {
        let check = self.with_state(|st| {
            let tab = st.tabs.get(index)?;
            Some((
                tab.session.id,
                if tab.exited.is_some() { None } else { tab.session.process_handle() },
            ))
        });
        if let Some(Some((id, handle))) = check {
            match handle {
                Some(handle) => workers::check_close(self.events.clone(), id, Some(handle)),
                None => self.remove_tab(id),
            }
        }
    }

    fn remove_tab(&self, id: SessionId) {
        let remaining = self.with_state(|st| {
            let index = st.tabs.iter().position(|t| t.session.id == id)?;
            st.tabs.remove(index);
            if st.current >= index && st.current > 0 {
                st.current -= 1;
            }
            if let Some(renderer) = st.renderer.as_mut() {
                renderer.invalidate();
            }
            Some(st.tabs.len())
        });
        match remaining.flatten() {
            Some(0) => self.quit(),
            Some(_) => self.after_tab_change(),
            None => {}
        }
    }

    /// Handles the window manager's close button, confirming first when programs run in any tab.
    pub fn close_requested(&self) -> slint::CloseRequestResponse {
        let handles = self.with_state(|st| {
            if st.quit_confirmed {
                return None;
            }
            Some(
                st.tabs
                    .iter()
                    .filter(|t| t.exited.is_none())
                    .filter_map(|t| t.session.process_handle())
                    .collect(),
            )
        });
        match handles.flatten() {
            None => slint::CloseRequestResponse::HideWindow,
            Some(handles) => {
                workers::check_quit(self.events.clone(), handles);
                slint::CloseRequestResponse::KeepWindowShown
            }
        }
    }

    fn quit(&self) {
        self.with_state(|st| st.quit_confirmed = true);
        if let Some(window) = self.window.upgrade() {
            let _ = window.hide();
        }
        let _ = slint::quit_event_loop();
    }

    /// Ends every session.
    pub fn shutdown(&self) {
        self.with_state(|st| st.tabs.clear());
    }

    fn ask(&self, pending: Pending, title: &str, message: String, action: &str) {
        let Some(window) = self.window.upgrade() else { return };
        self.with_state(|st| st.pending = Some(pending));
        window.set_confirm_title(title.into());
        window.set_confirm_message(message.into());
        window.set_confirm_action(action.into());
        window.set_confirm_open(true);
    }

    fn confirm(&self, accepted: bool) {
        let pending = self.with_state(|st| st.pending.take()).flatten();
        if let Some(window) = self.window.upgrade() {
            window.set_confirm_open(false);
            window.invoke_focus_terminal();
        }
        match (accepted, pending) {
            (true, Some(Pending::CloseTab(id))) => self.remove_tab(id),
            (true, Some(Pending::Quit)) => self.quit(),
            _ => {}
        }
    }

    /// Handles an event from another thread.
    pub fn handle(self: &Rc<Self>, event: AppEvent) {
        match event {
            AppEvent::Session(id, event) => self.session_event(id, event),
            AppEvent::FontsLoaded(Ok(fonts)) => self.set_fonts(fonts),
            AppEvent::FontsLoaded(Err(error)) => tracing::error!("cannot draw text: {error}"),
            AppEvent::Theme(settings) => self.set_theme(&settings),
            AppEvent::Paste(Some(text)) => self.paste_text(&text),
            AppEvent::Paste(None) => {}
            AppEvent::CloseChecked { session, running: None } => self.remove_tab(session),
            AppEvent::CloseChecked { session, running: Some(name) } => self.ask(
                Pending::CloseTab(session),
                "Close Tab?",
                format!("“{name}” is still running. Closing the tab will end it."),
                "Close Tab",
            ),
            AppEvent::QuitChecked { running } if running.is_empty() => self.quit(),
            AppEvent::QuitChecked { running } => {
                let names = running.join(", ");
                let message = if running.len() == 1 {
                    format!("“{names}” is still running. Closing the window will end it.")
                } else {
                    format!(
                        "{} programs are still running: {names}. Closing the window will end them.",
                        running.len()
                    )
                };
                self.ask(Pending::Quit, "Close Window?", message, "Close Window");
            }
            AppEvent::OpenTab(directory) => {
                self.open_tab(SpawnOptions { command: None, working_directory: directory });
            }
            AppEvent::OpenWindow(directory) => workers::open_window(directory),
        }
    }

    fn session_event(self: &Rc<Self>, id: SessionId, event: SessionEvent) {
        match event {
            SessionEvent::Started(handle) => {
                let handle =
                    self.with_state(|st| match st.tabs.iter_mut().find(|t| t.session.id == id) {
                        Some(tab) => {
                            tab.session.started(handle);
                            None
                        }
                        None => Some(handle),
                    });
                // The tab closed while its program was starting.
                if let Some(Some(handle)) = handle {
                    handle.stop();
                }
            }
            SessionEvent::Failed(error) => {
                tracing::warn!("{error}");
                let message = format!("\x1b[1;31mCannot start the program:\x1b[0m {error}\r\n");
                self.with_state(|st| {
                    if let Some(tab) = st.tabs.iter_mut().find(|t| t.session.id == id) {
                        feed(&mut *tab.session.term.lock(), message.as_bytes());
                        tab.exited = Some(-1);
                        tab.session.stopped();
                    }
                });
                self.sync_tabs();
                self.schedule_render();
            }
            SessionEvent::Term(event) => self.term_event(id, event),
        }
    }

    fn term_event(self: &Rc<Self>, id: SessionId, event: Event) {
        let is_current = self
            .with_state(|st| st.tabs.get(st.current).is_some_and(|t| t.session.id == id))
            .unwrap_or(false);
        match event {
            Event::Wakeup => {
                if is_current {
                    self.schedule_render();
                }
            }
            Event::Title(title) => {
                self.with_state(|st| {
                    if let Some(tab) = st.tabs.iter_mut().find(|t| t.session.id == id) {
                        tab.title = title;
                    }
                });
                self.sync_tabs();
            }
            Event::ResetTitle => {
                self.with_state(|st| {
                    if let Some(tab) = st.tabs.iter_mut().find(|t| t.session.id == id) {
                        tab.title = tab.default_title.clone();
                    }
                });
                self.sync_tabs();
            }
            Event::Bell => self.bell(id, is_current),
            Event::ClipboardStore(kind, text) => {
                let kind = match kind {
                    alacritty_terminal::term::ClipboardType::Clipboard => ClipboardKind::Clipboard,
                    alacritty_terminal::term::ClipboardType::Selection => ClipboardKind::Selection,
                };
                self.with_state(|st| st.clipboard.set(kind, text));
            }
            // OSC 52 reads stay disabled, so programs can't read the clipboard behind the user's back.
            Event::ClipboardLoad(..) => {}
            Event::ColorRequest(index, format) => {
                self.with_state(|st| {
                    let scheme = palette::resolve_scheme(&st.prefs.color_scheme, st.theme_dark);
                    if let Some(tab) = st.tabs.iter_mut().find(|t| t.session.id == id) {
                        let color =
                            ColorTable::new(scheme, tab.session.term.lock().colors()).get(index);
                        tab.session.write(format(color).into_bytes());
                    }
                });
            }
            Event::TextAreaSizeRequest(format) => {
                self.with_state(|st| {
                    let (w, h) = cell_size(st);
                    let size = st.grid.window_size(w, h);
                    if let Some(tab) = st.tabs.iter_mut().find(|t| t.session.id == id) {
                        tab.session.write(format(size).into_bytes());
                    }
                });
            }
            Event::PtyWrite(text) => {
                self.with_state(|st| {
                    if let Some(tab) = st.tabs.iter_mut().find(|t| t.session.id == id) {
                        tab.session.write(text.into_bytes());
                    }
                });
            }
            Event::CursorBlinkingChange => self.restart_blink(),
            Event::ChildExit(status) => self.child_exited(id, ExitReason::of(status)),
            Event::Exit => self.child_exited(id, ExitReason::Unknown),
            Event::MouseCursorDirty => {}
        }
    }

    /// Closes a tab whose program finished cleanly; keeps it with a note when it failed.
    fn child_exited(&self, id: SessionId, reason: ExitReason) {
        let close = self.with_state(|st| {
            let tab = st.tabs.iter_mut().find(|t| t.session.id == id)?;
            if tab.exited.is_some() {
                return Some(false);
            }
            tab.session.stopped();
            let Some(how) = reason.failure() else { return Some(true) };
            tab.exited = Some(reason.code());
            let note =
                format!("\r\n\x1b[0;2m[The program {how}. Press Enter to close the tab.]\x1b[0m");
            feed(&mut *tab.session.term.lock(), note.as_bytes());
            Some(false)
        });
        match close.flatten() {
            Some(true) => self.remove_tab(id),
            Some(false) => {
                self.sync_tabs();
                self.schedule_render();
            }
            None => {}
        }
    }

    fn bell(&self, id: SessionId, is_current: bool) {
        let flash = self.with_state(|st| {
            if let Some(tab) =
                st.tabs.iter_mut().find(|t| t.session.id == id).filter(|_| !is_current)
            {
                tab.bell = true;
            }
            st.prefs.visual_bell && is_current
        });
        self.sync_tabs();
        if flash == Some(true) {
            let Some(window) = self.window.upgrade() else { return };
            window.set_bell_flash(true);
            let weak = self.window.clone();
            self.bell_timer.start(TimerMode::SingleShot, BELL_FLASH, move || {
                if let Some(window) = weak.upgrade() {
                    window.set_bell_flash(false);
                }
            });
        }
    }

    /// Applies the desktop theme, which also picks the Nimbus terminal scheme's variant.
    pub fn set_theme(&self, settings: &nimbus_theme::ThemeSettings) {
        if let Some(window) = self.window.upgrade() {
            nimbus_theme::apply_theme!(window, settings, crate::ui::Theme);
            models::update_schemes(&window, settings.dark);
        }
        self.with_state(|st| st.theme_dark = settings.dark);
        self.schedule_render();
    }

    /// Uses a newly discovered font set.
    pub fn set_fonts(&self, fonts: Arc<FontSet>) {
        let families = fonts.monospace_families.clone();
        let family = fonts.family().to_string();
        self.with_state(|st| {
            st.fonts = Some(fonts);
            rebuild_glyphs(st);
        });
        if let Some(window) = self.window.upgrade() {
            let preferred = self.font_family();
            models::set_font_families(&window, &families, &family, &preferred);
        }
        self.update_geometry();
        self.render_now();
    }

    /// Changes the font size by `steps` zoom steps; zero resets the zoom.
    pub(super) fn zoom(&self, steps: i32) {
        self.with_state(|st| {
            st.zoom = if steps == 0 { 0.0 } else { st.zoom + steps as f32 * ZOOM_STEP };
            let size = prefs::clamp_font_size(st.prefs.font_size + st.zoom);
            st.zoom = size - st.prefs.font_size;
            rebuild_glyphs(st);
        });
        self.update_geometry();
        self.render_now();
    }

    pub(super) fn update_prefs(&self, change: impl FnOnce(&mut Prefs)) {
        let fonts_changed = self.with_state(|st| {
            let before = st.prefs.clone();
            change(&mut st.prefs);
            st.prefs = st.prefs.clone().sanitized();
            st.prefs_writer.save(&st.prefs);
            let config = term_config(&st.prefs);
            if term_config(&before) != config {
                for tab in &st.tabs {
                    tab.session.term.lock().set_options(config.clone());
                }
            }
            if before.font_size != st.prefs.font_size {
                rebuild_glyphs(st);
            }
            (before.font_family != st.prefs.font_family).then(|| st.prefs.font_family.clone())
        });
        if let Some(Some(family)) = fonts_changed {
            workers::load_fonts(self.events.clone(), family);
        }
        if let Some(window) = self.window.upgrade() {
            let st = self.state.borrow();
            models::sync_preferences(&window, &st.prefs);
        }
        self.restart_blink();
        self.update_geometry();
        self.schedule_render();
    }

    /// Recomputes the grid size from the view size, the scale factor, and the cell size; resizes every PTY.
    pub fn update_geometry(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let scale = window.window().scale_factor();
        let (width, height) = (window.get_grid_width(), window.get_grid_height());
        let rebuild =
            self.with_state(|st| (st.scale - scale).abs() > f32::EPSILON).unwrap_or(false);
        if rebuild {
            self.with_state(|st| {
                st.scale = scale;
                rebuild_glyphs(st);
            });
        }
        let resized = self.with_state(|st| {
            let Some(renderer) = st.renderer.as_ref() else { return false };
            let m = renderer.metrics();
            let grid = GridSize {
                columns: ((width * scale) / m.width as f32).floor().max(2.0) as usize,
                lines: ((height * scale) / m.height as f32).floor().max(1.0) as usize,
            };
            let size = grid.window_size(m.width, m.height);
            st.grid = grid;
            for tab in &mut st.tabs {
                tab.session.resize(size);
            }
            true
        });
        if resized == Some(true) {
            self.schedule_render();
        }
    }

    /// Schedules a frame soon, coalescing bursts of output.
    pub fn schedule_render(&self) {
        let delay = self.with_state(|st| {
            if st.render_scheduled {
                return None;
            }
            st.render_scheduled = true;
            Some(FRAME_INTERVAL.saturating_sub(st.last_render.elapsed()))
        });
        let Some(Some(delay)) = delay else { return };
        let this = self.me.clone();
        self.render_timer.start(TimerMode::SingleShot, delay, move || {
            if let Some(c) = this.upgrade() {
                c.render_now();
            }
        });
    }

    /// Draws the current tab now.
    pub fn render_now(&self) {
        let Some(window) = self.window.upgrade() else { return };
        let output = self.with_state(|st| {
            st.render_scheduled = false;
            st.last_render = Instant::now();
            let scheme = palette::resolve_scheme(&st.prefs.color_scheme, st.theme_dark);
            let blink_on = st.blink_on;
            let focused = st.focused;
            let bold_is_bright = st.prefs.bold_is_bright;
            let scale = st.scale;
            let current = st.current;
            let renderer = st.renderer.as_mut()?;
            let tab = st.tabs.get_mut(current)?;
            let (frame, mode, scroll) = {
                let mut term = tab.session.term.lock();
                let (matches, focused_match) = if tab.search.is_active() {
                    (tab.search.visible_matches(&term), tab.search.current().cloned())
                } else {
                    (Vec::new(), None)
                };
                let view = ViewOptions {
                    scheme,
                    bold_is_bright,
                    focused,
                    blink_on,
                    matches: &matches,
                    focused_match: focused_match.as_ref(),
                };
                let frame = renderer.snapshot(&mut term, &view);
                let total = term.total_lines().max(1) as f32;
                let history = term.history_size() as f32;
                let offset = term.grid().display_offset() as f32;
                let scroll = ((history - offset) / total, term.screen_lines() as f32 / total);
                (frame, *term.mode(), scroll)
            };
            let background = frame.background();
            let changed = renderer.draw(frame);
            let image = renderer.image();
            let size = (image.width() as f32 / scale, image.height() as f32 / scale);
            Some((changed.then_some(image), size, background, mode, scroll))
        });
        let Some(Some((image, size, background, mode, scroll))) = output else { return };
        if let Some(image) = image {
            window.set_screen(slint::Image::from_rgb8(image));
        }
        window.set_screen_width(size.0);
        window.set_screen_height(size.1);
        window.set_term_background(slint::Color::from_rgb_u8(
            background.r,
            background.g,
            background.b,
        ));
        window.set_pointer_text(!crate::mouse::reporting(mode));
        window.set_scroll_position(scroll.0);
        window.set_scroll_size(scroll.1);
    }

    pub(super) fn restart_blink(&self) {
        let blinking = self.with_state(|st| {
            st.blink_on = true;
            st.last_input = Instant::now();
            st.prefs.cursor_blink
                || st
                    .tabs
                    .get(st.current)
                    .is_some_and(|t| t.session.term.lock().cursor_style().blinking)
        });
        if blinking == Some(true) {
            let this = self.me.clone();
            self.blink_timer.start(TimerMode::Repeated, BLINK_INTERVAL, move || {
                if let Some(c) = this.upgrade() {
                    c.blink();
                }
            });
        } else {
            self.blink_timer.stop();
        }
        self.schedule_render();
    }

    fn blink(&self) {
        let redraw = self.with_state(|st| {
            let idle = st.last_input.elapsed() > BLINK_TIMEOUT;
            let next = if idle || !st.focused { true } else { !st.blink_on };
            let changed = next != st.blink_on;
            st.blink_on = next;
            (changed, idle)
        });
        if let Some((changed, idle)) = redraw {
            if changed {
                self.render_now();
            }
            if idle {
                self.blink_timer.stop();
            }
        }
    }

    fn focus_changed(&self, focused: bool) {
        self.with_state(|st| {
            st.focused = focused;
            if let Some(tab) = st.tabs.get_mut(st.current) {
                let report = {
                    let mut term = tab.session.term.lock();
                    term.is_focused = focused;
                    term.mode().contains(TermMode::FOCUS_IN_OUT)
                };
                if report {
                    tab.session.write(if focused { &b"\x1b[I"[..] } else { &b"\x1b[O"[..] });
                }
            }
        });
        self.restart_blink();
        self.render_now();
    }

    /// Writes pasted text to the current tab, bracketed when the program asked for it.
    pub(super) fn paste_text(&self, text: &str) {
        self.with_state(|st| {
            if let Some(tab) = st.tabs.get_mut(st.current).filter(|t| t.exited.is_none()) {
                let bytes = {
                    let mut term = tab.session.term.lock();
                    term.scroll_display(Scroll::Bottom);
                    crate::keys::paste(text, *term.mode())
                };
                tab.session.write(bytes);
            }
        });
        self.schedule_render();
    }

    /// Runs an app command.
    pub fn action(self: &Rc<Self>, action: Action) {
        let window = self.window.upgrade();
        match action {
            Action::Copy => {
                self.with_state(|st| {
                    if let Some(text) = st
                        .tabs
                        .get(st.current)
                        .and_then(|t| selection::text(&*t.session.term.lock()))
                    {
                        st.clipboard.set(ClipboardKind::Clipboard, text);
                    }
                });
            }
            Action::Paste => self.request_paste(ClipboardKind::Clipboard),
            Action::SelectAll => {
                self.with_state(|st| {
                    if let Some(tab) = st.tabs.get(st.current) {
                        selection::select_all(&mut *tab.session.term.lock());
                    }
                });
                self.schedule_render();
            }
            Action::NewTab | Action::NewWindow => {
                let handle = self.with_state(|st| {
                    st.tabs.get(st.current).and_then(|t| t.session.process_handle())
                });
                workers::find_directory(
                    self.events.clone(),
                    handle.flatten(),
                    action == Action::NewWindow,
                );
            }
            Action::CloseTab => {
                let current = self.state.borrow().current;
                self.close_tab(current);
            }
            Action::CloseWindow => {
                if self.close_requested() == slint::CloseRequestResponse::HideWindow {
                    self.quit();
                }
            }
            Action::NextTab | Action::PreviousTab => {
                let (current, count) = {
                    let st = self.state.borrow();
                    (st.current, st.tabs.len().max(1))
                };
                let next = if action == Action::NextTab {
                    (current + 1) % count
                } else {
                    (current + count - 1) % count
                };
                self.select_tab(next);
            }
            Action::MoveTabLeft | Action::MoveTabRight => {
                self.with_state(|st| {
                    let target = if action == Action::MoveTabLeft {
                        st.current.checked_sub(1)
                    } else {
                        Some(st.current + 1).filter(|t| *t < st.tabs.len())
                    };
                    if let Some(target) = target {
                        st.tabs.swap(st.current, target);
                        st.current = target;
                    }
                });
                self.sync_tabs();
            }
            Action::GoToTab(index) => {
                let count = self.tab_count();
                self.select_tab(if index == usize::MAX { count.saturating_sub(1) } else { index });
            }
            Action::Find => {
                if let Some(window) = &window {
                    window.invoke_open_search();
                }
                self.sync_search_bar();
            }
            Action::FindNext => self.search(Some(SearchDirection::Older)),
            Action::FindPrevious => self.search(Some(SearchDirection::Newer)),
            Action::ZoomIn => self.zoom(1),
            Action::ZoomOut => self.zoom(-1),
            Action::ZoomReset => self.zoom(0),
            Action::ScrollLineUp => self.scroll(Scroll::Delta(1)),
            Action::ScrollLineDown => self.scroll(Scroll::Delta(-1)),
            Action::ScrollPageUp => self.scroll(Scroll::PageUp),
            Action::ScrollPageDown => self.scroll(Scroll::PageDown),
            Action::ScrollTop => self.scroll(Scroll::Top),
            Action::ScrollBottom => self.scroll(Scroll::Bottom),
            Action::Preferences => {
                if let Some(window) = &window {
                    window.set_preferences_open(!window.get_preferences_open());
                }
            }
            Action::ToggleFullscreen => {
                if let Some(window) = &window {
                    let fullscreen = window.window().is_fullscreen();
                    window.window().set_fullscreen(!fullscreen);
                }
            }
        }
    }

    pub(super) fn request_paste(&self, kind: ClipboardKind) {
        let events = self.events.clone();
        self.with_state(|st| {
            st.clipboard.get(kind, move |text| workers::send(&events, AppEvent::Paste(text)));
        });
    }

    pub(super) fn scroll(&self, scroll: Scroll) {
        self.with_state(|st| {
            if let Some(tab) = st.tabs.get(st.current) {
                tab.session.term.lock().scroll_display(scroll);
            }
        });
        self.schedule_render();
    }

    fn search_edited(&self, text: &str) {
        let valid = self.with_state(|st| {
            let current = st.current;
            st.tabs.get_mut(current).map(|t| t.search.set_pattern(text))
        });
        if valid.flatten() == Some(false) {
            if let Some(window) = self.window.upgrade() {
                window.set_search_status("Can't search for this".into());
            }
            return;
        }
        self.search(if text.is_empty() { None } else { Some(SearchDirection::Older) });
    }

    /// Moves to the next match in `direction`, or just refreshes the status.
    fn search(&self, direction: Option<SearchDirection>) {
        let status = self.with_state(|st| {
            let current = st.current;
            let tab = st.tabs.get_mut(current)?;
            if !tab.search.is_active() {
                return Some(String::new());
            }
            let found = match direction {
                Some(direction) => {
                    tab.search.advance(&mut *tab.session.term.lock(), direction).is_some()
                }
                None => tab.search.current().is_some(),
            };
            Some(if found { String::new() } else { "No matches".into() })
        });
        if let (Some(Some(status)), Some(window)) = (status, self.window.upgrade()) {
            window.set_search_status(status.into());
        }
        self.schedule_render();
    }

    fn close_search(&self) {
        self.with_state(|st| {
            let current = st.current;
            if let Some(tab) = st.tabs.get_mut(current) {
                tab.search.clear();
            }
        });
        if let Some(window) = self.window.upgrade() {
            window.set_search_open(false);
            window.set_search_text(SharedString::new());
            window.set_search_status(SharedString::new());
            window.invoke_focus_terminal();
        }
        self.schedule_render();
    }
}

/// The cell size in physical pixels, or a typical one before fonts load.
pub(super) fn cell_size(st: &State) -> (u32, u32) {
    st.renderer.as_ref().map_or((8, 16), |r| (r.metrics().width, r.metrics().height))
}

/// Recreates the glyph cache for the current font, size, zoom, and scale.
fn rebuild_glyphs(st: &mut State) {
    let Some(fonts) = st.fonts.clone() else { return };
    let points = prefs::clamp_font_size(st.prefs.font_size + st.zoom);
    let pixels = points * 96.0 / 72.0 * st.scale;
    let glyphs = GlyphCache::new(fonts, pixels);
    match st.renderer.as_mut() {
        Some(renderer) => renderer.set_glyphs(glyphs),
        None => st.renderer = Some(Renderer::new(glyphs)),
    }
}

#[cfg(test)]
mod tests {
    use super::ExitReason;

    #[cfg(unix)]
    #[test]
    fn signal_deaths_are_failures() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;

        let killed = ExitReason::of(ExitStatus::from_raw(9));
        assert_eq!(killed, ExitReason::Signal(9));
        assert_eq!(killed.failure().as_deref(), Some("was terminated by signal 9"));
        assert_eq!(killed.code(), 137);

        let failed = ExitReason::of(ExitStatus::from_raw(3 << 8));
        assert_eq!(failed.failure().as_deref(), Some("exited with code 3"));
        assert_eq!(ExitReason::of(ExitStatus::from_raw(0)).failure(), None);
        assert_eq!(ExitReason::Unknown.failure(), None);
    }
}

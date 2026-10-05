// SPDX-License-Identifier: MIT

//! Connects samples to the window and the window's requests to the sampler.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use futures_util::StreamExt;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::core::format;
use crate::core::graph;
use crate::core::history::{HISTORY_LEN, History};
use crate::core::prefs::{Page, Prefs};
use crate::core::processes::{self, Filter, Row, SortColumn, SortOrder};
use crate::core::snapshot::{ProcessKey, ProcessKind, ProcessState, Snapshot};
use crate::sampler::{ProcessSignal, Sampler, SamplerEvent, SignalError, SystemSource};
use crate::{
    AppWindow, CoreItem, FileSystemRow, Graph, PendingSignal, ProcessIcon, ProcessRow, Theme,
};

/// Update intervals offered in the window, in milliseconds.
pub const INTERVALS_MS: [u64; 5] = [500, 1000, 2000, 5000, 10_000];

/// Smallest top of the network and disk graphs, so idle noise doesn't fill them.
const RATE_GRAPH_FLOOR: f32 = 64.0 * 1024.0;

/// How long an error about signalling a process stays visible.
const STATUS_TIMEOUT: Duration = Duration::from_secs(6);

/// What the window asks of the sampler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    Signal(ProcessKey, ProcessSignal),
    SetInterval(Duration),
}

struct State {
    snapshot: Snapshot,
    history: History,
    rows: Vec<Row>,
    filter: Filter,
    order: SortOrder,
    prefs: Prefs,
    selected: Option<ProcessKey>,
    /// The process and signal awaiting confirmation.
    pending: Option<(ProcessKey, ProcessSignal)>,
    current_user: Option<String>,
}

pub struct Controller {
    ui: slint::Weak<AppWindow>,
    state: RefCell<State>,
    processes: Rc<VecModel<ProcessRow>>,
    cores: Rc<VecModel<CoreItem>>,
    file_systems: Rc<VecModel<FileSystemRow>>,
    requests: Box<dyn Fn(Request)>,
    prefs_path: Option<PathBuf>,
    status_timer: slint::Timer,
}

fn interval_index(ms: u64) -> usize {
    INTERVALS_MS
        .iter()
        .enumerate()
        .min_by_key(|(_, choice)| choice.abs_diff(ms))
        .map(|(i, _)| i)
        .unwrap_or(2)
}

impl Controller {
    /// Wires up `ui`; `requests` receives the signals and interval changes the user asks for.
    pub fn new(
        ui: &AppWindow,
        prefs: Prefs,
        prefs_path: Option<PathBuf>,
        current_user: Option<String>,
        requests: impl Fn(Request) + 'static,
    ) -> Rc<Self> {
        let order = SortOrder { column: prefs.sort, descending: prefs.descending };
        let filter =
            Filter { user: current_user.clone().filter(|_| !prefs.all_users), ..Filter::default() };
        let controller = Rc::new(Self {
            ui: ui.as_weak(),
            state: RefCell::new(State {
                snapshot: Snapshot::default(),
                history: History::default(),
                rows: Vec::new(),
                filter,
                order,
                prefs: prefs.clone(),
                selected: None,
                pending: None,
                current_user,
            }),
            processes: Rc::new(VecModel::default()),
            cores: Rc::new(VecModel::default()),
            file_systems: Rc::new(VecModel::default()),
            requests: Box::new(requests),
            prefs_path,
            status_timer: slint::Timer::default(),
        });

        ui.set_processes(ModelRc::from(controller.processes.clone()));
        ui.set_cores(ModelRc::from(controller.cores.clone()));
        ui.set_file_systems(ModelRc::from(controller.file_systems.clone()));
        ui.set_page(prefs.page.index() as i32);
        ui.set_sort_column(order.column.index() as i32);
        ui.set_sort_descending(order.descending);
        ui.set_tree(prefs.tree);
        ui.set_scope(i32::from(prefs.all_users));
        ui.set_interval_index(interval_index(prefs.interval_ms) as i32);
        controller.connect(ui);
        controller
    }

    fn connect(self: &Rc<Self>, ui: &AppWindow) {
        let weak = Rc::downgrade(self);
        let with = move |f: &dyn Fn(&Controller)| {
            if let Some(controller) = weak.upgrade() {
                f(&controller);
            }
        };
        let w = with.clone();
        ui.on_page_selected(move |page| {
            w(&|c| c.update_prefs(|p| p.page = Page::from_index(page as usize).unwrap_or_default()))
        });
        let w = with.clone();
        ui.on_query_edited(move |text| {
            w(&|c| {
                c.state.borrow_mut().filter.query = text.to_string();
                c.refresh_processes();
            })
        });
        let w = with.clone();
        ui.on_tree_toggled(move |tree| {
            w(&|c| {
                c.update_prefs(|p| p.tree = tree);
                c.refresh_processes();
            })
        });
        let w = with.clone();
        ui.on_scope_selected(move |scope| w(&|c| c.set_all_users(scope == 1)));
        let w = with.clone();
        ui.on_interval_selected(move |index| {
            w(&|c| {
                let ms = INTERVALS_MS.get(index as usize).copied().unwrap_or(2000);
                c.update_prefs(|p| p.interval_ms = ms);
                (c.requests)(Request::SetInterval(Duration::from_millis(ms)));
            })
        });
        let w = with.clone();
        ui.on_sort_requested(move |column| {
            w(&|c| {
                if let Some(column) = SortColumn::from_index(column as usize) {
                    c.sort_by(column);
                }
            })
        });
        let w = with.clone();
        ui.on_select(move |pid| w(&|c| c.select_pid(pid)));
        let w = with.clone();
        ui.on_move_selection(move |delta| w(&|c| c.move_selection(delta)));
        let w = with.clone();
        ui.on_request_signal(move |signal| {
            w(&|c| match signal {
                PendingSignal::End => c.ask(ProcessSignal::End),
                PendingSignal::Kill => c.ask(ProcessSignal::Kill),
                PendingSignal::None => {}
            })
        });
        let w = with.clone();
        ui.on_stop_selected(move || w(&|c| c.signal_selected(ProcessSignal::Stop)));
        let w = with.clone();
        ui.on_continue_selected(move || w(&|c| c.signal_selected(ProcessSignal::Continue)));
        let w = with.clone();
        ui.on_confirm_signal(move || w(&|c| c.confirm()));
        let ui_weak = ui.as_weak();
        ui.on_quit(move || {
            if let Some(ui) = ui_weak.upgrade()
                && let Err(err) = ui.hide()
            {
                tracing::warn!("cannot close the window: {err}");
            }
        });
    }

    /// The sampling interval from the saved preferences.
    pub fn interval(&self) -> Duration {
        self.state.borrow().prefs.interval()
    }

    pub fn handle(&self, event: SamplerEvent) {
        match event {
            SamplerEvent::Snapshot(snapshot) => self.apply_snapshot(*snapshot),
            SamplerEvent::Signalled { result: Ok(()), .. } => self.set_status(""),
            SamplerEvent::Signalled { result: Err(err), .. } => self.set_status(&err.to_string()),
        }
    }

    fn set_status(&self, text: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_process_status(text.into());
        if text.is_empty() {
            self.status_timer.stop();
        } else {
            let weak = self.ui.clone();
            self.status_timer.start(slint::TimerMode::SingleShot, STATUS_TIMEOUT, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_process_status(SharedString::new());
                }
            });
        }
    }

    fn update_prefs(&self, change: impl FnOnce(&mut Prefs)) {
        let prefs = {
            let mut state = self.state.borrow_mut();
            change(&mut state.prefs);
            state.prefs.clone()
        };
        if let Some(path) = &self.prefs_path
            && let Err(err) = prefs.save(path)
        {
            tracing::warn!("cannot save {}: {err}", path.display());
        }
    }

    fn set_all_users(&self, all: bool) {
        self.update_prefs(|p| p.all_users = all);
        {
            let mut state = self.state.borrow_mut();
            state.filter.user = state.current_user.clone().filter(|_| !all);
        }
        self.refresh_processes();
    }

    fn sort_by(&self, column: SortColumn) {
        let order = self.state.borrow().order.toggled(column);
        self.state.borrow_mut().order = order;
        self.update_prefs(|p| {
            p.sort = order.column;
            p.descending = order.descending;
        });
        if let Some(ui) = self.ui.upgrade() {
            ui.set_sort_column(order.column.index() as i32);
            ui.set_sort_descending(order.descending);
        }
        self.refresh_processes();
    }

    fn select_pid(&self, pid: i32) {
        let key = {
            let state = self.state.borrow();
            u32::try_from(pid)
                .ok()
                .and_then(|pid| state.snapshot.processes.iter().find(|p| p.key.pid == pid))
                .map(|p| p.key)
        };
        self.state.borrow_mut().selected = key;
        self.show_selection();
    }

    fn move_selection(&self, delta: i32) {
        let key = {
            let state = self.state.borrow();
            if state.rows.is_empty() {
                return;
            }
            let current = state.selected.and_then(|key| {
                state.rows.iter().position(|r| state.snapshot.processes[r.index].key == key)
            });
            let last = state.rows.len() as i64 - 1;
            let next = match current {
                Some(i) => (i as i64 + i64::from(delta)).clamp(0, last),
                None if delta < 0 => last,
                None => 0,
            };
            state.snapshot.processes[state.rows[next as usize].index].key
        };
        self.state.borrow_mut().selected = Some(key);
        self.show_selection();
    }

    fn show_selection(&self) {
        let pid = self.state.borrow().selected.map_or(-1, |k| i32::try_from(k.pid).unwrap_or(-1));
        if let Some(ui) = self.ui.upgrade() {
            ui.set_selected_pid(pid);
        }
    }

    fn selected_name(&self) -> Option<(ProcessKey, String)> {
        let state = self.state.borrow();
        let key = state.selected?;
        state.snapshot.processes.iter().find(|p| p.key == key).map(|p| (key, p.name.clone()))
    }

    fn ask(&self, signal: ProcessSignal) {
        let Some((key, name)) = self.selected_name() else { return };
        self.state.borrow_mut().pending = Some((key, signal));
        if let Some(ui) = self.ui.upgrade() {
            ui.set_pending_name(name.into());
            ui.set_pending(if signal == ProcessSignal::Kill {
                PendingSignal::Kill
            } else {
                PendingSignal::End
            });
        }
    }

    fn confirm(&self) {
        let pending = self.state.borrow_mut().pending.take();
        if let Some(ui) = self.ui.upgrade() {
            ui.set_pending(PendingSignal::None);
        }
        if let Some((key, signal)) = pending {
            self.send_signal(key, signal);
        }
    }

    fn signal_selected(&self, signal: ProcessSignal) {
        if let Some(key) = self.state.borrow().selected {
            self.send_signal(key, signal);
        }
    }

    fn send_signal(&self, key: ProcessKey, signal: ProcessSignal) {
        let running = self.state.borrow().snapshot.processes.iter().any(|p| p.key == key);
        if running {
            (self.requests)(Request::Signal(key, signal));
        } else {
            self.set_status(&SignalError::Gone.to_string());
        }
    }

    fn apply_snapshot(&self, snapshot: Snapshot) {
        let mut pending_gone = false;
        {
            let mut state = self.state.borrow_mut();
            state.history.push(&snapshot);
            let gone = |key: &ProcessKey| !snapshot.processes.iter().any(|p| p.key == *key);
            if state.selected.as_ref().is_some_and(gone) {
                state.selected = None;
            }
            if state.pending.as_ref().is_some_and(|(key, _)| gone(key)) {
                state.pending = None;
                pending_gone = true;
            }
            state.snapshot = snapshot;
        }
        if pending_gone {
            if let Some(ui) = self.ui.upgrade() {
                ui.set_pending(PendingSignal::None);
            }
            self.set_status(&SignalError::Gone.to_string());
        }
        self.show_selection();
        self.refresh_processes();
        self.refresh_resources();
        self.refresh_file_systems();
    }

    fn refresh_processes(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        state.rows = processes::arrange(
            &state.snapshot.processes,
            &state.filter,
            state.order,
            state.prefs.tree,
        );
        let rows: Vec<ProcessRow> = state
            .rows
            .iter()
            .map(|row| {
                let p = &state.snapshot.processes[row.index];
                let disk = p.disk_read_rate + p.disk_write_rate;
                ProcessRow {
                    pid: i32::try_from(p.key.pid).unwrap_or(i32::MAX),
                    name: p.name.as_str().into(),
                    command: p.command.as_str().into(),
                    user: p.user.as_str().into(),
                    cpu: format::percent(p.cpu).into(),
                    cpu_fraction: (p.cpu / 100.0).clamp(0.0, 1.0),
                    memory: if p.memory == 0 {
                        "–".into()
                    } else {
                        format::bytes(p.memory).into()
                    },
                    disk: if disk < 1.0 { "–".into() } else { format::rate(disk).into() },
                    state: p.state.label().into(),
                    depth: i32::try_from(row.depth).unwrap_or(0),
                    icon: match p.kind {
                        ProcessKind::Application => ProcessIcon::Application,
                        ProcessKind::Kernel => ProcessIcon::Kernel,
                        ProcessKind::Terminal => ProcessIcon::Terminal,
                        ProcessKind::Service => ProcessIcon::Service,
                    },
                }
            })
            .collect();
        sync(&self.processes, rows);

        let snapshot = &state.snapshot;
        let running =
            snapshot.processes.iter().filter(|p| p.state == ProcessState::Running).count();
        ui.set_process_summary(
            format!(
                "{} processes, {} running · CPU {} · Memory {} of {}",
                snapshot.processes.len(),
                running,
                format::percent(snapshot.cpu.total),
                format::bytes(snapshot.memory.used),
                format::bytes(snapshot.memory.total),
            )
            .into(),
        );
    }

    fn refresh_resources(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let state = self.state.borrow();
        let (snapshot, history) = (&state.snapshot, &state.history);

        ui.set_host_name(snapshot.host.host_name.as_str().into());
        ui.set_os_name(snapshot.host.os.as_str().into());
        ui.set_uptime(format::uptime(snapshot.uptime).into());
        ui.set_load(
            format!(
                "{:.2}  {:.2}  {:.2}",
                snapshot.load.one, snapshot.load.five, snapshot.load.fifteen
            )
            .into(),
        );

        let cpu = history.cpu.to_vec();
        ui.set_cpu_graph(Graph {
            area: graph::area(&cpu, HISTORY_LEN, 100.0).into(),
            line: graph::line(&cpu, HISTORY_LEN, 100.0).into(),
            value: format::percent(snapshot.cpu.total).into(),
            ..Graph::default()
        });
        let mut detail = snapshot.cpu.brand.clone();
        if snapshot.cpu.frequency_mhz > 0 {
            detail.push_str(&format!(" · {:.1} GHz", snapshot.cpu.frequency_mhz as f64 / 1000.0));
        }
        if let Some(t) = snapshot.temperatures.iter().max_by(|a, b| a.celsius.total_cmp(&b.celsius))
        {
            detail.push_str(&format!(" · {:.0} °C", t.celsius));
        }
        ui.set_cpu_detail(detail.into());
        sync(
            &self.cores,
            snapshot
                .cpu
                .cores
                .iter()
                .enumerate()
                .map(|(i, usage)| CoreItem {
                    label: format!("CPU {}", i + 1).into(),
                    usage: (usage / 100.0).clamp(0.0, 1.0),
                    text: format::percent(*usage).into(),
                })
                .collect(),
        );

        let memory = history.memory.to_vec();
        let m = &snapshot.memory;
        ui.set_memory_graph(Graph {
            area: graph::area(&memory, HISTORY_LEN, 100.0).into(),
            line: graph::line(&memory, HISTORY_LEN, 100.0).into(),
            secondary_line: graph::line(&history.swap.to_vec(), HISTORY_LEN, 100.0).into(),
            value: format!(
                "{} of {} ({})",
                format::bytes(m.used),
                format::bytes(m.total),
                format::percent(m.used_fraction() * 100.0)
            )
            .into(),
            secondary_value: if m.swap_total == 0 {
                "not available".into()
            } else {
                format!("{} of {}", format::bytes(m.swap_used), format::bytes(m.swap_total)).into()
            },
            scale: SharedString::new(),
        });
        ui.set_memory_detail(format!("{} available", format::bytes(m.available)).into());

        let rate_graph = |a: &crate::core::history::Series,
                          b: &crate::core::history::Series,
                          va: f64,
                          vb: f64| {
            let max = graph::nice_max(a.max().max(b.max()), RATE_GRAPH_FLOOR);
            let (a, b) = (a.to_vec(), b.to_vec());
            Graph {
                area: graph::area(&a, HISTORY_LEN, max).into(),
                line: graph::line(&a, HISTORY_LEN, max).into(),
                secondary_line: graph::line(&b, HISTORY_LEN, max).into(),
                value: format::rate(va).into(),
                secondary_value: format::rate(vb).into(),
                scale: format::rate(f64::from(max)).into(),
            }
        };
        ui.set_network_graph(rate_graph(
            &history.receive,
            &history.transmit,
            snapshot.total_receive_rate(),
            snapshot.total_transmit_rate(),
        ));
        let received: u64 = snapshot.interfaces.iter().map(|i| i.received_total).sum();
        let sent: u64 = snapshot.interfaces.iter().map(|i| i.transmitted_total).sum();
        ui.set_network_detail(
            format!("{} in, {} out", format::bytes(received), format::bytes(sent)).into(),
        );
        ui.set_disk_graph(rate_graph(
            &history.disk_read,
            &history.disk_write,
            snapshot.total_disk_read_rate(),
            snapshot.total_disk_write_rate(),
        ));
        let devices = snapshot.disks.len();
        ui.set_disk_detail(match devices {
            0 => "No disks".into(),
            1 => snapshot.disks[0].device.as_str().into(),
            n => format!("{n} devices").into(),
        });
    }

    fn refresh_file_systems(&self) {
        let state = self.state.borrow();
        sync(
            &self.file_systems,
            state
                .snapshot
                .file_systems
                .iter()
                .map(|fs| FileSystemRow {
                    device: fs.device.as_str().into(),
                    mount_point: fs.mount_point.as_str().into(),
                    fs_type: fs.fs_type.as_str().into(),
                    used: format::bytes(fs.used()).into(),
                    total: format::bytes(fs.total).into(),
                    fraction: fs.used_fraction(),
                    removable: fs.removable,
                    read_only: fs.read_only,
                })
                .collect(),
        );
    }
}

/// Updates `model` to `rows`, touching only rows that changed.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let common = model.row_count().min(rows.len());
    for (i, row) in rows.iter().enumerate().take(common) {
        if model.row_data(i).as_ref() != Some(row) {
            model.set_row_data(i, row.clone());
        }
    }
    if model.row_count() > rows.len() {
        for i in (rows.len()..model.row_count()).rev() {
            model.remove(i);
        }
    } else {
        model.extend(rows.into_iter().skip(common));
    }
}

/// Runs the monitor until its window closes.
pub fn run(page: Option<Page>) -> anyhow::Result<()> {
    let config = nimbus_config::Config::load().unwrap_or_else(|err| {
        tracing::warn!("using the default configuration: {err}");
        nimbus_config::Config::default()
    });
    let ui = AppWindow::new()?;
    if let Err(err) = slint::set_xdg_app_id(crate::APP_ID) {
        tracing::warn!("cannot set the application id: {err}");
    }
    nimbus_theme::apply_theme!(ui, nimbus_theme::ThemeSettings::from_config(&config.appearance));
    let _config_watch = watch_config(&ui);

    let prefs_path = Prefs::default_path();
    let mut prefs = prefs_path.as_deref().map(Prefs::load).unwrap_or_default();
    if let Some(page) = page {
        prefs.page = page;
    }
    let interval = prefs.interval();
    let sampler: Rc<RefCell<Option<Sampler>>> = Rc::default();
    let requests = sampler.clone();
    let controller =
        Controller::new(&ui, prefs, prefs_path, crate::sampler::current_user(), move |request| {
            if let Some(sampler) = requests.borrow().as_ref() {
                match request {
                    Request::Signal(key, signal) => sampler.signal(key, signal),
                    Request::SetInterval(interval) => sampler.set_interval(interval),
                }
            }
        });

    let (sender, mut receiver) = futures_channel::mpsc::unbounded();
    *sampler.borrow_mut() = Some(Sampler::spawn(
        || Box::new(SystemSource::new()),
        interval,
        move |event| {
            // The receiver only goes away when the window has closed.
            let _ = sender.unbounded_send(event);
        },
    )?);
    let handler = controller.clone();
    slint::spawn_local(async move {
        while let Some(event) = receiver.next().await {
            handler.handle(event);
        }
    })
    .map_err(|err| anyhow::anyhow!("cannot receive samples: {err}"))?;

    ui.run()?;
    // Stop sampling before the window goes away.
    sampler.borrow_mut().take();
    Ok(())
}

/// Re-applies the theme when the Nimbus configuration changes.
fn watch_config(ui: &AppWindow) -> Option<nimbus_config::ConfigWatcher> {
    let path = nimbus_config::default_path().ok()?;
    let weak = ui.as_weak();
    nimbus_config::watch(&path, move |config| {
        // Resolving the system scheme may query the portal, so it happens on the watcher thread.
        let settings = nimbus_theme::ThemeSettings::from_config(&config.appearance);
        let weak = weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                nimbus_theme::apply_theme!(ui, settings);
            }
        });
    })
    .map_err(|err| tracing::warn!("not watching the configuration: {err}"))
    .ok()
}

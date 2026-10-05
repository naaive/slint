// SPDX-License-Identifier: MIT

//! Drives the window on Slint's testing backend with sample data.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use nimbus_monitor::app::{Controller, Request};
use nimbus_monitor::core::prefs::{Page, Prefs};
use nimbus_monitor::core::rates::RateTracker;
use nimbus_monitor::sampler::{ProcessSignal, SampleSource, SamplerEvent, SignalError, Source};
use nimbus_monitor::{AppWindow, PendingSignal};
use slint::Model;

struct Fixture {
    ui: AppWindow,
    controller: Rc<Controller>,
    requests: Rc<RefCell<Vec<Request>>>,
    source: SampleSource,
    rates: RateTracker,
    prefs_dir: tempfile::TempDir,
}

impl Fixture {
    fn new(prefs: Prefs) -> Self {
        i_slint_backend_testing::init_no_event_loop();
        let ui = AppWindow::new().unwrap();
        let requests = Rc::new(RefCell::new(Vec::new()));
        let sink = requests.clone();
        let prefs_dir = tempfile::tempdir().unwrap();
        let controller = Controller::new(
            &ui,
            prefs,
            Some(prefs_dir.path().join("monitor.toml")),
            Some("ada".into()),
            move |r| sink.borrow_mut().push(r),
        );
        let mut fixture = Self {
            ui,
            controller,
            requests,
            source: SampleSource::new(),
            rates: RateTracker::new(),
            prefs_dir,
        };
        fixture.tick();
        fixture.tick();
        fixture
    }

    fn tick(&mut self) {
        let snapshot = self.rates.update(self.source.sample());
        self.controller.handle(SamplerEvent::Snapshot(Box::new(snapshot)));
    }

    fn names(&self) -> Vec<String> {
        self.ui.get_processes().iter().map(|r| r.name.to_string()).collect()
    }

    fn saved(&self) -> Prefs {
        Prefs::load(&self.prefs_dir.path().join("monitor.toml"))
    }
}

#[test]
fn lists_own_processes_sorted_by_cpu() {
    let f = Fixture::new(Prefs::default());
    let names = f.names();
    assert_eq!(names.first().map(String::as_str), Some("rustc"));
    // Root's processes and kernel threads are hidden in "My Processes".
    assert!(
        !names.iter().any(|n| n == "sshd" || n == "NetworkManager" || n.starts_with("kworker"))
    );
    assert!(f.ui.get_process_summary().contains("14 processes"));
}

#[test]
fn scope_sort_tree_and_search() {
    let f = Fixture::new(Prefs::default());
    f.ui.invoke_scope_selected(1);
    assert!(f.names().iter().any(|n| n == "sshd"));
    assert!(f.saved().all_users);

    f.ui.invoke_sort_requested(0);
    assert_eq!(f.names().first().map(String::as_str), Some("bash"));
    assert_eq!(f.ui.get_sort_column(), 0);
    assert!(!f.ui.get_sort_descending());
    f.ui.invoke_sort_requested(0);
    assert!(f.ui.get_sort_descending());

    f.ui.set_tree(true);
    f.ui.invoke_tree_toggled(true);
    let rows: Vec<_> = f.ui.get_processes().iter().collect();
    let cargo = rows.iter().find(|r| r.name == "cargo").unwrap();
    let rustc = rows.iter().find(|r| r.name == "rustc").unwrap();
    assert_eq!(rustc.depth, cargo.depth + 1);
    assert!(f.saved().tree);

    f.ui.invoke_query_edited("FIRE".into());
    assert_eq!(f.names(), ["firefox", "Web Content"]);
    f.ui.invoke_query_edited("no such thing".into());
    assert!(f.names().is_empty());
}

#[test]
fn ending_a_process_asks_first() {
    let mut f = Fixture::new(Prefs::default());
    f.ui.invoke_select(2301);
    assert_eq!(f.ui.get_selected_pid(), 2301);
    f.ui.invoke_request_signal(PendingSignal::End);
    assert_eq!(f.ui.get_pending(), PendingSignal::End);
    assert_eq!(f.ui.get_pending_name(), "cargo");
    assert!(f.requests.borrow().is_empty());
    f.ui.invoke_confirm_signal();
    assert_eq!(f.ui.get_pending(), PendingSignal::None);
    let request = f.requests.borrow()[0];
    let Request::Signal(key, ProcessSignal::End) = request else {
        panic!("unexpected {request:?}")
    };
    assert_eq!(key.pid, 2301);

    // The sampler reports the outcome; the process disappears from the next sample.
    f.source.signal(key, ProcessSignal::End).unwrap();
    f.controller.handle(SamplerEvent::Signalled {
        key,
        signal: ProcessSignal::End,
        result: Ok(()),
    });
    f.tick();
    assert!(!f.names().iter().any(|n| n == "cargo"));
    assert_eq!(f.ui.get_selected_pid(), -1);

    f.controller.handle(SamplerEvent::Signalled {
        key,
        signal: ProcessSignal::Kill,
        result: Err(SignalError::PermissionDenied),
    });
    assert!(f.ui.get_process_status().contains("permission"));
    i_slint_backend_testing::mock_elapsed_time(Duration::from_secs(7));
    assert_eq!(f.ui.get_process_status(), "");
}

#[test]
fn keyboard_selection_and_stop() {
    let f = Fixture::new(Prefs::default());
    f.ui.invoke_move_selection(1);
    let first = f.ui.get_processes().row_data(0).unwrap().pid;
    assert_eq!(f.ui.get_selected_pid(), first);
    f.ui.invoke_move_selection(100);
    let rows = f.ui.get_processes();
    assert_eq!(f.ui.get_selected_pid(), rows.row_data(rows.row_count() - 1).unwrap().pid);
    f.ui.invoke_stop_selected();
    assert!(matches!(f.requests.borrow()[0], Request::Signal(_, ProcessSignal::Stop)));
}

#[test]
fn resources_and_interval() {
    let mut f = Fixture::new(Prefs { page: Page::Resources, ..Prefs::default() });
    for _ in 0..10 {
        f.tick();
    }
    assert_eq!(f.ui.get_page(), 1);
    let cpu = f.ui.get_cpu_graph();
    assert!(cpu.line.starts_with("M "));
    assert!(cpu.area.ends_with('Z'));
    assert_eq!(f.ui.get_cores().row_count(), 8);
    assert!(f.ui.get_memory_graph().value.contains("of 32 GiB"));
    assert!(f.ui.get_network_graph().scale.ends_with("/s"));
    assert_eq!(f.ui.get_file_systems().row_count(), 3);
    assert!(f.ui.get_cpu_detail().contains("GHz"));

    f.ui.invoke_interval_selected(4);
    assert_eq!(f.requests.borrow().last(), Some(&Request::SetInterval(Duration::from_secs(10))));
    assert_eq!(f.saved().interval_ms, 10_000);
    f.ui.invoke_page_selected(2);
    assert_eq!(f.saved().page, Page::FileSystems);
}

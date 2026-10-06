// SPDX-License-Identifier: MIT

//! The Date & Time page: the time zone and synchronization from timedated, and the clock's hours.

use std::rc::Rc;

use nimbus_services::timedate::{Command, Event, TimeState};
use slint::ComponentHandle;

use super::{Inner, Message, PickerKind, With, with_app};
use crate::date_time::{city, now_text, sync_status};
use crate::dispatch::Dispatch;
use crate::settings::{Key, Value};
use crate::sources::Control;
use crate::{AppWindow, Prefs, TimeModel, clock};

/// How often the shown time is updated.
const TICK: std::time::Duration = std::time::Duration::from_secs(10);

/// What the page knows about the date and time.
#[derive(Default)]
pub(crate) struct Time {
    control: Option<Rc<dyn Control<Command>>>,
    pub state: TimeState,
    /// The time zones to pick from, sorted.
    pub zones: Vec<String>,
    /// Whether timedated reported yet.
    reported: bool,
    /// Keeps the shown time current.
    ticker: Option<slint::Timer>,
}

pub(super) fn wire(ui: &AppWindow, with: &With) {
    let model = ui.global::<TimeModel>();
    let h = with.clone();
    model.on_set_sync(move |on| h(&|i| i.send_time(Command::SetNtp(on))));
    let h = with.clone();
    model.on_pick_timezone(move || h(&|i| i.open_picker(PickerKind::Timezone)));
    let h = with.clone();
    ui.global::<Prefs>().on_set_clock_24_hour(move |on| {
        h(&|i| {
            let format = clock::with_24_hour(&i.store.borrow().config().panel.clock_format, on);
            i.edit(Key::ClockFormat.name(), Value::Text(format));
        })
    });
}

impl Inner {
    /// Starts the timedated client.
    pub fn start_time(&self) {
        // Inline sources report right away, so no borrow may be held here.
        let control = self.sources.time(self.events(Message::Time));
        let ticker = (self.dispatch == Dispatch::Threaded).then(|| {
            let ticker = slint::Timer::default();
            ticker.start(slint::TimerMode::Repeated, TICK, || with_app(|app| app.show_time()));
            ticker
        });
        let time = &mut self.state.borrow_mut().time;
        time.control = Some(Rc::from(control));
        time.ticker = ticker;
    }

    pub(super) fn send_time(&self, command: Command) {
        // Inline sources report right away, so no borrow may be held while sending.
        let control = self.state.borrow().time.control.clone();
        if let Some(control) = control {
            control.send(command);
        }
    }

    pub(super) fn handle_time(&self, event: Event) {
        match event {
            Event::State(state) => {
                {
                    let time = &mut self.state.borrow_mut().time;
                    time.state = state;
                    time.reported = true;
                }
                self.show_time();
            }
            Event::Timezones(zones) => {
                self.state.borrow_mut().time.zones = zones;
                self.show_time();
                self.refresh_picker(PickerKind::Timezone);
            }
            Event::Failed(reason) => {
                self.show_banner(
                    &format!("The date and time settings couldn't be changed: {reason}"),
                    true,
                );
            }
        }
    }

    /// Shows the time zone, synchronization, and the current time in the panel's hours.
    pub(super) fn show_time(&self) {
        let twenty_four_hour = clock::is_24_hour(&self.store.borrow().config().panel.clock_format);
        let now = now_text(self.sources.now(), twenty_four_hour);
        let time = &self.state.borrow().time;
        let state = &time.state;
        let status = match (time.reported, state.available) {
            (false, _) => 0,
            (true, true) => 1,
            (true, false) => 2,
        };
        self.with_ui(|ui| {
            let model = ui.global::<TimeModel>();
            model.set_state(status);
            model.set_now(now.into());
            model.set_timezone(state.timezone.as_str().into());
            model.set_timezone_city(if state.timezone.is_empty() {
                "Unknown".into()
            } else {
                city(&state.timezone).into()
            });
            model.set_can_pick_timezone(state.available && !time.zones.is_empty());
            model.set_can_sync(state.can_ntp);
            model.set_sync(state.ntp);
            model.set_sync_status(sync_status(state).into());
        });
    }
}

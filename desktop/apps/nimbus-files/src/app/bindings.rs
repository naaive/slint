// SPDX-License-Identifier: MIT

//! Connects the window's callbacks to the controller.

use std::rc::{Rc, Weak};

use super::controller::Controller;
use crate::core::keymap::{Modifiers, Scope};

/// Wraps a handler so it runs only while the controller is alive; the window holds no strong reference to it.
fn bind<A>(
    controller: &Rc<Controller>,
    handler: impl Fn(&Rc<Controller>, A) + 'static,
) -> impl Fn(A) + 'static {
    let weak: Weak<Controller> = Rc::downgrade(controller);
    move |args| {
        if let Some(controller) = weak.upgrade() {
            handler(&controller, args);
        }
    }
}

fn index(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

pub(super) fn connect(c: &Rc<Controller>) {
    let ui = c.ui();
    let go_back = bind(c, |c, ()| c.go_back());
    ui.on_go_back(move || go_back(()));
    let go_forward = bind(c, |c, ()| c.go_forward());
    ui.on_go_forward(move || go_forward(()));
    let go_up = bind(c, |c, ()| c.go_up());
    ui.on_go_up(move || go_up(()));
    let crumb = bind(c, |c, i: i32| index(i).into_iter().for_each(|i| c.crumb_clicked(i)));
    ui.on_crumb_clicked(crumb);
    let edit = bind(c, |c, ()| c.edit_location());
    ui.on_edit_location(move || edit(()));
    let accepted = bind(c, |c, text: slint::SharedString| c.location_accepted(&text));
    ui.on_location_accepted(accepted);
    let cancelled = bind(c, |c, ()| c.location_cancelled());
    ui.on_location_cancelled(move || cancelled(()));
    let place = bind(c, |c, i: i32| index(i).into_iter().for_each(|i| c.place_clicked(i)));
    ui.on_place_clicked(place);

    let toggle_search = bind(c, |c, ()| c.toggle_search());
    ui.on_toggle_search(move || toggle_search(()));
    let search_edited = bind(c, |c, text: slint::SharedString| c.search_edited(&text));
    ui.on_search_edited(search_edited);
    let search_accepted = bind(c, |c, ()| c.search_accepted());
    ui.on_search_accepted(move || search_accepted(()));
    let recursive = bind(c, |c, ()| c.toggle_recursive());
    ui.on_toggle_recursive(move || recursive(()));

    let list_mode = bind(c, |c, list: bool| c.set_list_mode(list));
    ui.on_set_list_mode(list_mode);
    let sort_key = bind(c, |c, key: i32| c.set_sort_key(key));
    ui.on_set_sort_key(sort_key);
    let descending = bind(c, |c, d: bool| c.change_prefs(|p| p.sort.descending = d));
    ui.on_set_sort_descending(descending);
    let folders_first =
        bind(c, |c, ()| c.change_prefs(|p| p.sort.folders_first = !p.sort.folders_first));
    ui.on_toggle_folders_first(move || folders_first(()));
    let hidden = bind(c, |c, ()| c.toggle_hidden());
    ui.on_toggle_hidden(move || hidden(()));
    let column = bind(c, |c, key: i32| c.column_clicked(key));
    ui.on_column_clicked(column);
    let main_menu = bind(c, |c, (x, y): (f32, f32)| c.main_menu(x, y));
    ui.on_main_menu_requested(move |x, y| main_menu((x, y)));

    let pressed = bind(c, |c, (i, ctrl, shift): (i32, bool, bool)| {
        index(i).into_iter().for_each(|i| c.item_pressed(i, ctrl, shift));
    });
    ui.on_item_pressed(move |i, ctrl, shift| pressed((i, ctrl, shift)));
    let activated = bind(c, |c, i: i32| index(i).into_iter().for_each(|i| c.item_activated(i)));
    ui.on_item_activated(activated);
    let item_context = bind(c, |c, (i, x, y): (i32, f32, f32)| {
        index(i).into_iter().for_each(|i| c.item_context(i, x, y));
    });
    ui.on_item_context(move |i, x, y| item_context((i, x, y)));
    let background = bind(c, |c, ctrl: bool| c.background_pressed(ctrl));
    ui.on_background_pressed(background);
    let background_context = bind(c, |c, (x, y): (f32, f32)| c.background_context(x, y));
    ui.on_background_context(move |x, y| background_context((x, y)));
    let band = bind(c, |c, args: (f32, f32, f32, f32, i32, f32, f32, f32)| {
        let (ax, ay, bx, by, columns, width, height, origin) = args;
        c.band((ax, ay), (bx, by), columns, (width, height), origin);
    });
    ui.on_band(move |ax, ay, bx, by, columns, width, height, origin| {
        band((ax, ay, bx, by, columns, width, height, origin))
    });
    let thumbnail = bind(c, |c, i: i32| index(i).into_iter().for_each(|i| c.request_thumbnail(i)));
    ui.on_request_thumbnail(thumbnail);

    let weak = Rc::downgrade(c);
    ui.on_view_key(move |text, ctrl, shift, alt| {
        weak.upgrade().is_some_and(|c| c.key(&text, Modifiers { ctrl, shift, alt }, Scope::View))
    });
    let weak = Rc::downgrade(c);
    ui.on_window_key(move |text, ctrl, shift, alt| {
        weak.upgrade().is_some_and(|c| c.key(&text, Modifiers { ctrl, shift, alt }, Scope::Window))
    });

    let menu = bind(c, |c, i: i32| index(i).into_iter().for_each(|i| c.menu_activated(i)));
    ui.on_menu_activated(menu);
    let weak = Rc::downgrade(c);
    ui.on_menu_step(move |current, delta| {
        weak.upgrade().map_or(-1, |c| c.menu_step(current, delta))
    });

    let cancel_job = bind(c, |c, id: i32| c.cancel_job(id));
    ui.on_cancel_job(cancel_job);
    let toast_action = bind(c, |c, ()| c.toast_action());
    ui.on_toast_action_clicked(move || toast_action(()));
    let toast_dismissed = bind(c, |c, ()| c.hide_toast());
    ui.on_toast_dismissed(move || toast_dismissed(()));
    let restore = bind(c, |c, ()| c.restore_clicked());
    ui.on_restore_clicked(move || restore(()));
    let empty_trash = bind(c, |c, ()| c.confirm_empty_trash());
    ui.on_empty_trash_clicked(move || empty_trash(()));

    let name_edited = bind(c, |c, text: slint::SharedString| c.name_edited(&text));
    ui.on_name_edited(name_edited);
    let name_accepted = bind(c, |c, text: slint::SharedString| c.name_accepted(&text));
    ui.on_name_accepted(name_accepted);
    let confirm = bind(c, |c, ()| c.confirm_accepted());
    ui.on_confirm_accepted(move || confirm(()));
    let conflict = bind(c, |c, (choice, all): (i32, bool)| c.conflict_resolved(choice, all));
    ui.on_conflict_resolved(move |choice, all| conflict((choice, all)));
    let app = bind(c, |c, i: i32| index(i).into_iter().for_each(|i| c.app_chosen(i)));
    ui.on_app_chosen(app);
    let open_default = bind(c, |c, ()| c.open_default_chosen());
    ui.on_open_default_chosen(move || open_default(()));
    let dialog_cancelled = bind(c, |c, ()| c.dialog_cancelled());
    ui.on_dialog_cancelled(move || dialog_cancelled(()));
}

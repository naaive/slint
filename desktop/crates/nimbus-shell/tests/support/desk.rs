// SPDX-License-Identifier: MIT

//! A stand-in for a host: it shows the parts of a [`ShellView`] in windows on an output of a fixed size,
//! laid out the way the compositor arranges layer surfaces and popups,
//! routes input to them, and composites them into one image.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use nimbus_shell::{
    Align, Part, PartComponent, PartWindow, Placement, Popup, PopupPlacement, Rect, ShellView,
};
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PlatformError, SharedString};

/// Off-screen software windows for every component, which a [`Desk`] renders.
pub struct Software {
    created: RefCell<Option<Rc<MinimalSoftwareWindow>>>,
}

struct SoftwarePlatform(Rc<Software>);

impl Platform for SoftwarePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.create_window())
    }
}

impl Software {
    pub fn new() -> Rc<Self> {
        Rc::new(Self { created: RefCell::new(None) })
    }

    /// Installs a platform with only off-screen windows on this thread; it fails when the thread already has one.
    pub fn install() -> Result<Rc<Self>, PlatformError> {
        let software = Self::new();
        slint::platform::set_platform(Box::new(SoftwarePlatform(software.clone())))
            .map_err(|err| PlatformError::Other(format!("cannot set the platform: {err}")))?;
        Ok(software)
    }

    /// Makes the window of a new component; a platform calls this to create window adapters.
    pub fn create_window(&self) -> Rc<MinimalSoftwareWindow> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        *self.created.borrow_mut() = Some(window.clone());
        window
    }

    /// The window of the last component created since the last call.
    pub fn take_created(&self) -> Option<Rc<MinimalSoftwareWindow>> {
        self.created.borrow_mut().take()
    }
}

/// Draws `window` into `pixels`, which keep what earlier frames drew; returns whether anything changed.
pub fn draw(window: &MinimalSoftwareWindow, pixels: &mut Vec<PremultipliedRgbaColor>) -> bool {
    let size = window.size();
    let len = (size.width * size.height) as usize;
    if pixels.len() != len {
        *pixels = vec![PremultipliedRgbaColor::background(); len];
        window.request_redraw();
    }
    window.draw_if_needed(|renderer| {
        renderer.render(pixels, size.width as usize);
    })
}

/// A part's window and where it is on the output, in logical pixels.
pub struct Shown {
    pub window: PartWindow,
    pub rect: Rect,
    /// Where it opened, for a popup.
    placement: Option<PopupPlacement>,
    software: Option<Rc<MinimalSoftwareWindow>>,
    pixels: RefCell<Vec<PremultipliedRgbaColor>>,
}

/// The parts of one view on an output, created and dropped as the view asks.
pub struct Desk {
    pub view: ShellView,
    pub width: f32,
    pub height: f32,
    shown: RefCell<Vec<Shown>>,
    software: Option<Rc<Software>>,
    hovered: RefCell<Option<usize>>,
    /// Parts opened, closed, or moved since the last [`Desk::draw`].
    changed: Cell<bool>,
}

impl Desk {
    pub fn new(view: ShellView, width: f32, height: f32, software: Option<Rc<Software>>) -> Self {
        let desk = Self {
            view,
            width,
            height,
            shown: RefCell::default(),
            software,
            hovered: RefCell::default(),
            changed: Cell::new(true),
        };
        desk.sync();
        desk
    }

    /// Creates and drops windows until they match the view's parts, then lays them out.
    pub fn sync(&self) {
        let wanted = self.view.parts();
        let placement = self.view.popup_placement();
        let mut shown = self.shown.borrow_mut();
        let count = shown.len();
        shown.retain(|s| {
            wanted.contains(&s.window.part())
                && (!matches!(s.window.part(), Part::Popup(_)) || s.placement == placement)
        });
        let mut changed = shown.len() != count;
        for (order, part) in wanted.iter().enumerate() {
            if shown.iter().any(|s| s.window.part() == *part) {
                continue;
            }
            let window = self.view.create(*part).expect("the part's window opens");
            window.show().expect("the part's window shows");
            let software = self.software.as_ref().and_then(|s| s.take_created());
            let shown_part = Shown {
                window,
                rect: Rect::default(),
                placement: placement.filter(|_| matches!(part, Part::Popup(_))),
                software,
                pixels: RefCell::default(),
            };
            let at = shown.iter().filter(|s| wanted[..order].contains(&s.window.part())).count();
            shown.insert(at, shown_part);
            changed = true;
        }
        if changed {
            *self.hovered.borrow_mut() = None;
        }
        changed |= self.layout(&mut shown);
        if changed {
            self.changed.set(true);
        }
    }

    /// Places every shown part; returns whether any moved.
    fn layout(&self, shown: &mut [Shown]) -> bool {
        let output = Rect { x: 0.0, y: 0.0, width: self.width, height: self.height };
        let mut zone = output;
        let mut moved = false;
        for index in 0..shown.len() {
            let window = &shown[index].window;
            let rect = match window.placement() {
                Some(placement) => {
                    let area = if placement.exclusive_zone.is_some() { zone } else { output };
                    let rect = place(&placement, area);
                    reserve(&mut zone, &placement);
                    rect
                }
                None => {
                    let placement = shown[index].placement.expect("a popup has a placement");
                    let parent = shown
                        .iter()
                        .find(|s| s.window.part() == placement.parent)
                        .expect("a popup's parent is shown")
                        .rect;
                    place_popup(window, &placement, parent, self.width)
                }
            };
            let shown = &mut shown[index];
            moved |= shown.rect != rect;
            shown.rect = rect;
            let size = slint::LogicalSize::new(rect.width, rect.height);
            if shown.window.window().size().to_logical(1.0) != size {
                shown.window.window().set_size(size);
            }
        }
        moved
    }

    /// Runs `with` on the shown window of `part`, after syncing.
    pub fn with<T>(&self, part: Part, with: impl FnOnce(&Shown) -> T) -> Option<T> {
        self.sync();
        self.shown.borrow().iter().find(|s| s.window.part() == part).map(with)
    }

    /// The component of `part`, after syncing.
    pub fn component(&self, part: Part) -> Option<PartComponent> {
        self.with(part, |s| s.window.component().clone())
    }

    pub fn parts(&self) -> Vec<Part> {
        self.sync();
        self.shown.borrow().iter().map(|s| s.window.part()).collect()
    }

    /// The input region of `part` in output coordinates, or nothing when it isn't shown.
    pub fn input_region(&self, part: Part) -> Vec<Rect> {
        self.with(part, |s| {
            s.window
                .input_region()
                .into_iter()
                .map(|r| Rect { x: r.x + s.rect.x, y: r.y + s.rect.y, ..r })
                .collect()
        })
        .unwrap_or_default()
    }

    /// The part that gets keys: a popup, which holds a grab, or else a part that takes the keyboard.
    pub fn keyboard_window(&self) -> Option<PartComponent> {
        self.sync();
        let shown = self.shown.borrow();
        let popup = shown.iter().find(|s| matches!(s.window.part(), Part::Popup(_)));
        let keyboard = shown.iter().find(|s| s.window.placement().is_some_and(|p| p.keyboard));
        popup.or(keyboard).map(|s| s.window.component().clone())
    }

    /// Sends a key to the part that gets keys.
    pub fn key(&self, text: SharedString, pressed: bool) {
        if let Some(part) = self.keyboard_window() {
            let event = if pressed {
                WindowEvent::KeyPressed { text }
            } else {
                WindowEvent::KeyReleased { text }
            };
            part.window().dispatch_event(event);
        }
    }

    /// Sends a pointer event at a point of the output to the topmost part that takes input there,
    /// and tells the part the pointer was over before that it left.
    /// Returns whether a part took the event.
    pub fn pointer(
        &self,
        x: f32,
        y: f32,
        event: impl FnOnce(LogicalPosition) -> WindowEvent,
    ) -> bool {
        self.sync();
        let shown = self.shown.borrow();
        let target = shown.iter().enumerate().rev().find_map(|(index, s)| {
            let local = (x - s.rect.x, y - s.rect.y);
            let inside = s.window.input_region().iter().any(|r| r.contains(local.0, local.1));
            inside.then_some((index, local))
        });
        let previous = self.hovered.replace(target.map(|(index, _)| index));
        if let Some(previous) = previous.filter(|p| Some(*p) != target.map(|t| t.0))
            && let Some(s) = shown.get(previous)
        {
            s.window.window().dispatch_event(WindowEvent::PointerExited);
        }
        let Some((index, (x, y))) = target else {
            return false;
        };
        shown[index].window.window().dispatch_event(event(LogicalPosition::new(x, y)));
        true
    }

    /// Moves the pointer to a point of the output.
    pub fn move_pointer(&self, x: f32, y: f32) {
        self.pointer(x, y, |position| WindowEvent::PointerMoved { position });
    }

    /// Presses or releases a button at a point of the output.
    /// A press outside the shell's parts dismisses the open popup, as its grab does on a compositor.
    pub fn button(&self, x: f32, y: f32, button: PointerEventButton, pressed: bool) {
        let event = |position| {
            if pressed {
                WindowEvent::PointerPressed { position, button }
            } else {
                WindowEvent::PointerReleased { position, button }
            }
        };
        if !self.pointer(x, y, event) && pressed && self.view.popup() != Popup::None {
            self.view.close_popup();
        }
    }

    /// Draws what changed in the shown parts; returns whether the composited image changed.
    /// With `force`, it draws every part in full.
    pub fn draw(&self, force: bool) -> bool {
        self.sync();
        slint::platform::update_timers_and_animations();
        self.sync();
        let mut drawn = self.changed.take();
        for shown in self.shown.borrow().iter() {
            let Some(software) = &shown.software else { continue };
            if force {
                software.request_redraw();
            }
            drawn |= draw(software, &mut shown.pixels.borrow_mut());
        }
        drawn
    }

    /// Composites the shown parts as last drawn over `backdrop`, which gives the color of each pixel below them.
    pub fn composite(&self, backdrop: impl Fn(u32, u32) -> [f32; 3]) -> Vec<[u8; 3]> {
        let (width, height) = (self.width as u32, self.height as u32);
        let mut canvas: Vec<[f32; 3]> =
            (0..width * height).map(|i| backdrop(i % width, i / width)).collect();
        for shown in self.shown.borrow().iter() {
            let Some(software) = &shown.software else { continue };
            let size = software.size();
            let (left, top) = (shown.rect.x.round() as i64, shown.rect.y.round() as i64);
            for (i, p) in shown.pixels.borrow().iter().enumerate() {
                let x = left + i64::from(i as u32 % size.width);
                let y = top + i64::from(i as u32 / size.width);
                if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
                    continue;
                }
                let under = &mut canvas[(y as u32 * width + x as u32) as usize];
                let alpha = f32::from(p.alpha) / 255.0;
                for (channel, over) in under.iter_mut().zip([p.red, p.green, p.blue]) {
                    *channel = f32::from(over) + *channel * (1.0 - alpha);
                }
            }
        }
        canvas.iter().map(|rgb| rgb.map(|c| c.round().clamp(0.0, 255.0) as u8)).collect()
    }

    /// Draws every shown part in full and composites them over `backdrop`.
    pub fn render(&self, backdrop: impl Fn(u32, u32) -> [f32; 3]) -> Vec<[u8; 3]> {
        self.draw(true);
        self.composite(backdrop)
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        // Parts above go first, as a host drops popups before their parents.
        let mut shown = self.shown.borrow_mut();
        while shown.pop().is_some() {}
    }
}

/// The rectangle of a part with `placement` in `area`, as layer shell places surfaces.
fn place(placement: &Placement, area: Rect) -> Rect {
    let edges = placement.edges;
    let margin = placement.margin;
    let width = if edges.left && edges.right { area.width - 2.0 * margin } else { placement.width };
    let height =
        if edges.top && edges.bottom { area.height - 2.0 * margin } else { placement.height };
    let x = match (edges.left, edges.right) {
        (true, _) => area.x + margin,
        (false, true) => area.x + area.width - width - margin,
        (false, false) => area.x + (area.width - width) / 2.0,
    };
    let y = match (edges.top, edges.bottom) {
        (true, _) => area.y + margin,
        (false, true) => area.y + area.height - height - margin,
        (false, false) => area.y + (area.height - height) / 2.0,
    };
    Rect { x, y, width, height }
}

/// Takes a part's exclusive zone off the edge of `zone` it's attached to.
fn reserve(zone: &mut Rect, placement: &Placement) {
    let Some(exclusive) = placement.exclusive_zone.filter(|z| *z > 0.0) else {
        return;
    };
    let taken = exclusive + placement.margin;
    match (placement.edges.top, placement.edges.bottom) {
        (true, false) => {
            zone.y += taken;
            zone.height -= taken;
        }
        (false, true) => zone.height -= taken,
        _ => {}
    }
}

/// The rectangle of a popup's window next to its anchor in `parent`, kept on an output `width` wide.
fn place_popup(window: &PartWindow, placement: &PopupPlacement, parent: Rect, width: f32) -> Rect {
    let geometry = window.geometry();
    let (surface_width, surface_height) = window.size();
    let anchor = placement.anchor;
    let (ax, ay) = (parent.x + anchor.x, parent.y + anchor.y);
    let x = match placement.align {
        Align::Center => ax + (anchor.width - geometry.width) / 2.0,
        Align::End => ax + anchor.width - geometry.width,
    };
    let x = x.clamp(0.0, (width - geometry.width).max(0.0));
    let y = if placement.below {
        ay + anchor.height + placement.gap
    } else {
        ay - placement.gap - geometry.height
    };
    Rect { x: x - geometry.x, y: y - geometry.y, width: surface_width, height: surface_height }
}

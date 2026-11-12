// Input normalization: raw pointer events in, intents out.
//
// The host owns three things a view must never track: what the pointer is over,
// which region captured the press, and whether that press became a drag.
// Everything else -- what a click *means* -- is the view's decision, carried as
// an intent with the modifiers intact.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }

impl Rect {
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x as f32 && x <= (self.x + self.w) as f32
            && y >= self.y as f32 && y <= (self.y + self.h) as f32
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region { pub id: String, pub rect: Rect }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods { pub cmd: bool, pub shift: bool, pub alt: bool, pub ctrl: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button { Primary, Secondary, Middle }

#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    Activate { id: String, mods: Mods },
    Context { id: String, x: f32, y: f32 },
    DragStart { id: String },
    Drag { id: String, dx: f32, dy: f32 },
    DragEnd { id: String, cancelled: bool },
    Scroll { id: Option<String>, delta: f32, mods: Mods },
}

#[derive(Clone, Debug)]
struct Capture { id: String, origin: (f32, f32), last: (f32, f32), dragging: bool }

/// Distance before a press becomes a drag, in logical pixels.
const DRAG_THRESHOLD: f32 = 4.0;

#[derive(Default)]
pub struct Router { hover: Option<String>, capture: Option<Capture>, cursor: (f32, f32), focus: Option<String> }

impl Router {
    pub fn new() -> Self { Self::default() }
    pub fn hover(&self) -> Option<&str> { self.hover.as_deref() }

    /// Last pointer position seen, for event sources that report buttons
    /// without coordinates (winit does).
    pub fn cursor_position(&self) -> (f32, f32) { self.cursor }

    /// Keyboard focus. The host owns it, the view styles it.
    pub fn focus(&self) -> Option<&str> { self.focus.as_deref() }
    pub fn set_focus(&mut self, id: Option<String>) { self.focus = id; }

    /// Tab and shift-Tab walk the regions in paint order.
    pub fn focus_next(&mut self, regions: &[Region], backwards: bool) {
        if regions.is_empty() { return; }
        let current = self.focus.as_ref().and_then(|id| regions.iter().position(|r| &r.id == id));
        let next = match (current, backwards) {
            (Some(index), false) => (index + 1) % regions.len(),
            (Some(index), true) => (index + regions.len() - 1) % regions.len(),
            (None, false) => 0,
            (None, true) => regions.len() - 1,
        };
        self.focus = Some(regions[next].id.clone());
    }

    /// The region holding the press, when the press has not become a drag.
    pub fn pressed(&self) -> Option<&str> {
        self.capture.as_ref().filter(|c| !c.dragging).map(|c| c.id.as_str())
    }

    fn hit(regions: &[Region], x: f32, y: f32) -> Option<&Region> {
        regions.iter().rev().find(|region| region.rect.contains(x, y))
    }

    pub fn pointer_moved(&mut self, regions: &[Region], x: f32, y: f32) -> Vec<Intent> {
        self.cursor = (x, y);
        self.hover = Self::hit(regions, x, y).map(|r| r.id.clone());
        let Some(capture) = self.capture.as_mut() else { return Vec::new() };
        let dx = x - capture.last.0;
        let dy = y - capture.last.1;
        capture.last = (x, y);
        if !capture.dragging {
            let total = ((x - capture.origin.0).powi(2) + (y - capture.origin.1).powi(2)).sqrt();
            if total < DRAG_THRESHOLD { return Vec::new(); }
            capture.dragging = true;
            return vec![Intent::DragStart { id: capture.id.clone() }];
        }
        vec![Intent::Drag { id: capture.id.clone(), dx, dy }]
    }

    pub fn pointer_down(&mut self, regions: &[Region], button: Button, x: f32, y: f32, mods: Mods) -> Vec<Intent> {
        self.hover = Self::hit(regions, x, y).map(|r| r.id.clone());
        let Some(region) = Self::hit(regions, x, y).map(|r| r.id.clone()) else {
            self.capture = None;
            return Vec::new();
        };
        // Secondary and ctrl+click are a context request, not a press: there is
        // nothing to capture, because a menu dismisses on the next down.
        if button == Button::Secondary || (mods.ctrl && button == Button::Primary) {
            return vec![Intent::Context { id: region, x, y }];
        }
        // A press focuses what it lands on: focus follows the pointer into the
        // control, the way every desktop toolkit behaves.
        self.focus = Some(region.clone());
        self.capture = Some(Capture { id: region, origin: (x, y), last: (x, y), dragging: false });
        Vec::new()
    }

    pub fn pointer_up(&mut self, regions: &[Region], x: f32, y: f32, mods: Mods) -> Vec<Intent> {
        self.hover = Self::hit(regions, x, y).map(|r| r.id.clone());
        // An up with no capture is a lost up: ignore it rather than activating
        // whatever happens to be under the pointer now.
        let Some(capture) = self.capture.take() else { return Vec::new() };
        let under = Self::hit(regions, x, y).map(|r| r.id.clone());
        if capture.dragging {
            let cancelled = under.as_deref() != Some(capture.id.as_str());
            return vec![Intent::DragEnd { id: capture.id, cancelled }];
        }
        if under.as_deref() == Some(capture.id.as_str()) {
            return vec![Intent::Activate { id: capture.id, mods }];
        }
        Vec::new()
    }

    pub fn wheel(&mut self, regions: &[Region], x: f32, y: f32, delta: f32, mods: Mods) -> Vec<Intent> {
        let id = Self::hit(regions, x, y).map(|r| r.id.clone());
        vec![Intent::Scroll { id, delta, mods }]
    }

    /// The pointer left the window: nothing is hovered any more.
    pub fn pointer_left(&mut self) { self.hover = None; }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> Vec<Region> {
        vec![
            Region { id: "a".into(), rect: Rect { x: 0, y: 0, w: 100, h: 100 } },
            Region { id: "b".into(), rect: Rect { x: 0, y: 0, w: 50, h: 50 } },
        ]
    }

    #[test]
    fn press_and_release_inside_activates() {
        let mut r = Router::new();
        let regs = regions();
        assert!(r.pointer_down(&regs, Button::Primary, 10.0, 10.0, Mods::default()).is_empty());
        assert_eq!(r.pressed(), Some("b"));
        assert_eq!(r.pointer_up(&regs, 12.0, 12.0, Mods::default()),
            vec![Intent::Activate { id: "b".into(), mods: Mods::default() }]);
        assert_eq!(r.pressed(), None);
    }

    #[test]
    fn release_outside_does_not_activate() {
        let mut r = Router::new();
        let regs = regions();
        r.pointer_down(&regs, Button::Primary, 10.0, 10.0, Mods::default());
        assert!(r.pointer_up(&regs, 300.0, 300.0, Mods::default()).is_empty());
    }

    #[test]
    fn drag_beyond_threshold_emits_start_and_end_cancelled() {
        let mut r = Router::new();
        let regs = regions();
        r.pointer_down(&regs, Button::Primary, 10.0, 10.0, Mods::default());
        assert!(r.pointer_moved(&regs, 12.0, 12.0).is_empty());
        assert_eq!(r.pointer_moved(&regs, 40.0, 10.0), vec![Intent::DragStart { id: "b".into() }]);
        assert_eq!(r.pressed(), None, "a drag is not a press");
        assert_eq!(r.pointer_moved(&regs, 60.0, 10.0),
            vec![Intent::Drag { id: "b".into(), dx: 20.0, dy: 0.0 }]);
        assert_eq!(r.pointer_up(&regs, 90.0, 90.0, Mods::default()),
            vec![Intent::DragEnd { id: "b".into(), cancelled: true }]);
    }

    #[test]
    fn right_click_is_context_not_activation() {
        let mut r = Router::new();
        let regs = regions();
        assert_eq!(r.pointer_down(&regs, Button::Secondary, 5.0, 5.0, Mods::default()),
            vec![Intent::Context { id: "b".into(), x: 5.0, y: 5.0 }]);
        assert!(r.pointer_up(&regs, 5.0, 5.0, Mods::default()).is_empty());
    }

    #[test]
    fn ctrl_click_is_context_on_primary() {
        let mut r = Router::new();
        let regs = regions();
        let mods = Mods { ctrl: true, ..Default::default() };
        assert_eq!(r.pointer_down(&regs, Button::Primary, 5.0, 5.0, mods),
            vec![Intent::Context { id: "b".into(), x: 5.0, y: 5.0 }]);
    }

    #[test]
    fn modifiers_ride_along_with_activation() {
        let mut r = Router::new();
        let regs = regions();
        let mods = Mods { cmd: true, ..Default::default() };
        r.pointer_down(&regs, Button::Primary, 5.0, 5.0, mods);
        assert_eq!(r.pointer_up(&regs, 5.0, 5.0, mods),
            vec![Intent::Activate { id: "b".into(), mods }]);
    }

    #[test]
    fn lost_up_is_ignored() {
        let mut r = Router::new();
        let regs = regions();
        assert!(r.pointer_up(&regs, 5.0, 5.0, Mods::default()).is_empty());
    }

    #[test]
    fn wheel_targets_the_region_under_the_pointer() {
        let mut r = Router::new();
        let regs = regions();
        assert_eq!(r.wheel(&regs, 5.0, 5.0, -3.0, Mods::default()),
            vec![Intent::Scroll { id: Some("b".into()), delta: -3.0, mods: Mods::default() }]);
    }

    #[test]
    fn topmost_region_wins() {
        let mut r = Router::new();
        let regs = regions();
        r.pointer_down(&regs, Button::Primary, 5.0, 5.0, Mods::default());
        assert_eq!(r.pressed(), Some("b"), "b is painted last, so it is on top");
    }
}

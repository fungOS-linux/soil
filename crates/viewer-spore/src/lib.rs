//! Core state machine for the Soil Viewer Spore.
//!
//! This crate deliberately contains no browser, network, display or capture
//! authority. The host owns those objects behind `soil:view`; this state
//! machine only resolves a reviewed target and normalizes presentation actions.
//! It can be compiled to wasm once the guest bindings are generated.

use std::collections::BTreeMap;

/// Largest PNG the viewer may ask the host to produce in one capture.
pub const MAX_CAPTURE_BYTES: u32 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Web,
    Spore,
    Retained,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub kind: TargetKind,
    pub title: String,
    pub count: u64,
}

impl Target {
    pub fn new(
        id: impl Into<String>,
        kind: TargetKind,
        title: impl Into<String>,
        count: u64,
    ) -> Result<Self, Error> {
        let id = id.into();
        if id.is_empty() || id.len() > 128 {
            return Err(Error::Invalid);
        }
        if count == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            id,
            kind,
            title: title.into(),
            count,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Next,
    Previous,
    First,
    Last,
    Select,
    Back,
    FullscreenOn,
    FullscreenOff,
    FullscreenToggle,
}

/// Keys a host may normalize before forwarding to a Spore.
///
/// Raw Android keycodes never cross this boundary. D-pad up/down and menu/info
/// are host chrome, not content navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKey {
    DpadLeft,
    DpadRight,
    DpadUp,
    DpadDown,
    Ok,
    Back,
    PlayPause,
    Rewind,
    FastForward,
    Menu,
    Info,
}

impl RemoteKey {
    pub fn action(self) -> Option<Action> {
        match self {
            Self::DpadLeft | Self::Rewind => Some(Action::Previous),
            Self::DpadRight | Self::FastForward => Some(Action::Next),
            Self::Ok => Some(Action::Select),
            Self::Back => Some(Action::Back),
            Self::PlayPause => Some(Action::FullscreenToggle),
            Self::DpadUp | Self::DpadDown | Self::Menu | Self::Info => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub target: Target,
    pub position: u64,
    pub count: u64,
    pub loading: bool,
    pub fullscreen: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureRequest {
    pub generation: u64,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Denied,
    UnknownTarget,
    NotReady,
    Unsupported,
    OverBudget,
    CaptureDenied,
    Invalid,
    Closed,
}

#[derive(Debug)]
struct OpenView {
    target: Target,
    position: u64,
    loading: bool,
    fullscreen: bool,
}

#[derive(Debug)]
pub struct Viewer {
    targets: BTreeMap<String, Target>,
    view: Option<OpenView>,
    generation: u64,
    closed: bool,
}

impl Viewer {
    /// Build a viewer from the host's already-reviewed target list.
    pub fn new(targets: impl IntoIterator<Item = Target>) -> Result<Self, Error> {
        let mut index = BTreeMap::new();
        for target in targets {
            if index.insert(target.id.clone(), target).is_some() {
                return Err(Error::Invalid);
            }
        }
        Ok(Self {
            targets: index,
            view: None,
            generation: 0,
            closed: false,
        })
    }

    /// Resolve a reviewed target id. There is no URL/path fallback.
    pub fn open(&mut self, target_id: &str) -> Result<State, Error> {
        self.ensure_open()?;
        let target = self
            .targets
            .get(target_id)
            .cloned()
            .ok_or(Error::UnknownTarget)?;
        self.generation = self.generation.saturating_add(1);
        self.view = Some(OpenView {
            target,
            position: 0,
            loading: false,
            fullscreen: false,
        });
        self.state()
    }

    /// Apply a normalized presentation action.
    pub fn act(&mut self, action: Action) -> Result<State, Error> {
        self.ensure_open()?;
        let view = self.view.as_mut().ok_or(Error::NotReady)?;
        let before = (view.position, view.loading, view.fullscreen);

        match action {
            Action::Next => {
                if view.position + 1 < view.target.count {
                    view.position += 1;
                }
            }
            Action::Previous => {
                view.position = view.position.saturating_sub(1);
            }
            Action::First => view.position = 0,
            Action::Last => view.position = view.target.count - 1,
            Action::Select => {}
            Action::Back => {
                if view.fullscreen {
                    view.fullscreen = false;
                } else {
                    view.position = view.position.saturating_sub(1);
                }
            }
            Action::FullscreenOn => view.fullscreen = true,
            Action::FullscreenOff => view.fullscreen = false,
            Action::FullscreenToggle => view.fullscreen = !view.fullscreen,
        }

        if (view.position, view.loading, view.fullscreen) != before {
            self.generation = self.generation.saturating_add(1);
        }
        self.state()
    }

    /// Apply a host-normalized remote key. Returns `None` for host chrome keys.
    pub fn remote(&mut self, key: RemoteKey) -> Result<Option<State>, Error> {
        match key.action() {
            Some(action) => self.act(action).map(Some),
            None => Ok(None),
        }
    }

    pub fn observe(&self) -> Result<State, Error> {
        self.ensure_open()?;
        self.state()
    }

    /// Ask the host for one bounded PNG. The host, not this state machine,
    /// checks the separate `soil:view.capture` grant and reads the pixels.
    pub fn capture(&self, max_bytes: u32) -> Result<CaptureRequest, Error> {
        self.ensure_open()?;
        if max_bytes == 0 {
            return Err(Error::Invalid);
        }
        if max_bytes > MAX_CAPTURE_BYTES {
            return Err(Error::OverBudget);
        }
        Ok(CaptureRequest {
            generation: self.generation,
            max_bytes,
        })
    }

    pub fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        self.closed = true;
        self.view = None;
        self.generation = self.generation.saturating_add(1);
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    fn ensure_open(&self) -> Result<(), Error> {
        if self.closed {
            Err(Error::Closed)
        } else {
            Ok(())
        }
    }

    fn state(&self) -> Result<State, Error> {
        let view = self.view.as_ref().ok_or(Error::NotReady)?;
        Ok(State {
            target: view.target.clone(),
            position: view.position,
            count: view.target.count,
            loading: view.loading,
            fullscreen: view.fullscreen,
            generation: self.generation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer() -> Viewer {
        Viewer::new([
            Target::new("deck", TargetKind::Web, "Venture deck", 32).unwrap(),
            Target::new("brief", TargetKind::Retained, "Brief", 4).unwrap(),
        ])
        .unwrap()
    }

    #[test]
    fn unknown_targets_fail_without_advancing_generation() {
        let mut viewer = viewer();
        assert_eq!(
            viewer.open("https://example.test"),
            Err(Error::UnknownTarget)
        );
        assert_eq!(viewer.observe(), Err(Error::NotReady));
    }

    #[test]
    fn opening_a_target_starts_at_the_first_slide() {
        let mut viewer = viewer();
        let state = viewer.open("deck").unwrap();
        assert_eq!(state.position, 0);
        assert_eq!(state.count, 32);
        assert_eq!(state.target.kind, TargetKind::Web);
        assert!(!state.loading);
        assert_eq!(state.generation, 1);
    }

    #[test]
    fn navigation_clamps_and_only_changes_generation_on_movement() {
        let mut viewer = viewer();
        let opened = viewer.open("brief").unwrap();
        assert_eq!(viewer.act(Action::Previous).unwrap().position, 0);
        assert_eq!(
            viewer.act(Action::First).unwrap().generation,
            opened.generation
        );
        let last = viewer.act(Action::Last).unwrap();
        assert_eq!(last.position, 3);
        assert_eq!(last.generation, opened.generation + 1);
        assert_eq!(viewer.act(Action::Next).unwrap().position, 3);
        assert_eq!(
            viewer.act(Action::Next).unwrap().generation,
            last.generation
        );
        let previous = viewer.act(Action::Previous).unwrap();
        assert_eq!(previous.position, 2);
        assert_eq!(previous.generation, last.generation + 1);
        let previous = viewer.act(Action::Previous).unwrap();
        assert_eq!(previous.position, 1);
        assert_eq!(previous.generation, last.generation + 2);
    }

    #[test]
    fn fullscreen_and_back_have_deliberate_order() {
        let mut viewer = viewer();
        viewer.open("deck").unwrap();
        let fs = viewer.act(Action::FullscreenToggle).unwrap();
        assert!(fs.fullscreen);
        let back = viewer.act(Action::Back).unwrap();
        assert!(!back.fullscreen);
        assert_eq!(back.position, 0);
        viewer.act(Action::Next).unwrap();
        let back = viewer.act(Action::Back).unwrap();
        assert!(!back.fullscreen);
        assert_eq!(back.position, 0);
    }

    #[test]
    fn remote_keys_map_to_content_actions_not_raw_codes() {
        let mut viewer = viewer();
        viewer.open("deck").unwrap();
        assert_eq!(
            viewer
                .remote(RemoteKey::DpadRight)
                .unwrap()
                .unwrap()
                .position,
            1
        );
        assert_eq!(
            viewer
                .remote(RemoteKey::FastForward)
                .unwrap()
                .unwrap()
                .position,
            2
        );
        assert_eq!(
            viewer
                .remote(RemoteKey::DpadLeft)
                .unwrap()
                .unwrap()
                .position,
            1
        );
        assert_eq!(
            viewer.remote(RemoteKey::Rewind).unwrap().unwrap().position,
            0
        );
        assert!(viewer.remote(RemoteKey::DpadUp).unwrap().is_none());
        assert!(viewer.remote(RemoteKey::Menu).unwrap().is_none());
        assert!(viewer.remote(RemoteKey::Info).unwrap().is_none());
    }

    #[test]
    fn capture_is_bounded_and_generation_bound() {
        let mut viewer = viewer();
        let state = viewer.open("deck").unwrap();
        assert_eq!(viewer.capture(0), Err(Error::Invalid));
        assert_eq!(
            viewer.capture(MAX_CAPTURE_BYTES + 1),
            Err(Error::OverBudget)
        );
        let request = viewer.capture(1024).unwrap();
        assert_eq!(request.generation, state.generation);
        assert_eq!(request.max_bytes, 1024);
    }

    #[test]
    fn closing_revokes_future_actions_and_capture() {
        let mut viewer = viewer();
        viewer.open("deck").unwrap();
        viewer.close().unwrap();
        assert_eq!(viewer.act(Action::Next), Err(Error::Closed));
        assert_eq!(viewer.observe(), Err(Error::Closed));
        assert_eq!(viewer.capture(1024), Err(Error::Closed));
        assert_eq!(viewer.close(), Err(Error::Closed));
        assert_eq!(viewer.open("deck"), Err(Error::Closed));
    }

    #[test]
    fn duplicate_or_empty_targets_are_refused() {
        let target = Target::new("deck", TargetKind::Web, "Deck", 1).unwrap();
        assert_eq!(
            Viewer::new([target.clone(), target]).unwrap_err(),
            Error::Invalid
        );
        assert_eq!(
            Target::new("", TargetKind::Web, "Deck", 1),
            Err(Error::Invalid)
        );
        assert_eq!(
            Target::new("deck", TargetKind::Web, "Deck", 0),
            Err(Error::Invalid)
        );
    }
}

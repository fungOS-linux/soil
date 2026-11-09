//! What the launcher shows at rest, and how it is dressed.
//!
//! Learned by default, pinned when a person says so: `pins` is the only part
//! anyone has to curate, and an empty list means "show me what I use". This is
//! device state like the usage cache, not mesh state, so it lives beside it and
//! never syncs.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{kind_from_name, store};

const FORMAT_VERSION: u32 = 1;
const MAX_PINS: usize = 64;
const MAX_HIDDEN: usize = 16;
const MAX_NAME: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// A character or emoji the avatar draws. Empty means derive it from `name`.
    #[serde(default)]
    pub avatar: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "You".into(),
            avatar: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherConfig {
    #[serde(default = "format_version")]
    version: u32,
    #[serde(default)]
    pub profile: Profile,
    /// Ids that are always on the wall, in this order.
    #[serde(default)]
    pub pins: Vec<String>,
    /// Kinds the person asked not to see at rest.
    #[serde(default)]
    pub hidden_kinds: Vec<String>,
}

fn format_version() -> u32 {
    FORMAT_VERSION
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            profile: Profile::default(),
            pins: Vec::new(),
            hidden_kinds: Vec::new(),
        }
    }
}

impl LauncherConfig {
    pub fn load(path: &Path) -> Self {
        store::read::<Self>(path, "launcher config")
            .filter(|config| config.version == FORMAT_VERSION)
            .map(LauncherConfig::normalized)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        store::write(path, self)
    }

    pub fn pinned_rank(&self, id: &str) -> Option<usize> {
        self.pins.iter().position(|pin| pin == id)
    }

    pub fn hides(&self, kind: &str) -> bool {
        self.hidden_kinds.iter().any(|hidden| hidden == kind)
    }

    /// Clamp a config that came from a browser. Pins stay in the order given
    /// (that order *is* the wall), hidden kinds must be kinds that exist, and a
    /// profile is a label rather than a paragraph.
    pub fn normalized(mut self) -> Self {
        self.version = FORMAT_VERSION;
        let mut seen = std::collections::BTreeSet::new();
        self.pins = self
            .pins
            .into_iter()
            .map(|pin| pin.trim().to_owned())
            .filter(|pin| !pin.is_empty() && pin.len() <= 512 && seen.insert(pin.clone()))
            .take(MAX_PINS)
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        self.hidden_kinds = self
            .hidden_kinds
            .into_iter()
            .map(|kind| kind.trim().to_lowercase())
            .filter(|kind| kind_from_name(kind).is_some() && seen.insert(kind.clone()))
            .take(MAX_HIDDEN)
            .collect();
        self.profile.name = self.profile.name.trim().chars().take(MAX_NAME).collect();
        if self.profile.name.is_empty() {
            self.profile.name = Profile::default().name;
        }
        self.profile.avatar = self.profile.avatar.trim().chars().take(2).collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_config_is_learned_only() {
        let dir = tempfile::tempdir().unwrap();
        let config = LauncherConfig::load(&dir.path().join("launcher.json"));
        assert!(config.pins.is_empty());
        assert!(config.hidden_kinds.is_empty());
        assert_eq!(config.profile.name, "You");
    }

    #[test]
    fn pins_keep_their_order_and_drop_duplicates() {
        let config = LauncherConfig {
            pins: vec!["b".into(), "a".into(), "b".into(), "  ".into()],
            ..Default::default()
        }
        .normalized();
        assert_eq!(config.pins, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn hidden_kinds_must_be_kinds() {
        let config = LauncherConfig {
            hidden_kinds: vec![
                "application".into(),
                "nonsense".into(),
                "APPLICATION".into(),
            ],
            ..Default::default()
        }
        .normalized();
        assert_eq!(config.hidden_kinds, vec!["application".to_string()]);
        assert!(config.hides("application"));
        assert!(!config.hides("spore"));
    }

    #[test]
    fn a_profile_is_a_label_not_a_paragraph() {
        let mut config = LauncherConfig::default();
        config.profile.name = "   ".into();
        assert_eq!(config.clone().normalized().profile.name, "You");
        config.profile.name = "Avery".into();
        config.profile.avatar = "AW".into();
        let config = config.normalized();
        assert_eq!(config.profile.name, "Avery");
        assert_eq!(config.profile.avatar, "AW");
    }

    #[test]
    fn a_saved_config_is_the_config_that_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/launcher.json");
        let config = LauncherConfig {
            pins: vec!["com.apple.Safari".into()],
            profile: Profile {
                name: "Avery".into(),
                avatar: String::new(),
            },
            ..Default::default()
        };
        config.save(&path).unwrap();
        let loaded = LauncherConfig::load(&path);
        assert_eq!(loaded.pins, config.pins);
        assert_eq!(loaded.profile.name, "Avery");
    }
}

//! Update planning: what applying a catalog entry would cost.
//!
//! The class is a promise to the person using the machine, so it is computed
//! from the packages rather than guessed from a version bump:
//!
//! | class | what changed |
//! |---|---|
//! | `live` | only the Lua view; Canvas hot-reloads it |
//! | `restart` | the module or an asset; opens on the new version next launch |
//! | `consent` | the grant set grew |
//! | `system` | the system role or the publisher changed |
//! | `hold` | pinned, or a downgrade |
//!
//! Precedence is `hold` > `system` > `consent` > `live`/`restart`: the most
//! disruptive truth wins, so an update that both changes the module and grows
//! the grant set is never advertised as a restart.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use soil_spore::Manifest;

use crate::index::Entry;
use crate::{Result, compare_versions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Live,
    Restart,
    Consent,
    System,
    Hold,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Restart => "restart",
            Self::Consent => "consent",
            Self::System => "system",
            Self::Hold => "hold",
        }
    }

    /// The one-line explanation the storefront shows beside the class.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Live => "view only · applies now",
            Self::Restart => "module rebuilt · next launch",
            Self::Consent => "new capability · needs you",
            Self::System => "system surface · needs reboot",
            Self::Hold => "pinned · will not apply",
        }
    }

    /// Whether auto-update may apply this class without a person.
    pub fn automatic(self) -> bool {
        matches!(self, Self::Live | Self::Restart)
    }
}

/// What is installed on this machine.
#[derive(Debug, Clone)]
pub struct Installed {
    pub manifest: Manifest,
    /// The package directory, for reading the manual and assets.
    pub root: PathBuf,
    /// `spore.lock` digest of the installed package.
    pub digest: String,
    /// SHA-256 of `module.wasm`, or `None` for a composition.
    pub module: Option<String>,
    /// Digest over the sorted `ui/**/*.lua` hashes.
    pub ui: Option<String>,
    /// The publisher recorded at install time, when the index had one.
    pub publisher: Option<String>,
    /// A policy label when a person or a fleet pinned this Spore.
    pub pinned: Option<String>,
}

impl Installed {
    /// Read an installed package directory.
    pub fn read(root: &Path, publisher: Option<String>, pinned: Option<String>) -> Result<Self> {
        let manifest = Manifest::load(root)?;
        let files = soil_spore::collect_package(root, &manifest)?;
        let lock = soil_spore::compute_lock(root, &manifest)?;
        Ok(Self {
            digest: lock.digest,
            module: module_digest(&manifest, &files),
            ui: view_digest(&files),
            manifest,
            root: root.to_path_buf(),
            publisher,
            pinned,
        })
    }

    pub fn id(&self) -> &str {
        &self.manifest.spore.id
    }
}

/// SHA-256 of the module a package would run, if it has one.
pub fn module_digest(manifest: &Manifest, files: &[(String, Vec<u8>)]) -> Option<String> {
    let path = manifest.module.as_ref()?.path.as_str();
    let bytes = files
        .iter()
        .find(|(name, _)| name == path)
        .map(|(_, bytes)| bytes)?;
    Some(crate::hex(&Sha256::digest(bytes)))
}

/// The identity a hot reload sees: every `ui/**/*.lua` under the package.
///
/// Sorted path plus digest of bytes, then one hash over the lines. Two builds
/// that ship the same view agree here; a rebuild that changes only whitespace
/// does not, which is the honest answer for "would a hot reload do anything?".
pub fn view_digest(files: &[(String, Vec<u8>)]) -> Option<String> {
    let mut lines = Vec::new();
    for (name, bytes) in files {
        if name.starts_with("ui/") && name.ends_with(".lua") {
            lines.push(format!("{} {}", crate::hex(&Sha256::digest(bytes)), name));
        }
    }
    lines.sort();
    if lines.is_empty() {
        return None;
    }
    Some(crate::hex(&Sha256::digest(lines.join("\n").as_bytes())))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub spore_id: String,
    pub from: String,
    pub to: String,
    pub digest: String,
    pub class: Class,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub updates: Vec<Update>,
    /// Installed and already on the catalog's digest.
    pub current: Vec<String>,
    /// Installed but no longer listed by the active source.
    pub yanked: Vec<String>,
}

impl Plan {
    pub fn pending(&self) -> usize {
        self.updates.len()
    }

    pub fn count(&self, class: Class) -> usize {
        self.updates.iter().filter(|u| u.class == class).count()
    }
}

/// Decide what every installed Spore should do about the catalog.
pub fn plan(installed: &[Installed], catalog: &BTreeMap<String, Entry>) -> Plan {
    let mut plan = Plan::default();
    let mut seen = BTreeSet::new();
    for app in installed {
        seen.insert(app.id());
        let Some(entry) = catalog.get(app.id()) else {
            plan.yanked.push(app.id().to_owned());
            continue;
        };
        if entry.digest == app.digest {
            plan.current.push(app.id().to_owned());
            continue;
        }
        let (class, reason) = classify(app, entry);
        plan.updates.push(Update {
            spore_id: app.id().to_owned(),
            from: app.manifest.spore.version.clone(),
            to: entry.version.clone(),
            digest: entry.digest.clone(),
            class,
            reason,
        });
    }
    plan.updates.sort_by(|a, b| a.spore_id.cmp(&b.spore_id));
    plan.current.sort();
    plan.yanked.sort();
    plan
}

fn classify(app: &Installed, entry: &Entry) -> (Class, String) {
    if let Some(pin) = &app.pinned {
        return (Class::Hold, format!("pinned by {pin}"));
    }
    if compare_versions(&entry.version, &app.manifest.spore.version).is_lt() {
        return (
            Class::Hold,
            format!(
                "source offers an older version than the installed {}",
                app.manifest.spore.version
            ),
        );
    }

    let role = app
        .manifest
        .system
        .as_ref()
        .map(|system| system.role.as_str());
    if role != entry.system.as_deref() {
        return (Class::System, system_reason(role, entry.system.as_deref()));
    }
    if let (Some(have), Some(want)) = (app.publisher.as_deref(), entry.publisher.as_deref())
        && have != want
    {
        return (
            Class::System,
            format!("publisher changes from {have} to {want}"),
        );
    }

    let held: BTreeSet<&str> = app.manifest.grants.keys().map(String::as_str).collect();
    let added: Vec<&str> = entry
        .grants
        .iter()
        .map(String::as_str)
        .filter(|grant| !held.contains(grant))
        .collect();
    if !added.is_empty() {
        return (Class::Consent, format!("asks for {}", added.join(", ")));
    }

    if app.module.is_some() && app.module == entry.module && app.ui.is_some() && app.ui != entry.ui
    {
        return (
            Class::Live,
            "view-only change; Canvas hot-reloads it".into(),
        );
    }

    (
        Class::Restart,
        "module or assets changed; reopens at the next launch".into(),
    )
}

fn system_reason(have: Option<&str>, want: Option<&str>) -> String {
    match (have, want) {
        (None, Some(role)) => format!("becomes a system surface ({role}); needs a reboot"),
        (Some(role), None) => format!("stops being a system surface ({role}); needs a reboot"),
        (Some(have), Some(want)) => {
            format!("system role changes from {have} to {want}; needs a reboot")
        }
        (None, None) => "system surface changed; needs a reboot".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{EntryKind, Index};

    const CID: &str = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";

    fn manifest(version: &str, grants: &[&str], role: Option<&str>) -> Manifest {
        let mut source = format!(
            "schema = 1\n[spore]\nid = \"dev.fungos.demo\"\nname = \"demo\"\nversion = \"{version}\"\nlicense = \"MIT\"\nsummary = \"demo\"\n[engine]\nsoil = \">=0.1\"\ntarget = \"wasm32-wasip2\"\n[module]\npath = \"module.wasm\"\n[ui]\nentry = \"ui/demo.lua\"\n[launch]\ndefault = \"canvas\"\nsurfaces = [\"canvas\"]\n[grants]\n"
        );
        for grant in grants {
            source.push_str(&format!("\"{grant}\" = \"reason\"\n"));
        }
        if let Some(role) = role {
            source.push_str(&format!("[system]\nrole = \"{role}\"\n"));
        }
        Manifest::parse(&source).expect("test manifest")
    }

    fn installed(
        version: &str,
        digest: &str,
        module: &str,
        ui: &str,
        grants: &[&str],
    ) -> Installed {
        Installed {
            manifest: manifest(version, grants, None),
            root: PathBuf::new(),
            digest: digest.to_owned(),
            module: Some(module.to_owned()),
            ui: Some(ui.to_owned()),
            publisher: Some("fungos".into()),
            pinned: None,
        }
    }

    fn entry(version: &str, digest: &str, module: &str, ui: &str, grants: &[&str]) -> Entry {
        Entry {
            seq: 1,
            kind: EntryKind::Update,
            spore_id: "dev.fungos.demo".into(),
            version: version.to_owned(),
            digest: digest.to_owned(),
            package: CID.into(),
            listing: None,
            module: Some(module.to_owned()),
            ui: Some(ui.to_owned()),
            grants: grants.iter().map(|grant| (*grant).to_owned()).collect(),
            system: None,
            publisher: Some("fungos".into()),
            signature: String::new(),
        }
    }

    fn catalog(entry: Entry) -> BTreeMap<String, Entry> {
        let mut catalog = BTreeMap::new();
        catalog.insert(entry.spore_id.clone(), entry);
        catalog
    }

    const D0: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const D1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const G0: &str = "aaaa";
    const G1: &str = "bbbb";

    #[test]
    fn a_view_only_change_is_live() {
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(entry("0.1.1", D1, "m", G1, &["soil:canvas"])),
        );
        assert_eq!(plan.updates[0].class, Class::Live);
        assert!(plan.updates[0].class.automatic());
    }

    #[test]
    fn a_rebuilt_module_is_a_restart() {
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(entry("0.2.0", D1, "m2", G1, &["soil:canvas"])),
        );
        assert_eq!(plan.updates[0].class, Class::Restart);
    }

    #[test]
    fn a_new_grant_is_consent_even_when_the_module_changed() {
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(entry("0.2.0", D1, "m2", G1, &["soil:canvas", "soil:http"])),
        );
        assert_eq!(plan.updates[0].class, Class::Consent);
        assert!(plan.updates[0].reason.contains("soil:http"));
        assert!(!plan.updates[0].class.automatic());
    }

    #[test]
    fn a_system_role_beats_a_grant_change() {
        let mut next = entry("0.2.0", D1, "m2", G1, &["soil:canvas", "soil:http"]);
        next.system = Some("store".into());
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(next),
        );
        assert_eq!(plan.updates[0].class, Class::System);
        assert!(plan.updates[0].reason.contains("store"));
    }

    #[test]
    fn a_publisher_change_is_a_system_update() {
        let mut next = entry("0.1.1", D1, "m", G1, &["soil:canvas"]);
        next.publisher = Some("someone-else".into());
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(next),
        );
        assert_eq!(plan.updates[0].class, Class::System);
    }

    #[test]
    fn a_downgrade_or_a_pin_is_held() {
        let older = super::plan(
            &[installed("0.2.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(entry("0.1.0", D1, "m", G1, &["soil:canvas"])),
        );
        assert_eq!(older.updates[0].class, Class::Hold);

        let mut pinned = installed("0.1.0", D0, "m", G0, &["soil:canvas"]);
        pinned.pinned = Some("fleet certified-2026".into());
        let held = super::plan(
            &[pinned],
            &catalog(entry("0.2.0", D1, "m2", G1, &["soil:canvas"])),
        );
        assert_eq!(held.updates[0].class, Class::Hold);
        assert!(held.updates[0].reason.contains("certified-2026"));
    }

    #[test]
    fn the_same_digest_is_current_and_a_missing_entry_is_yanked() {
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &catalog(entry("0.1.0", D0, "m", G0, &["soil:canvas"])),
        );
        assert!(plan.updates.is_empty());
        assert_eq!(plan.current, vec!["dev.fungos.demo"]);

        let yanked = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &BTreeMap::new(),
        );
        assert_eq!(yanked.yanked, vec!["dev.fungos.demo"]);
    }

    #[test]
    fn versions_compare_numerically_not_lexically() {
        assert!(compare_versions("0.2.14", "0.2.4").is_gt());
        assert!(compare_versions("0.1.0", "0.1.0").is_eq());
        assert!(compare_versions("0.1.0-rc1", "0.1.0").is_eq());
    }

    #[test]
    fn the_view_digest_ignores_what_is_not_a_view() {
        let files = vec![
            ("ui/a.lua".to_owned(), b"return {}".to_vec()),
            ("module.wasm".to_owned(), b"wasm".to_vec()),
            ("ui/API.md".to_owned(), b"docs".to_vec()),
        ];
        let first = view_digest(&files).expect("a view exists");
        let mut changed = files.clone();
        changed[0].1 = b"return { changed = true }".to_vec();
        assert_ne!(first, view_digest(&changed).unwrap());
        let mut docs = files;
        docs[2].1 = b"more docs".to_vec();
        assert_eq!(first, view_digest(&docs).unwrap());
    }

    #[test]
    fn a_catalog_entry_is_enough_to_plan() {
        let index = Index::parse(&format!(
            r#"{{"schema":1,"source":"test","checkpoint":{{"seq":1,"root":"r","signature":"s"}},"entries":[{{"seq":1,"kind":"add","spore_id":"dev.fungos.demo","version":"0.1.1","digest":"{D1}","package":"{CID}","module":"m","ui":"{G1}","grants":["soil:canvas"],"publisher":"fungos"}}]}}"#
        ))
        .expect("index");
        let plan = super::plan(
            &[installed("0.1.0", D0, "m", G0, &["soil:canvas"])],
            &index.catalog(),
        );
        assert_eq!(plan.updates.len(), 1);
        assert_eq!(plan.updates[0].to, "0.1.1");
    }
}

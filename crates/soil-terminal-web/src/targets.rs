//! What the picker offers: local shells, ssh hosts, and Mush desktops.
//!
//! A target is a name, a label, and the argv to put on a pty. The catalog comes
//! from `~/.config/fungos/terminal/targets.toml` when that file exists;
//! otherwise it is discovered from the machine: the login shells that exist,
//! the `Host` aliases in `~/.ssh/config`, the peers `mycelium peers --json`
//! reports (the fleet this host can reach), and the desktops in
//! `~/.config/fungos/desktops/` (when a Mush binary is available).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Shell,
    Ssh,
    /// A peer discovered through Mycelium, opened over ssh.
    Fleet,
    Mush,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Shell => "shell",
            Kind::Ssh => "ssh",
            Kind::Fleet => "fleet",
            Kind::Mush => "mush",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "shell" => Some(Kind::Shell),
            "ssh" => Some(Kind::Ssh),
            "mush" => Some(Kind::Mush),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Target {
    pub name: String,
    pub label: String,
    pub kind: Kind,
    /// One line the picker can show under the label.
    pub detail: String,
    /// The program and arguments the pty runs.
    pub command: Vec<String>,
}

pub fn load(explicit: Option<&Path>) -> Result<Vec<Target>> {
    match explicit {
        Some(path) => from_file(path),
        None => {
            let path = config_path();
            if path.is_file() {
                from_file(&path)
            } else {
                discover()
            }
        }
    }
}

fn config_path() -> PathBuf {
    home()
        .map(|home| home.join(".config/fungos/terminal/targets.toml"))
        .unwrap_or_else(|| PathBuf::from("targets.toml"))
}

fn from_file(path: &Path) -> Result<Vec<Target>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read targets {}", path.display()))?;
    let (mut targets, fleet) =
        parse_config(&text).with_context(|| format!("parse targets {}", path.display()))?;
    if targets.is_empty() {
        anyhow::bail!("{} defines no [[target]] entries", path.display());
    }
    if fleet {
        append_fleet(&mut targets);
    }
    targets.shrink_to_fit();
    Ok(targets)
}

/// The fleet is dynamic, so it is appended even when a config file pins the
/// rest of the catalog; `[mycelium] enabled = false` turns it off.
fn append_fleet(targets: &mut Vec<Target>) {
    let seen: BTreeSet<String> = targets.iter().map(|target| target.name.clone()).collect();
    for target in fleet_targets(&seen) {
        if !targets.iter().any(|existing| existing.name == target.name) {
            targets.push(target);
        }
    }
}

#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    mush: MushConfig,
    #[serde(default)]
    mycelium: MyceliumConfig,
    #[serde(default)]
    target: Vec<Entry>,
}

#[derive(Deserialize)]
struct MyceliumConfig {
    #[serde(default = "enabled")]
    enabled: bool,
}

impl Default for MyceliumConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct MushConfig {
    #[serde(default = "default_mush_binary")]
    binary: String,
    #[serde(default)]
    wasm: Option<String>,
}

impl Default for MushConfig {
    fn default() -> Self {
        Self {
            binary: default_mush_binary(),
            wasm: None,
        }
    }
}

fn default_mush_binary() -> String {
    "target/debug/mush".into()
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    #[serde(default)]
    label: Option<String>,
    kind: String,
    #[serde(default)]
    detail: Option<String>,
    /// `shell`: the argv to run. Defaults to `$SHELL`.
    #[serde(default)]
    command: Vec<String>,
    /// `ssh`: the host or `~/.ssh/config` alias.
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    /// `mush`: a Desktop registry id, or a bare VFS root.
    #[serde(default)]
    desktop: Option<String>,
    #[serde(default)]
    root: Option<String>,
    /// Development override for the embedded Spore.
    #[serde(default)]
    wasm: Option<String>,
}

pub fn parse(text: &str) -> Result<Vec<Target>> {
    Ok(parse_config(text)?.0)
}

fn parse_config(text: &str) -> Result<(Vec<Target>, bool)> {
    let config: Config = toml::from_str(text)?;
    let mut targets = Vec::with_capacity(config.target.len());
    let mut seen = BTreeSet::new();
    for entry in config.target {
        let target = entry.resolve(&config.mush)?;
        if !seen.insert(target.name.clone()) {
            anyhow::bail!("duplicate target name {:?}", target.name);
        }
        targets.push(target);
    }
    Ok((targets, config.mycelium.enabled))
}

impl Entry {
    fn resolve(self, mush: &MushConfig) -> Result<Target> {
        let kind = Kind::parse(&self.kind)
            .with_context(|| format!("target {:?}: unknown kind {:?}", self.name, self.kind))?;
        let label = self.label.unwrap_or_else(|| self.name.clone());
        let (command, detail) = match kind {
            Kind::Shell => {
                let command = if self.command.is_empty() {
                    vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
                } else {
                    self.command
                };
                let detail = self.detail.unwrap_or_else(|| command.join(" "));
                (command, detail)
            }
            Kind::Ssh => {
                let host = self
                    .host
                    .with_context(|| format!("target {:?}: kind ssh needs host", self.name))?;
                let mut command = vec!["ssh".to_string(), "-tt".to_string()];
                command.extend(self.args);
                command.push(host.clone());
                let detail = self.detail.unwrap_or_else(|| format!("ssh {host}"));
                (command, detail)
            }
            Kind::Fleet => {
                anyhow::bail!(
                    "target {:?}: fleet targets come from `mycelium peers`, not the config",
                    self.name
                );
            }
            Kind::Mush => {
                let desktop = self.desktop;
                let root = self.root.map(|root| expand_home(&root));
                if desktop.is_none() && root.is_none() {
                    anyhow::bail!("target {:?}: kind mush needs desktop or root", self.name);
                }
                let mut command = vec![mush.binary.clone()];
                if let Some(desktop) = &desktop {
                    command.push("--desktop".into());
                    command.push(desktop.clone());
                }
                if let Some(root) = &root {
                    command.push("--root".into());
                    command.push(root.display().to_string());
                }
                // The Spore is embedded in the host; --wasm is a development
                // override, and a stale module fails its type check.
                if let Some(wasm) = self.wasm.or_else(|| mush.wasm.clone()) {
                    command.push("--wasm".into());
                    command.push(expand_home(&wasm).display().to_string());
                }
                let what = desktop
                    .map(|desktop| format!("desktop {desktop}"))
                    .unwrap_or_else(|| {
                        root.as_ref()
                            .map(|root| root.display().to_string())
                            .unwrap_or_default()
                    });
                let detail = self.detail.unwrap_or_else(|| format!("mush · {what}"));
                (command, detail)
            }
        };
        if command.first().is_none_or(|program| program.is_empty()) {
            anyhow::bail!("target {:?}: empty command", self.name);
        }
        Ok(Target {
            name: self.name,
            label,
            kind,
            detail,
            command,
        })
    }
}

fn discover() -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();

    let mut shells = Vec::new();
    if let Some(shell) = std::env::var_os("SHELL") {
        shells.push(PathBuf::from(shell));
    }
    for candidate in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        shells.push(PathBuf::from(candidate));
    }
    for path in shells {
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !seen.insert(name.to_owned()) {
            continue;
        }
        targets.push(Target {
            name: name.to_owned(),
            label: name.to_owned(),
            kind: Kind::Shell,
            detail: path.display().to_string(),
            command: vec![path.display().to_string()],
        });
    }

    if let Some(home) = home() {
        for alias in ssh_hosts(&home.join(".ssh/config")) {
            if !seen.insert(alias.clone()) {
                continue;
            }
            targets.push(Target {
                label: alias.clone(),
                name: alias.clone(),
                kind: Kind::Ssh,
                detail: format!("ssh {alias}"),
                command: vec!["ssh".into(), "-tt".into(), alias],
            });
        }
        for target in fleet_targets(&seen) {
            if !seen.insert(target.name.clone()) {
                continue;
            }
            targets.push(target);
        }
        for target in desktop_targets(&home) {
            if !seen.insert(target.name.clone()) {
                continue;
            }
            targets.push(target);
        }
    }
    Ok(targets)
}

/// `Host` aliases from an ssh config. Patterns are skipped: a picker cannot open
/// `*`.
fn ssh_hosts(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line
            .strip_prefix("Host ")
            .or_else(|| line.strip_prefix("Host\t"))
        else {
            continue;
        };
        for alias in rest.split_whitespace() {
            if alias.starts_with('!') || alias.contains('*') || alias.contains('?') {
                continue;
            }
            hosts.push(alias.to_owned());
        }
    }
    hosts
}

#[derive(Deserialize)]
struct Peer {
    #[serde(default)]
    hello: Option<PeerHello>,
    #[serde(default)]
    hardware: Option<PeerHardware>,
    #[serde(default)]
    last_seen: u64,
    #[serde(default)]
    observed_endpoints: Vec<PeerEndpoint>,
}

#[derive(Deserialize)]
struct PeerHello {
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Deserialize)]
struct PeerHardware {
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Deserialize)]
struct PeerEndpoint {
    address: String,
}

/// The fleet the host can reach: every Mycelium peer with a hostname and an
/// address, freshest first. A peer already named in `~/.ssh/config` is left to
/// that curated alias.
fn fleet_targets(seen: &BTreeSet<String>) -> Vec<Target> {
    let Some(json) = mycelium_peers() else {
        return Vec::new();
    };
    peers_to_targets(&json, now_unix(), seen)
}

fn peers_to_targets(json: &str, now: u64, seen: &BTreeSet<String>) -> Vec<Target> {
    let Ok(peers) = serde_json::from_str::<Vec<Peer>>(json) else {
        return Vec::new();
    };
    let mut best: BTreeMap<String, (u64, String, String)> = BTreeMap::new();
    for peer in peers {
        let host = peer
            .hello
            .and_then(|hello| hello.hostname)
            .or_else(|| peer.hardware.and_then(|hardware| hardware.hostname));
        let Some(host) = host.filter(|host| !host.is_empty()) else {
            continue;
        };
        let Some(address) = pick_address(&peer.observed_endpoints) else {
            continue;
        };
        if seen.iter().any(|name| name.eq_ignore_ascii_case(&host)) {
            continue;
        }
        let key = host.to_ascii_lowercase();
        match best.get_mut(&key) {
            Some(entry) if peer.last_seen <= entry.0 => {}
            Some(entry) => *entry = (peer.last_seen, host, address),
            None => {
                best.insert(key, (peer.last_seen, host, address));
            }
        }
    }
    let mut entries: Vec<_> = best.into_values().collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    entries
        .into_iter()
        .map(|(last_seen, host, address)| Target {
            name: format!("fleet:{host}"),
            label: host,
            kind: Kind::Fleet,
            detail: format!("mycelium · {address} · {}", age(now, last_seen)),
            command: vec!["ssh".into(), "-tt".into(), address],
        })
        .collect()
}

/// A mesh address beats a LAN address: it works from wherever the bastion is.
fn pick_address(endpoints: &[PeerEndpoint]) -> Option<String> {
    endpoints
        .iter()
        .map(|endpoint| endpoint.address.trim())
        .find(|address| address.starts_with("100."))
        .or_else(|| {
            endpoints
                .iter()
                .map(|endpoint| endpoint.address.trim())
                .find(|address| !address.is_empty())
        })
        .map(str::to_owned)
}

fn age(now: u64, last_seen: u64) -> String {
    let secs = now.saturating_sub(last_seen);
    match secs {
        0..=119 => "now".into(),
        120..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// `mycelium peers --json`, bounded: a wedged daemon must not hold up the
/// listener. The child is killed at the deadline and the reader is joined so no
/// thread is left behind.
fn mycelium_peers() -> Option<String> {
    let binary = std::env::var_os("MYCELIUM_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            let local = home()?.join(".mycelium/bin/mycelium");
            local.is_file().then_some(local)
        })
        .unwrap_or_else(|| PathBuf::from("mycelium"));
    let mut child = Command::new(binary)
        .arg("peers")
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                let _ = reader.join();
                return None;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => {
                let _ = child.kill();
                let _ = reader.join();
                return None;
            }
        }
    }
    reader.join().ok()
}

#[derive(Deserialize)]
struct Desktop {
    id: String,
    #[serde(default)]
    name: Option<String>,
    root: String,
}

fn desktop_targets(home: &Path) -> Vec<Target> {
    let Some(binary) = mush_binary() else {
        return Vec::new();
    };
    let dir = home.join(".config/fungos/desktops");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(desktop) = toml::from_str::<Desktop>(&text) else {
            continue;
        };
        let root = expand_home(&desktop.root);
        if !root.is_dir() {
            continue;
        }
        let label = desktop.name.unwrap_or_else(|| desktop.id.clone());
        targets.push(Target {
            name: desktop.id.clone(),
            label: format!("{label} desktop"),
            kind: Kind::Mush,
            detail: format!("mush · desktop {}", desktop.id),
            command: vec![binary.clone(), "--desktop".into(), desktop.id],
        });
    }
    targets.sort_by(|a, b| a.label.cmp(&b.label));
    targets
}

fn mush_binary() -> Option<String> {
    if let Some(binary) = std::env::var_os("SOIL_MUSH_BIN") {
        return Some(binary.to_string_lossy().into_owned());
    }
    let local = PathBuf::from("target/debug/mush");
    if local.is_file() {
        return Some(local.display().to_string());
    }
    on_path("mush").then(|| "mush".to_owned())
}

/// Whether a target's program can be spawned. Checked before the pty so a
/// missing binary is one line, not portable-pty's dump of `PATH`.
pub fn program_exists(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    on_path(program)
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return home().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_three_kinds() {
        let targets = parse(
            r#"
            [mush]
            binary = "/opt/mush"
            wasm = "/opt/mush.wasm"

            [[target]]
            name = "studio"
            kind = "shell"
            command = ["zsh", "-l"]

            [[target]]
            name = "neo"
            kind = "ssh"
            host = "averys-macbook-neo"
            args = ["-p", "2222"]

            [[target]]
            name = "acme"
            label = "Acme desktop"
            kind = "mush"
            desktop = "acme"
            "#,
        )
        .unwrap();
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].command, ["zsh", "-l"]);
        assert_eq!(targets[0].kind, Kind::Shell);
        assert_eq!(
            targets[1].command,
            ["ssh", "-tt", "-p", "2222", "averys-macbook-neo"]
        );
        assert_eq!(targets[1].label, "neo");
        assert_eq!(
            targets[2].command,
            ["/opt/mush", "--desktop", "acme", "--wasm", "/opt/mush.wasm"]
        );
        assert_eq!(targets[2].label, "Acme desktop");
    }

    #[test]
    fn a_mush_root_and_wasm_override_are_passed_through() {
        let targets = parse(
            r#"
            [mush]
            binary = "/opt/mush"
            wasm = "/opt/mush.wasm"

            [[target]]
            name = "bare"
            kind = "mush"
            root = "/desktops/bare"
            "#,
        )
        .unwrap();
        assert_eq!(
            targets[0].command,
            [
                "/opt/mush",
                "--root",
                "/desktops/bare",
                "--wasm",
                "/opt/mush.wasm"
            ]
        );
    }

    #[test]
    fn a_shell_defaults_to_the_login_shell() {
        let targets = parse("[[target]]\nname = \"sh\"\nkind = \"shell\"\n").unwrap();
        assert_eq!(targets[0].command.len(), 1);
    }

    #[test]
    fn names_must_be_unique() {
        let error = parse(
            r#"
            [[target]]
            name = "a"
            kind = "shell"
            [[target]]
            name = "a"
            kind = "shell"
            "#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("duplicate"), "{error}");
    }

    #[test]
    fn ssh_needs_a_host_and_mush_needs_a_root() {
        assert!(parse("[[target]]\nname = \"a\"\nkind = \"ssh\"\n").is_err());
        assert!(parse("[[target]]\nname = \"a\"\nkind = \"mush\"\n").is_err());
    }

    #[test]
    fn fleet_peers_are_deduped_and_prefer_the_mesh_address() {
        let json = r#"[
          {"hello":{"hostname":"neo"},"last_seen":100,
           "observed_endpoints":[{"address":"192.168.1.5"}]},
          {"hello":{"hostname":"neo"},"last_seen":200,
           "observed_endpoints":[{"address":"192.168.1.5"},{"address":"100.64.0.5"}]},
          {"hardware":{"hostname":"batty"},"last_seen":50,
           "observed_endpoints":[{"address":"100.64.0.6"}]},
          {"hello":{},"last_seen":1,"observed_endpoints":[]}
        ]"#;
        let targets = peers_to_targets(json, 300, &BTreeSet::new());
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].label, "neo");
        assert_eq!(targets[0].kind, Kind::Fleet);
        assert_eq!(targets[0].command, ["ssh", "-tt", "100.64.0.5"]);
        assert!(targets[0].detail.contains("now"), "{}", targets[0].detail);
        assert_eq!(targets[1].label, "batty");
        assert!(
            targets[1].detail.contains("4m ago"),
            "{}",
            targets[1].detail
        );
    }

    #[test]
    fn a_curated_ssh_alias_wins_over_a_fleet_peer() {
        let json = r#"[
          {"hello":{"hostname":"neo"},"last_seen":200,
           "observed_endpoints":[{"address":"100.64.0.5"}]}
        ]"#;
        let seen = BTreeSet::from(["neo".to_string()]);
        assert!(peers_to_targets(json, 300, &seen).is_empty());
    }

    #[test]
    fn ssh_hosts_skip_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(
            &path,
            "Host neo\n  HostName neo.lan\nHost agora-one batty\nHost *\n  ForwardAgent yes\nHost !bad ?x\n",
        )
        .unwrap();
        assert_eq!(ssh_hosts(&path), ["neo", "agora-one", "batty"]);
    }
}

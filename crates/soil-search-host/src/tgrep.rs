//! Optional Microsoft `tgrep` client over an explicitly granted root.
//!
//! The root may be a native checkout or a `fungos-vfs` FUSE mount. The host
//! owns the binary path and root; a Spore never chooses either. Searches use
//! tgrep's ripgrep-compatible JSON stream and never invoke a shell.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct TgrepHit {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub text: String,
}

pub struct TgrepSource {
    root: PathBuf,
    binary: PathBuf,
}

impl TgrepSource {
    pub fn new(root: impl Into<PathBuf>, binary: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            binary: binary.into(),
        }
    }

    pub fn search(&self, pattern: &str, limit: usize) -> anyhow::Result<Vec<TgrepHit>> {
        let output = Command::new(&self.binary)
            .args(["--json", "--", pattern])
            .arg(&self.root)
            .output()
            .map_err(|error| anyhow::anyhow!("run tgrep: {error}"))?;
        match output.status.code() {
            Some(0 | 1) => {}
            _ => anyhow::bail!(
                "tgrep failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        let mut hits = Vec::new();
        for line in output.stdout.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(serde_json::Value::as_str) != Some("match") {
                continue;
            }
            let Some(data) = value.get("data") else {
                continue;
            };
            let Some(path) = data
                .pointer("/path/text")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            let path = relative_path(&self.root, path);
            let line_number = data
                .get("line_number")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let column = data
                .pointer("/submatches/0/start")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
                + 1;
            let text = data
                .pointer("/lines/text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            hits.push(TgrepHit {
                path,
                line: line_number,
                column,
                text,
            });
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }
}

fn relative_path(root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ripgrep_json_and_ignores_begin_end() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("tgrep");
        std::fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"begin\",\"data\":{\"path\":{\"text\":\"/repo/src/main.rs\"}}}'\nprintf '%s\\n' '{\"type\":\"match\",\"data\":{\"path\":{\"text\":\"/repo/src/main.rs\"},\"line_number\":7,\"submatches\":[{\"start\":4}],\"lines\":{\"text\":\"fn main() {\\n\"}}}'\nprintf '%s\\n' '{\"type\":\"end\",\"data\":{}}'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let source = TgrepSource::new("/repo", &binary);
        let hits = source.search("fn main", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/main.rs");
        assert_eq!(hits[0].line, 7);
        assert_eq!(hits[0].column, 5);
        assert_eq!(hits[0].text, "fn main() {");
    }
}

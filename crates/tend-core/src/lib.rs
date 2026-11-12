//! Compose FungOS Canvas Lua modules.
//!
//! Canvas strips `load`/`require`/`package` from its sandbox, so a Spore's view,
//! its shared components and the SDK modules it needs are pre-composed into one
//! source before anything is installed. This is that step, and only that step:
//! files in, text out.
//!
//! Deliberately host-free. No clock, no network, no environment, no writes, no
//! process spawning - which is what lets the same code run natively and inside
//! `mush` as a WASM component, and what makes every test a comparison of bytes
//! rather than a rehearsal of a machine.
//!
//! The Lua original lives in `fungos-sdk/tools/tend.lua`. This is a port, not a
//! reimagining: `strip_comments` keeps its exact behaviour because composed
//! output is compared byte for byte in the golden tests.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

/// Canvas caps a module source at 32 KiB.
pub const MAX_MODULE_BYTES: usize = 32 * 1024;

#[derive(Debug)]
pub enum Error {
    /// The workspace manifest is not the shape `tend.lua` describes.
    Manifest(String),
    /// A composed module would not load.
    Compose(String),
    /// The result is too large to install.
    TooLarge { bytes: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(message) => write!(f, "workspace manifest: {message}"),
            Self::Compose(message) => write!(f, "compose: {message}"),
            Self::TooLarge { bytes } => write!(
                f,
                "composed module is {bytes} bytes; Canvas loads at most {MAX_MODULE_BYTES}"
            ),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// `tend.lua` at a workspace root: which trees hold Spores, and which files are
/// shared components.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workspace {
    pub members: Vec<String>,
    pub components: Vec<String>,
}

impl Workspace {
    /// Parse the Lua table literal the manifest is written as.
    ///
    /// `return { members = { "spores/*" }, components = { "sdk/ui/*.lua" } }`.
    ///
    /// This is a reader for one shape, not a Lua interpreter: unknown keys are
    /// refused rather than ignored, because a manifest that grows a key the
    /// tool does not honour is a build that silently does less than it says.
    pub fn parse(source: &str) -> Result<Self> {
        let body = source
            .split_once("return")
            .map(|(_, rest)| rest)
            .ok_or_else(|| Error::Manifest("expected a `return { … }` table".into()))?;
        let mut workspace = Self::default();
        let mut rest = body;
        while let Some(key_at) = next_key(rest) {
            let (key, after) = key_at;
            let list = string_list(after)
                .ok_or_else(|| Error::Manifest(format!("`{key}` must be a list of strings")))?;
            match key.as_str() {
                "members" => workspace.members = list.0,
                "components" => workspace.components = list.0,
                other => {
                    return Err(Error::Manifest(format!(
                        "unknown key `{other}`; a manifest key this tool ignores is a build that lies"
                    )));
                }
            }
            rest = list.1;
        }
        if workspace.members.is_empty() {
            return Err(Error::Manifest("`members` is empty".into()));
        }
        Ok(workspace)
    }

    /// Every Spore directory under the declared member globs.
    ///
    /// Only the `dir/*` form is supported: it is the only form this workspace
    /// uses, and a second form would need a glob engine to stay honest.
    pub fn spore_dirs(&self, root: &std::path::Path) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        for member in &self.members {
            let (parent, _) = member.split_once("/*").ok_or_else(|| {
                Error::Manifest(format!("member `{member}` is not a `<dir>/*` glob"))
            })?;
            let base = root.join(parent);
            let entries = std::fs::read_dir(&base)
                .map_err(|error| Error::Manifest(format!("{}: {error}", base.display())))?;
            for entry in entries {
                let path = entry
                    .map_err(|error| Error::Manifest(error.to_string()))?
                    .path();
                if path.join("spore.toml").is_file() {
                    dirs.push(path);
                }
            }
        }
        dirs.sort();
        Ok(dirs)
    }
}

/// The next `key = ` in a table body, and the text after it.
fn next_key(source: &str) -> Option<(String, &str)> {
    let mut rest = source;
    loop {
        let equals = rest.find("=")?;
        let candidate = rest[..equals].trim();
        let key = candidate.rsplit(['{', ',', '\n']).next()?.trim();
        if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Some((key.to_owned(), &rest[equals + 1..]));
        }
        rest = &rest[equals + 1..];
    }
}

/// `{ "a", "b" }` after a key, and the text after the list.
fn string_list(source: &str) -> Option<(Vec<String>, &str)> {
    let open = source.find('{')?;
    let close = source[open..].find('}')? + open;
    let body = &source[open + 1..close];
    let mut items = Vec::new();
    for item in body.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let quote = item.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let inner = item.strip_prefix(quote)?.strip_suffix(quote)?;
        items.push(inner.to_owned());
    }
    Some((items, &source[close + 1..]))
}

/// Strip `--` line comments and blank lines, and trim each surviving line.
///
/// String literals and their escapes are respected, so `"--"` survives. Long
/// comments (`--[[ ]]`) are left alone: the sources this composes do not use
/// them, and pretending to handle them would be a parser that lies.
pub fn strip_comments(source: &str) -> String {
    let bytes: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut index = 0;
    let mut quote: Option<char> = None;
    while index < bytes.len() {
        let character = bytes[index];
        if let Some(open) = quote {
            out.push(character);
            if character == '\\' {
                index += 1;
                if index < bytes.len() {
                    out.push(bytes[index]);
                }
            } else if character == open {
                quote = None;
            }
            index += 1;
            continue;
        }
        if character == '"' || character == '\'' {
            quote = Some(character);
            out.push(character);
            index += 1;
            continue;
        }
        if character == '-' && bytes.get(index + 1) == Some(&'-') {
            while index < bytes.len() && bytes[index] != '\n' {
                index += 1;
            }
            continue;
        }
        out.push(character);
        index += 1;
    }
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One module as the preloaded table wants it: `["name"] = function(require) … end`.
///
/// Sorted by name so two runs over the same sources produce the same bytes.
/// `BTreeMap` is the whole reason the golden tests can compare text.
pub fn loaders(modules: &BTreeMap<String, String>) -> String {
    modules
        .iter()
        .map(|(name, source)| {
            format!(
                "[{name:?}] = function(require)\n{}\nend",
                strip_comments(source)
            )
        })
        .collect::<Vec<_>>()
        .join(",\n")
}

/// Compose one Spore's Canvas module from its sources and the shared components.
///
/// Order is the caller's: view, then components, then the SDK modules it
/// requires. Nothing here discovers requirements - that is tend's job, and
/// putting a `require` scanner in the core would make the golden output depend
/// on whichever scanner ran.
pub fn compose(parts: &[String]) -> Result<String> {
    let module = parts
        .iter()
        .map(|part| strip_comments(part))
        .collect::<Vec<_>>()
        .join("\n");
    if module.len() > MAX_MODULE_BYTES {
        return Err(Error::TooLarge {
            bytes: module.len(),
        });
    }
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_go_and_strings_stay() {
        let source = "local a = 1 -- trailing\n-- whole line\nlocal s = \"-- not a comment\"\n\nlocal t = 'a\\'--b'\n";
        assert_eq!(
            strip_comments(source),
            "local a = 1\nlocal s = \"-- not a comment\"\nlocal t = 'a\\'--b'"
        );
    }

    #[test]
    fn blank_lines_and_indentation_are_normalised() {
        assert_eq!(strip_comments("\n\n  x = 1  \n\n\ty = 2\n"), "x = 1\ny = 2");
    }

    #[test]
    fn loaders_are_sorted_so_two_runs_match() {
        let mut modules = BTreeMap::new();
        modules.insert("fungos.ui".to_string(), "return { a = 1 }".to_string());
        modules.insert("fungos.compose".to_string(), "return { b = 2 }".to_string());
        let text = loaders(&modules);
        assert!(
            text.starts_with("[\"fungos.compose\"] = function(require)"),
            "{text}"
        );
        assert!(
            text.contains("[\"fungos.ui\"] = function(require)\nreturn { a = 1 }\nend"),
            "{text}"
        );
        assert_eq!(text.matches("= function(require)").count(), 2);
    }

    #[test]
    fn a_workspace_manifest_parses() {
        let workspace = Workspace::parse(
            "return {\n  members = { \"spores/*\" },\n  components = { \"sdk/ui/*.lua\", \"lua/fungos/*.lua\" },\n}\n",
        )
        .expect("manifest");
        assert_eq!(workspace.members, vec!["spores/*"]);
        assert_eq!(
            workspace.components,
            vec!["sdk/ui/*.lua", "lua/fungos/*.lua"]
        );
    }

    #[test]
    fn a_key_the_tool_would_ignore_is_refused() {
        let refused = Workspace::parse("return { members = { \"spores/*\" }, agent = {} }");
        assert!(matches!(refused, Err(Error::Manifest(_))), "{refused:?}");
    }

    #[test]
    fn a_manifest_without_members_is_refused() {
        assert!(Workspace::parse("return { components = {} }").is_err());
    }

    #[test]
    fn a_module_past_the_canvas_ceiling_is_refused() {
        let big = "x".repeat(MAX_MODULE_BYTES + 1);
        assert!(matches!(compose(&[big]), Err(Error::TooLarge { .. })));
    }
}

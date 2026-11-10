//! The generated manual page.
//!
//! A Spore already declares its name, version, licence, launch surfaces and
//! grants. Those are the parts of a man page that must never drift from the
//! package, so they are rendered from the manifest rather than retyped. Only
//! the synopsis and the prose are authored, and the prose ships inside the
//! package so `man` works with no network.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::{Error, Manifest};

/// A man page assembled from one package.
pub struct Manual {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    pub summary: String,
    pub description: Option<String>,
    pub usage: Vec<String>,
    pub prose: Option<String>,
    pub launch_default: String,
    pub surfaces: Vec<String>,
    pub grants: Vec<(String, String)>,
    pub categories: Vec<String>,
    pub screenshots: usize,
    pub record: Option<String>,
    pub homepage: Option<String>,
}

impl Manual {
    pub fn from_manifest(manifest: &Manifest, prose: Option<String>) -> Self {
        let catalog = manifest.catalog.as_ref();
        Self {
            id: manifest.spore.id.clone(),
            name: manifest.spore.name.clone(),
            version: manifest.spore.version.clone(),
            license: manifest.spore.license.clone(),
            summary: manifest.spore.summary.clone(),
            description: manifest.spore.description.clone(),
            usage: manifest
                .docs
                .as_ref()
                .map(|docs| docs.usage.clone())
                .unwrap_or_default(),
            prose,
            launch_default: manifest.launch.default.clone(),
            surfaces: manifest.launch.surfaces.clone(),
            grants: manifest
                .grants
                .iter()
                .map(|(name, reason)| (name.clone(), reason.clone()))
                .collect(),
            categories: catalog
                .map(|catalog| catalog.categories.clone())
                .unwrap_or_default(),
            screenshots: catalog.map_or(0, |catalog| catalog.screenshots.len()),
            record: catalog.and_then(|catalog| catalog.record.clone()),
            homepage: manifest.spore.homepage.clone(),
        }
    }

    /// Render a terminal man page. `width` is the full page width; body columns
    /// are indented the way `man(1)` indents them.
    pub fn render(&self, width: usize) -> String {
        let width = width.max(40);
        let body = width.saturating_sub(INDENT);
        let mut out = String::new();

        let title = format!("{}(1)", self.name.to_uppercase());
        let header = format!("{title}  fungOS Spore Manual  {title}");
        let _ = writeln!(out, "{header}");

        section(&mut out, "NAME");
        paragraph(
            &mut out,
            body,
            &format!("{} \u{2014} {}", self.name, self.summary),
        );

        if !self.usage.is_empty() {
            section(&mut out, "SYNOPSIS");
            for line in &self.usage {
                paragraph(&mut out, body, line);
            }
        }

        section(&mut out, "DESCRIPTION");
        paragraph(
            &mut out,
            body,
            self.description.as_deref().unwrap_or(&self.summary),
        );
        match &self.prose {
            Some(prose) => render_markdown(&mut out, prose, body),
            None => paragraph(
                &mut out,
                body,
                "This Spore ships no authored manual; its summary and declared grants are the whole of it.",
            ),
        }

        section(&mut out, "VERSION");
        paragraph(&mut out, body, &format!("{} {}", self.name, self.version));
        paragraph(&mut out, body, &self.id);

        section(&mut out, "LICENSE");
        paragraph(&mut out, body, &self.license);

        section(&mut out, "LAUNCH");
        let surfaces = self
            .surfaces
            .iter()
            .map(|surface| {
                if *surface == self.launch_default {
                    format!("{surface} (default)")
                } else {
                    surface.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        paragraph(&mut out, body, &surfaces);

        if !self.grants.is_empty() {
            section(&mut out, "GRANTS");
            let key = self
                .grants
                .iter()
                .map(|(name, _)| name.chars().count())
                .max()
                .unwrap_or(0)
                .max(12);
            for (name, reason) in &self.grants {
                let prefix = format!("{name:<key$}  ");
                let column = body.saturating_sub(key + 2).max(24);
                for (index, line) in wrap(reason, column).iter().enumerate() {
                    if index == 0 {
                        row(&mut out, &format!("{prefix}{line}"));
                    } else {
                        row(&mut out, &format!("{}{line}", " ".repeat(key + 2)));
                    }
                }
            }
        }

        if !self.categories.is_empty() || self.screenshots > 0 {
            section(&mut out, "CATALOG");
            if !self.categories.is_empty() {
                row(
                    &mut out,
                    &format!("categories   {}", self.categories.join(", ")),
                );
            }
            if self.screenshots > 0 {
                row(&mut out, &format!("screenshots  {}", self.screenshots));
            }
            if let Some(record) = &self.record {
                row(&mut out, &format!("record       {record}"));
            }
        }

        if let Some(homepage) = &self.homepage {
            section(&mut out, "HOMEPAGE");
            paragraph(&mut out, body, homepage);
        }

        section(&mut out, "SEE ALSO");
        paragraph(&mut out, body, "soil-spore(1)");
        out
    }
}

/// Read the authored body a manifest names, if any.
pub fn read_prose(root: &Path, manifest: &Manifest) -> Result<Option<String>, Error> {
    let Some(man) = manifest.docs.as_ref().and_then(|docs| docs.man.as_ref()) else {
        return Ok(None);
    };
    let path = root.join(man);
    let prose = fs::read_to_string(&path)
        .map_err(|error| Error::Io(format!("{}: {error}", path.display())))?;
    Ok(Some(prose))
}

/// A compact metadata block: what `spore info` prints and what a store row
/// reads before it fetches a single CID.
pub fn render_info(manifest: &Manifest) -> String {
    let mut rows: Vec<(&str, String)> = vec![
        ("id", manifest.spore.id.clone()),
        ("name", manifest.spore.name.clone()),
        ("version", manifest.spore.version.clone()),
        ("license", manifest.spore.license.clone()),
        ("summary", manifest.spore.summary.clone()),
    ];
    if let Some(description) = &manifest.spore.description {
        rows.push(("description", description.clone()));
    }
    if let Some(homepage) = &manifest.spore.homepage {
        rows.push(("homepage", homepage.clone()));
    }
    if let Some(system) = &manifest.system {
        rows.push(("system", system.role.clone()));
    }
    rows.push((
        "launch",
        format!(
            "{} (default: {})",
            manifest.launch.surfaces.join(", "),
            manifest.launch.default
        ),
    ));
    rows.push(("grants", manifest.grants.len().to_string()));
    if let Some(docs) = &manifest.docs {
        rows.push(("usage", docs.usage.join(" | ")));
        if let Some(man) = &docs.man {
            rows.push(("manual", man.clone()));
        }
    }
    if let Some(presentation) = &manifest.presentation
        && let Some(icon) = &presentation.icon
    {
        rows.push(("icon", icon.clone()));
    }
    if let Some(catalog) = &manifest.catalog {
        rows.push(("categories", catalog.categories.join(", ")));
        if !catalog.keywords.is_empty() {
            rows.push(("keywords", catalog.keywords.join(", ")));
        }
        rows.push(("screenshots", catalog.screenshots.len().to_string()));
        if let Some(record) = &catalog.record {
            rows.push(("record", record.clone()));
        }
    }

    let key = rows.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (name, value) in rows {
        let _ = writeln!(out, "{name:<key$}  {value}");
    }
    out
}

const INDENT: usize = 7;
const HANG: usize = 2;

fn section(out: &mut String, title: &str) {
    let _ = writeln!(out);
    let _ = writeln!(out, "{title}");
}

fn paragraph(out: &mut String, width: usize, text: &str) {
    for line in wrap(text, width) {
        let _ = writeln!(out, "{}{line}", " ".repeat(INDENT));
    }
}

fn row(out: &mut String, text: &str) {
    let _ = writeln!(out, "{}{text}", " ".repeat(INDENT));
}

/// Greedy wrap. Words longer than the column are left whole; a man page that
/// silently hyphenates a package id is worse than one long line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Render the authored Markdown body into the indented man column.
///
/// This is a text renderer for the document subset a manual uses: headings,
/// paragraphs, lists, fenced code and inline emphasis. It is not a Markdown
/// engine; Canvas renders the same source richly when the Spore is open.
fn render_markdown(out: &mut String, source: &str, width: usize) {
    let mut fenced = false;
    for line in source.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            let _ = writeln!(out, "{}{trimmed}", " ".repeat(INDENT + HANG));
            continue;
        }
        let trimmed = trimmed.trim_start();
        if trimmed.is_empty() {
            let _ = writeln!(out);
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            let _ = writeln!(out);
            let _ = writeln!(out, "{}{}", " ".repeat(INDENT), heading.to_uppercase());
            continue;
        }
        let text = inline(trimmed);
        let bullet =
            trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ");
        if bullet {
            let body = text.trim_start_matches(['-', '*', '+']).trim_start();
            let words = wrap(&format!("\u{2022} {body}"), width.saturating_sub(HANG));
            for (index, line) in words.iter().enumerate() {
                let pad = if index == 0 { "" } else { "  " };
                let _ = writeln!(out, "{}{pad}{line}", " ".repeat(INDENT));
            }
            continue;
        }
        for line in wrap(&text, width) {
            let _ = writeln!(out, "{}{line}", " ".repeat(INDENT));
        }
    }
}

/// Strip the markup a terminal cannot show and keep what it can.
fn inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let Some(end) = rest[start..].find("](") else {
            break;
        };
        let Some(close) = rest[start + end + 2..].find(')') else {
            break;
        };
        let label = &rest[start + 1..start + end];
        let target = &rest[start + end + 2..start + end + 2 + close];
        out.push_str(&rest[..start]);
        out.push_str(label);
        if !label.contains(target) {
            out.push_str(" (");
            out.push_str(target);
            out.push(')');
        }
        rest = &rest[start + end + 2 + close + 1..];
    }
    out.push_str(rest);
    let mut cleaned = String::with_capacity(out.len());
    for token in out.split("`") {
        let token = token.replace("**", "").replace("__", "");
        cleaned.push_str(&token);
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"schema = 1
[spore]
id = "dev.fungos.markdown"
name = "markdown"
version = "0.1.0"
license = "MIT"
summary = "Markdown reader/editor"
description = "Read and edit Markdown as one document."
[docs]
usage = ["markdown [FILE]", "markdown --version"]
man = "docs/man.md"
[engine]
soil = ">=0.1"
target = "wasm32-wasip2"
[module]
path = "module.wasm"
[ui]
entry = "ui/editor.lua"
[launch]
default = "canvas"
surfaces = ["web", "native", "canvas"]
[grants]
"soil:canvas" = "present the surface"
[catalog]
categories = ["productivity"]
screenshots = [{ cid = "ipfs://bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi", caption = "Editing a note" }]
"#;

    #[test]
    fn a_manual_is_generated_from_the_manifest() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let manual = Manual::from_manifest(
            &manifest,
            Some("# Notes\n\nOne document, three hosts.".into()),
        );
        let page = manual.render(80);
        assert!(page.starts_with("MARKDOWN(1)"));
        assert!(page.contains("NAME"));
        assert!(page.contains("markdown --version"));
        assert!(page.contains("NOTES"));
        assert!(page.contains("One document, three hosts."));
        assert!(page.contains("soil:canvas"));
        assert!(page.contains("MIT"));
        assert!(page.contains("productivity"));
    }

    #[test]
    fn a_spore_without_prose_still_has_a_page() {
        let manifest = Manifest::parse(MANIFEST).unwrap();
        let page = Manual::from_manifest(&manifest, None).render(80);
        assert!(page.contains("ships no authored manual"));
    }

    #[test]
    fn links_keep_their_target_and_emphasis_is_stripped() {
        assert_eq!(
            inline("see [the spec](https://example.test/spec) now"),
            "see the spec (https://example.test/spec) now"
        );
        assert_eq!(inline("**bold** and `code`"), "bold and code");
    }

    #[test]
    fn wrapping_respects_the_column() {
        let lines = wrap("one two three four", 9);
        assert!(lines.iter().all(|line| line.chars().count() <= 9));
        assert_eq!(lines, vec!["one two", "three", "four"]);
    }
}

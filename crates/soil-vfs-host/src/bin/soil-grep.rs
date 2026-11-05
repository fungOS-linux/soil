//! Bounded code search over a `fungos-vfs` workspace.
//!
//! ```sh
//! soil-grep --root ~/fungos-node --mode regex "fn main"
//! soil-grep --root ~/fungos-node --mode fuzzy "contcts"
//! ```

use std::path::PathBuf;

use anyhow::{Context, Result};
use soil_vfs_host::{VfsHost, index};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let mut root = None;
    let mut index_path = None;
    let mut mode = index::Mode::Regex;
    let mut limit = 50u32;
    let mut query = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    args.get(index + 1).context("--root requires PATH")?,
                ));
                index += 2;
            }
            "--index" => {
                index_path = Some(PathBuf::from(
                    args.get(index + 1).context("--index requires PATH")?,
                ));
                index += 2;
            }
            "--mode" => {
                mode = match args.get(index + 1).map(String::as_str) {
                    Some("regex") => index::Mode::Regex,
                    Some("fuzzy") => index::Mode::Fuzzy,
                    Some("full-text") => index::Mode::FullText,
                    Some("substring") => index::Mode::Substring,
                    Some("semantic") => index::Mode::Semantic,
                    _ => anyhow::bail!("--mode regex|fuzzy|full-text|substring|semantic"),
                };
                index += 2;
            }
            "--limit" => {
                limit = args
                    .get(index + 1)
                    .context("--limit requires N")?
                    .parse()
                    .context("invalid --limit")?;
                index += 2;
            }
            "--" => {
                query.extend(args[index + 1..].iter().cloned());
                break;
            }
            value if value.starts_with('-') => anyhow::bail!("unknown option {value}"),
            value => {
                query.push(value.to_string());
                index += 1;
            }
        }
    }
    if query.is_empty() {
        anyhow::bail!(
            "usage: soil-grep --root PATH [--index PATH] [--mode MODE] [--limit N] QUERY"
        );
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let mut host = match index_path {
        Some(path) => VfsHost::open_with_index(&root, path)?,
        None => VfsHost::open(&root)?,
    };
    use soil_vfs_host::index::Host as _;
    let page = host.search(index::Query {
        text: query.join(" "),
        mode: Some(mode),
        parent: None,
        media_type_prefix: None,
        offset: 0,
        limit,
    })?;
    for hit in page.hits {
        match &hit.snippet {
            Some(snippet) => println!("{}\t{}\t{}", hit.path, hit.score, snippet.trim()),
            None => println!("{}\t{}", hit.path, hit.score),
        }
    }
    Ok(())
}

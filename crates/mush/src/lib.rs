//! Mush: a sandboxed shell over one VFS workspace.
//!
//! Mush does not consult the host filesystem, `PATH`, environment, or process
//! table. Every path is workspace-relative and every mutation is a VFS
//! namespace operation. Scripts are Mush commands, not host shell syntax.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fungos_vfs::Vfs;

pub struct Mush {
    vfs: Vfs,
    root: PathBuf,
    cwd: String,
}

impl Mush {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let vfs = Vfs::open(&root).with_context(|| format!("open VFS {}", root.display()))?;
        Ok(Self {
            vfs,
            root,
            cwd: String::new(),
        })
    }

    pub fn prompt(&self) -> String {
        if self.cwd.is_empty() {
            "mush:/$ ".into()
        } else {
            format!("mush:/{}$ ", self.cwd)
        }
    }

    pub fn execute_line(&mut self, line: &str) -> Result<Outcome> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(Outcome::Continue);
        }
        let words = split_words(line)?;
        let Some(command) = words.first().map(String::as_str) else {
            return Ok(Outcome::Continue);
        };
        let args = &words[1..];
        match command {
            "help" => {
                println!("{}", HELP);
            }
            "exit" | "quit" => return Ok(Outcome::Exit),
            "pwd" => println!("/{}", self.cwd),
            "cd" => {
                let target = args.first().map(String::as_str).unwrap_or("");
                self.cwd = self.resolve(target)?;
            }
            "ls" => {
                let target = args.first().map(String::as_str).unwrap_or(".");
                let dir = self.resolve(target)?;
                for (path, is_dir, size, content_id) in self.children(&dir)? {
                    if is_dir {
                        println!("d\t-\t\t{path}/");
                    } else {
                        println!("f\t{size}\t{content_id}\t{path}");
                    }
                }
            }
            "cat" => {
                let path = self.arg(args, "cat PATH")?;
                let path = self.resolve(path)?;
                let bytes = self.vfs.get(&path)?;
                print!("{}", String::from_utf8_lossy(&bytes));
            }
            "write" => {
                let path = args.first().context("write PATH TEXT...")?;
                let path = self.resolve(path)?;
                let text = args[1..].join(" ");
                let bytes = if args.len() > 1 {
                    format!("{text}\n").into_bytes()
                } else {
                    Vec::new()
                };
                let media = media_type(&path);
                let entry = self.vfs.put(&path, &bytes, media, None)?;
                println!(
                    "wrote /{} v{} {}",
                    entry.path, entry.version, entry.content_id
                );
            }
            "rm" => {
                if args.is_empty() {
                    bail!("rm PATH...");
                }
                for arg in args {
                    let path = self.resolve(arg)?;
                    let entry = self
                        .vfs
                        .file(&path)?
                        .with_context(|| format!("not found: /{path}"))?;
                    self.vfs.remove(&path)?;
                    println!(
                        "unlinked /{} ({}, v{}); blob retained",
                        path, entry.content_id, entry.version
                    );
                }
            }
            "mv" => {
                if args.len() != 2 {
                    bail!("mv FROM TO");
                }
                let from = self.resolve(&args[0])?;
                let to = self.resolve(&args[1])?;
                let entry = self.vfs.rename(&from, &to)?;
                println!("renamed /{from} -> /{} ({})", entry.path, entry.content_id);
            }
            "cp" => {
                if args.len() != 2 {
                    bail!("cp FROM TO");
                }
                let from = self.resolve(&args[0])?;
                let to = self.resolve(&args[1])?;
                let bytes = self.vfs.get(&from)?;
                let media = media_type(&to);
                let entry = self.vfs.put(&to, &bytes, media, None)?;
                println!("copied /{from} -> /{} v{}", entry.path, entry.version);
            }
            "stat" => {
                let path = self.resolve(self.arg(args, "stat PATH")?)?;
                let entry = self
                    .vfs
                    .file(&path)?
                    .with_context(|| format!("not found: /{path}"))?;
                println!("{entry:?}");
            }
            "history" => {
                let path = self.resolve(self.arg(args, "history PATH")?)?;
                for (version, content_id, size, created) in self.vfs.history(&path)? {
                    println!("v{version}\t{size}\t{created}\t{content_id}");
                }
            }
            "search" => {
                let query = args.join(" ");
                if query.is_empty() {
                    bail!("search QUERY");
                }
                let mut host = soil_vfs_host::VfsHost::open(&self.root)?;
                use soil_vfs_host::index::Host as _;
                host.refresh()
                    .map_err(|error| anyhow::anyhow!("index: {error:?}"))?;
                let page = host.search(soil_vfs_host::index::Query {
                    text: query,
                    mode: Some(soil_vfs_host::index::Mode::Fuzzy),
                    parent: None,
                    media_type_prefix: None,
                    offset: 0,
                    limit: 50,
                })?;
                for hit in page.hits {
                    println!(
                        "{}\t{}\t{}",
                        hit.path,
                        hit.score,
                        hit.snippet.unwrap_or_default()
                    );
                }
            }
            "source" => {
                let path = self.resolve(self.arg(args, "source SCRIPT")?)?;
                let script =
                    String::from_utf8(self.vfs.get(&path)?).context("script is not UTF-8")?;
                for line in script.lines() {
                    if self.execute_line(line)? == Outcome::Exit {
                        break;
                    }
                }
            }
            "echo" => println!("{}", args.join(" ")),
            other => bail!("unknown command {other}; try help"),
        }
        Ok(Outcome::Continue)
    }

    fn arg<'a>(&self, args: &'a [String], usage: &str) -> Result<&'a str> {
        args.first()
            .map(String::as_str)
            .with_context(|| usage.to_string())
    }

    fn resolve(&self, value: &str) -> Result<String> {
        if value.starts_with('/') {
            bail!("paths are workspace-relative; leading / is not allowed");
        }
        let mut parts: Vec<&str> = self
            .cwd
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        for part in value.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part if part.contains('\\') => bail!("invalid path"),
                part => parts.push(part),
            }
        }
        Ok(parts.join("/"))
    }

    fn children(&self, dir: &str) -> Result<Vec<(String, bool, u64, String)>> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let entries = self.vfs.list(if dir.is_empty() { "" } else { dir })?;
        let mut children = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for entry in entries {
            let rest = entry.path.strip_prefix(&prefix).unwrap_or(&entry.path);
            if let Some((first, _)) = rest.split_once('/') {
                if seen.insert(first.to_string()) {
                    children.push((format!("{prefix}{first}"), true, 0, String::new()));
                }
            } else if seen.insert(rest.to_string()) {
                children.push((
                    entry.path.clone(),
                    false,
                    entry.size,
                    entry.content_id.to_string(),
                ));
            }
        }
        Ok(children)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Exit,
}

fn media_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("rs") => "text/x-rust",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        Some("sh" | "bash") => "text/x-shellscript",
        _ => "text/plain",
    }
}

fn split_words(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    for ch in line.chars() {
        if escape {
            word.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(ch),
        }
    }
    if quote.is_some() {
        bail!("unterminated quote");
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

pub const HELP: &str = "\
mush — a sandboxed VFS shell

  help                    this text
  pwd                     print workspace path
  cd [DIR]                change directory
  ls [DIR]                list direct children
  cat PATH                print a file
  write PATH [TEXT...]    create/replace a file
  rm PATH...              unlink namespace entries (blobs retained)
  mv FROM TO              rename a namespace entry
  cp FROM TO              copy bytes to a new version
  stat PATH               show entry metadata
  history PATH            show versions
  search QUERY            fuzzy index search
  source SCRIPT           run a Mush script from the VFS
  echo TEXT               print text
  exit                    leave";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxed_commands_keep_blobs_and_reject_host_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut mush = Mush::open(dir.path()).unwrap();
        mush.execute_line("write notes/today.md hello world")
            .unwrap();
        mush.execute_line("ls notes").unwrap();
        mush.execute_line("cp notes/today.md copies/today.md")
            .unwrap();
        mush.execute_line("mv notes/today.md notes/archive.md")
            .unwrap();
        mush.execute_line("rm notes/archive.md").unwrap();
        assert!(mush.vfs.file("notes/today.md").unwrap().is_none());
        assert!(mush.vfs.file("notes/archive.md").unwrap().is_none());
        assert_eq!(mush.vfs.history("notes/archive.md").unwrap().len(), 1);
        assert_eq!(mush.vfs.get("copies/today.md").unwrap(), b"hello world\n");
        assert!(mush.execute_line("cat /etc/passwd").is_err());
        assert!(mush.execute_line("cd ../../..").is_ok());
        assert_eq!(mush.cwd, "");
    }

    #[test]
    fn scripts_are_vfs_data_not_host_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mut mush = Mush::open(dir.path()).unwrap();
        mush.vfs
            .put(
                "scripts/demo.mush",
                b"echo hi\nwrite out.txt from script\n",
                "text/plain",
                None,
            )
            .unwrap();
        mush.execute_line("source scripts/demo.mush").unwrap();
        assert_eq!(mush.vfs.get("out.txt").unwrap(), b"from script\n");
        assert!(mush.execute_line("sh -c id").is_err());
    }
}

//! Mush Spore: a sandboxed shell over `soil:vfs` and `soil:index`.
//!
//! The native host owns authority and calls one line at a time. This component
//! owns parsing, `PWD`/`OLDPWD`, environment variables, startup files, and
//! command semantics. It has no host filesystem, process, or network access.

wit_bindgen::generate!({ path: "wit", world: "mush", generate_all });

use std::{collections::BTreeMap, sync::Mutex};

use soil::index::index::{self, Mode, Query};
use soil::vfs::vfs::{self, Kind};
use soil::vfs_mut::vfs_mut;

const MAX_SCRIPT_DEPTH: u8 = 8;
const READ_CHUNK: u32 = 64 * 1024;

static SHELL: Mutex<Option<Shell>> = Mutex::new(None);

struct Component;

export!(Component);

impl Guest for Component {
    fn start(args: Vec<String>) -> Result<Response, String> {
        let mut shell = Shell::new(args);
        let output = shell.load_startup();
        let prompt = shell.prompt();
        let exit = shell.exit;
        *SHELL.lock().map_err(|_| "shell state poisoned")? = Some(shell);
        Ok(Response {
            output,
            exit,
            prompt,
        })
    }

    fn execute(line: String) -> Result<Response, String> {
        let mut guard = SHELL.lock().map_err(|_| "shell state poisoned")?;
        let shell = guard.as_mut().ok_or("shell not started")?;
        shell.run_line(&line);
        Ok(Response {
            output: std::mem::take(&mut shell.out),
            exit: shell.exit,
            prompt: shell.prompt(),
        })
    }

    fn prompt() -> String {
        SHELL
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(Shell::prompt))
            .unwrap_or_else(|| "mush:/$ ".into())
    }

    fn environment(name: String) -> Option<String> {
        SHELL.lock().ok().and_then(|guard| {
            guard
                .as_ref()
                .and_then(|shell| shell.env.get(&name).cloned())
        })
    }
}

struct Shell {
    cwd: String,
    env: BTreeMap<String, String>,
    out: String,
    exit: bool,
    source_depth: u8,
}

impl Shell {
    fn new(args: Vec<String>) -> Self {
        let mut env = BTreeMap::new();
        for (key, value) in [
            ("HOME", "/"),
            ("PWD", "/"),
            ("OLDPWD", "/"),
            ("PATH", "/bin:/usr/bin"),
            ("TERM", "dumb"),
            ("USER", "mush"),
            ("LOGNAME", "mush"),
            ("SHELL", "/bin/mush"),
            ("MUSH_VERSION", env!("CARGO_PKG_VERSION")),
        ] {
            env.insert(key.into(), value.into());
        }
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if let Some(value) = arg.strip_prefix("--term=") {
                env.insert("TERM".into(), value.into());
            } else if arg == "--term" {
                if let Some(value) = iter.next() {
                    env.insert("TERM".into(), value);
                }
            } else if let Some(value) = arg.strip_prefix("--home=") {
                env.insert("HOME".into(), value.into());
            }
        }
        Self {
            cwd: String::new(),
            env,
            out: String::new(),
            exit: false,
            source_depth: 0,
        }
    }

    fn load_startup(&mut self) -> String {
        for candidate in [".mushrc", "mushrc"] {
            match self.read_file(candidate) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for line in text.lines() {
                        self.run_line(line);
                        if self.exit {
                            break;
                        }
                    }
                    break;
                }
                Err(_) => continue,
            }
        }
        std::mem::take(&mut self.out)
    }

    fn run_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return;
        }
        let words = match split_words(line) {
            Ok(words) => words,
            Err(error) => {
                self.error(&error);
                return;
            }
        };
        let (words, redirect) = match parse_redirection(words) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.error(&error);
                return;
            }
        };
        let Some(command) = words.first().map(String::as_str) else {
            return;
        };
        let args: Vec<String> = words[1..].iter().map(|word| self.expand(word)).collect();
        let saved = std::mem::take(&mut self.out);
        let result = match command {
            "help" => {
                self.out.push_str(HELP);
                self.out.push('\n');
                Ok(())
            }
            "exit" | "quit" => {
                self.exit = true;
                Ok(())
            }
            "pwd" => {
                self.out.push_str(&self.display_cwd());
                self.out.push('\n');
                Ok(())
            }
            "cd" => self.cd(args.first().map(String::as_str)),
            "ls" => self.ls(&args),
            "mkdir" => self.mkdir(&args),
            "find" => self.find(&args),
            "rmdir" => self.rmdir(&args),
            "cat" => self.cat(&args),
            "write" => self.write(&args),
            "append" => self.append(&args),
            "touch" => self.touch(&args),
            "head" => self.head(&args),
            "tail" => self.tail(&args),
            "grep" => self.grep(&args),
            "wc" => self.wc(&args),
            "stat" => self.stat(&args),
            "tree" => self.tree(&args),
            "rm" => self.rm(&args),
            "mv" => self.mv(&args),
            "cp" => self.cp(&args),
            "search" => self.search(&args),
            "source" => self.source(&args),
            "echo" => {
                self.out.push_str(&args.join(" "));
                self.out.push('\n');
                Ok(())
            }
            "export" | "set" => self.set_env(&args),
            "unset" => self.unset_env(&args),
            "env" => {
                for (key, value) in &self.env {
                    self.out.push_str(&format!("{key}={value}\n"));
                }
                Ok(())
            }
            other => Err(format!("unknown command {other}; try help")),
        };
        let produced = std::mem::replace(&mut self.out, saved);
        match result {
            Ok(()) => {
                if let Some((append, target)) = redirect {
                    if let Err(error) = self.redirect(&target, append, &produced) {
                        self.error(&error);
                    }
                } else {
                    self.out.push_str(&produced);
                }
            }
            Err(error) => self.error(&error),
        }
    }

    fn redirect(&mut self, target: &str, append: bool, data: &str) -> Result<(), String> {
        let path = self.resolve(target)?;
        let (mut bytes, version) = match vfs::stat(&path) {
            Ok(entry) if entry.kind == Kind::Directory => {
                return Err(format!("redirect: {} is a directory", self.display(&path)));
            }
            Ok(entry) if append => (self.read_file(&path)?, Some(entry.version)),
            Ok(_) => (Vec::new(), None),
            Err(_) => (Vec::new(), None),
        };
        bytes.extend_from_slice(data.as_bytes());
        vfs_mut::write(&path, &bytes, media_type(&path), version)
            .map_err(|error| format!("redirect {}: {error:?}", self.display(&path)))?;
        Ok(())
    }

    fn cd(&mut self, target: Option<&str>) -> Result<(), String> {
        let target = target.unwrap_or("~");
        let target = if target == "-" {
            self.env
                .get("OLDPWD")
                .cloned()
                .unwrap_or_else(|| "/".into())
        } else {
            target.to_string()
        };
        let path = self.resolve(&target)?;
        let entry = vfs::stat(&path).map_err(|error| format!("cd: {error:?}"))?;
        if entry.kind != Kind::Directory {
            return Err(format!("cd: not a directory: {}", self.display(&path)));
        }
        let old = self.display_cwd();
        self.cwd = path;
        self.env.insert("OLDPWD".into(), old);
        self.env.insert("PWD".into(), self.display_cwd());
        Ok(())
    }

    fn ls(&mut self, args: &[String]) -> Result<(), String> {
        let mut long = false;
        let mut target = None;
        for arg in args {
            if arg == "-l" {
                long = true;
            } else if arg == "-a" || arg == "-la" || arg == "-al" {
            } else if target.is_none() {
                target = Some(arg.as_str());
            }
        }
        let dir = self.resolve(target.unwrap_or("."))?;
        let mut offset = 0;
        let mut entries = Vec::new();
        loop {
            let page = vfs::ls(&dir, offset, 512).map_err(|error| format!("ls: {error:?}"))?;
            entries.extend(page.entries);
            match page.next {
                Some(next) if next > offset => offset = next,
                _ => break,
            }
        }
        for entry in entries {
            if long {
                let kind = if entry.kind == Kind::Directory {
                    "d"
                } else {
                    "f"
                };
                self.out.push_str(&format!(
                    "{kind}\t{}\t{}\t{}\n",
                    entry.size, entry.content_id, entry.name
                ));
            } else {
                self.out.push_str(&entry.name);
                if entry.kind == Kind::Directory {
                    self.out.push('/');
                }
                self.out.push('\n');
            }
        }
        Ok(())
    }

    fn mkdir(&mut self, args: &[String]) -> Result<(), String> {
        let create_parents = args.iter().any(|arg| arg == "-p");
        let paths = args
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Err("mkdir [-p] PATH...".into());
        }
        for arg in paths {
            let path = self.resolve(arg)?;
            if create_parents {
                self.mkdir_recursive(&path)?;
            } else {
                vfs_mut::mkdir(&path)
                    .map_err(|error| format!("mkdir {}: {error:?}", self.display(&path)))?;
            }
        }
        Ok(())
    }

    fn mkdir_recursive(&mut self, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Ok(());
        }
        let mut prefix = String::new();
        for component in path.split('/') {
            if component.is_empty() {
                continue;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            match vfs::stat(&prefix) {
                Ok(entry) if entry.kind == Kind::Directory => {}
                Ok(_) => return Err(format!("mkdir: {} is a file", self.display(&prefix))),
                Err(_) => vfs_mut::mkdir(&prefix)
                    .map_err(|error| format!("mkdir {}: {error:?}", self.display(&prefix)))?,
            }
        }
        Ok(())
    }

    fn rmdir(&mut self, args: &[String]) -> Result<(), String> {
        if args.is_empty() {
            return Err("rmdir PATH...".into());
        }
        for arg in args {
            let path = self.resolve(arg)?;
            vfs_mut::rmdir(&path)
                .map_err(|error| format!("rmdir {}: {error:?}", self.display(&path)))?;
        }
        Ok(())
    }

    fn cat(&mut self, args: &[String]) -> Result<(), String> {
        let Some(arg) = args.first() else {
            return Err("cat PATH".into());
        };
        let path = self.resolve(arg)?;
        let bytes = self.read_file(&path)?;
        self.out.push_str(&String::from_utf8_lossy(&bytes));
        Ok(())
    }

    fn write(&mut self, args: &[String]) -> Result<(), String> {
        let Some(arg) = args.first() else {
            return Err("write PATH [TEXT...]".into());
        };
        let path = self.resolve(arg)?;
        let bytes = if args.len() > 1 {
            format!("{}\n", args[1..].join(" ")).into_bytes()
        } else {
            Vec::new()
        };
        let result = vfs_mut::write(&path, &bytes, media_type(&path), None)
            .map_err(|error| format!("write {}: {error:?}", self.display(&path)))?;
        self.out.push_str(&format!(
            "wrote {} v{} {}\n",
            self.display(&path),
            result.version,
            result.content_id
        ));
        Ok(())
    }

    fn append(&mut self, args: &[String]) -> Result<(), String> {
        let Some(arg) = args.first() else {
            return Err("append PATH [TEXT...]".into());
        };
        let path = self.resolve(arg)?;
        let (mut bytes, version) = match vfs::stat(&path) {
            Ok(entry) if entry.kind == Kind::Directory => {
                return Err(format!("append: {} is a directory", self.display(&path)));
            }
            Ok(entry) => (self.read_file(&path)?, Some(entry.version)),
            Err(_) => (Vec::new(), None),
        };
        if args.len() > 1 {
            bytes.extend_from_slice(args[1..].join(" ").as_bytes());
            bytes.push(b'\n');
        }
        let result = vfs_mut::write(&path, &bytes, media_type(&path), version)
            .map_err(|error| format!("append {}: {error:?}", self.display(&path)))?;
        self.out.push_str(&format!(
            "appended {} v{} {}\n",
            self.display(&path),
            result.version,
            result.content_id
        ));
        Ok(())
    }

    fn touch(&mut self, args: &[String]) -> Result<(), String> {
        if args.is_empty() {
            return Err("touch PATH...".into());
        }
        for arg in args {
            let path = self.resolve(arg)?;
            if let Ok(entry) = vfs::stat(&path) {
                if entry.kind == Kind::Directory {
                    return Err(format!("touch: {} is a directory", self.display(&path)));
                }
                continue;
            }
            vfs_mut::write(&path, b"", media_type(&path), None)
                .map_err(|error| format!("touch {}: {error:?}", self.display(&path)))?;
        }
        Ok(())
    }

    fn head(&mut self, args: &[String]) -> Result<(), String> {
        self.head_tail(args, false)
    }

    fn tail(&mut self, args: &[String]) -> Result<(), String> {
        self.head_tail(args, true)
    }

    fn head_tail(&mut self, args: &[String], tail: bool) -> Result<(), String> {
        let name = if tail { "tail" } else { "head" };
        let mut count = 10usize;
        let mut path = None;
        let mut index = 0;
        while index < args.len() {
            if args[index] == "-n" {
                let Some(value) = args.get(index + 1) else {
                    return Err(format!("{name} -n N PATH"));
                };
                count = value
                    .parse::<usize>()
                    .map_err(|_| format!("{name}: invalid line count {value:?}"))?;
                index += 2;
            } else if let Some(value) = args[index].strip_prefix("-n") {
                count = value
                    .parse::<usize>()
                    .map_err(|_| format!("{name}: invalid line count {value:?}"))?;
                index += 1;
            } else if path.is_none() {
                path = Some(args[index].as_str());
                index += 1;
            } else {
                return Err(format!("{name} [-n N] PATH"));
            }
        }
        let Some(path) = path else {
            return Err(format!("{name} [-n N] PATH"));
        };
        let path = self.resolve(path)?;
        let text = String::from_utf8_lossy(&self.read_file(&path)?).to_string();
        let lines = text.lines().collect::<Vec<_>>();
        let start = if tail {
            lines.len().saturating_sub(count)
        } else {
            0
        };
        let end = if tail {
            lines.len()
        } else {
            count.min(lines.len())
        };
        for line in &lines[start..end] {
            self.out.push_str(line);
            self.out.push('\n');
        }
        Ok(())
    }

    fn grep(&mut self, args: &[String]) -> Result<(), String> {
        let mut ignore_case = false;
        let mut index = 0;
        while index < args.len() && args[index].starts_with('-') {
            if args[index] == "-i" {
                ignore_case = true;
                index += 1;
            } else {
                return Err(format!("grep: unknown option {}", args[index]));
            }
        }
        let Some(pattern) = args.get(index) else {
            return Err("grep [-i] PATTERN PATH...".into());
        };
        if index + 1 >= args.len() {
            return Err("grep [-i] PATTERN PATH...".into());
        }
        let needle = if ignore_case {
            pattern.to_ascii_lowercase()
        } else {
            pattern.clone()
        };
        for arg in &args[index + 1..] {
            let path = self.resolve(arg)?;
            let text = String::from_utf8_lossy(&self.read_file(&path)?).to_string();
            for (line_no, line) in text.lines().enumerate() {
                let haystack = if ignore_case {
                    line.to_ascii_lowercase()
                } else {
                    line.to_string()
                };
                if haystack.contains(&needle) {
                    self.out.push_str(&format!(
                        "{}:{}:{}\n",
                        self.display(&path),
                        line_no + 1,
                        line
                    ));
                }
            }
        }
        Ok(())
    }

    fn wc(&mut self, args: &[String]) -> Result<(), String> {
        let mut lines_only = false;
        let mut bytes_only = false;
        let mut paths = Vec::new();
        for arg in args {
            match arg.as_str() {
                "-l" => lines_only = true,
                "-c" => bytes_only = true,
                other => paths.push(other),
            }
        }
        if paths.is_empty() {
            return Err("wc [-l|-c] PATH...".into());
        }
        for arg in paths {
            let path = self.resolve(arg)?;
            let bytes = self.read_file(&path)?;
            let lines = bytes.iter().filter(|byte| **byte == b'\n').count();
            if lines_only && !bytes_only {
                self.out
                    .push_str(&format!("{lines}\t{}\n", self.display(&path)));
            } else if bytes_only && !lines_only {
                self.out
                    .push_str(&format!("{}\t{}\n", bytes.len(), self.display(&path)));
            } else {
                self.out.push_str(&format!(
                    "{lines}\t{}\t{}\n",
                    bytes.len(),
                    self.display(&path)
                ));
            }
        }
        Ok(())
    }

    fn stat(&mut self, args: &[String]) -> Result<(), String> {
        let Some(arg) = args.first() else {
            return Err("stat PATH".into());
        };
        if args.len() != 1 {
            return Err("stat PATH".into());
        }
        let path = self.resolve(arg)?;
        let entry = vfs::stat(&path).map_err(|error| format!("stat: {error:?}"))?;
        self.out.push_str(&format!(
            "path: {}\nkind: {}\nsize: {}\nversion: {}\ncontent: {}\nmedia: {}\nupdated: {}\n",
            self.display(&entry.path),
            if entry.kind == Kind::Directory {
                "directory"
            } else {
                "file"
            },
            entry.size,
            entry.version,
            entry.content_id,
            entry.media_type,
            entry.updated_at_ms,
        ));
        Ok(())
    }

    fn tree(&mut self, args: &[String]) -> Result<(), String> {
        let start = self.resolve(args.first().map(String::as_str).unwrap_or("."))?;
        let mut stack = vec![(start.clone(), 0usize)];
        let mut entries = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let mut offset = 0;
            loop {
                let page =
                    vfs::ls(&dir, offset, 512).map_err(|error| format!("tree: {error:?}"))?;
                for entry in page.entries {
                    entries += 1;
                    if entries > 5_000 {
                        return Err("tree: entry limit exceeded".into());
                    }
                    self.out.push_str(&"  ".repeat(depth));
                    self.out.push_str(&entry.name);
                    if entry.kind == Kind::Directory {
                        self.out.push('/');
                        stack.push((entry.path, depth + 1));
                    }
                    self.out.push('\n');
                }
                match page.next {
                    Some(next) if next > offset => offset = next,
                    _ => break,
                }
            }
        }
        Ok(())
    }

    fn rm(&mut self, args: &[String]) -> Result<(), String> {
        let recursive = args
            .iter()
            .any(|arg| arg == "-r" || arg == "-R" || arg == "-rf");
        let paths = args
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Err("rm [-r] PATH...".into());
        }
        for arg in paths {
            let path = self.resolve(arg)?;
            if recursive {
                self.remove_recursive(&path)?;
            } else {
                vfs_mut::remove(&path)
                    .map_err(|error| format!("rm {}: {error:?}", self.display(&path)))?;
            }
        }
        Ok(())
    }

    fn remove_recursive(&mut self, path: &str) -> Result<(), String> {
        let entry = vfs::stat(path).map_err(|error| format!("rm: {error:?}"))?;
        if entry.kind == Kind::Directory {
            let mut offset = 0;
            let mut children = Vec::new();
            loop {
                let page = vfs::ls(path, offset, 512).map_err(|error| format!("rm: {error:?}"))?;
                children.extend(page.entries.into_iter().map(|entry| entry.path));
                match page.next {
                    Some(next) if next > offset => offset = next,
                    _ => break,
                }
            }
            for child in children {
                self.remove_recursive(&child)?;
            }
            vfs_mut::rmdir(path)
                .map_err(|error| format!("rmdir {}: {error:?}", self.display(path)))?;
        } else {
            vfs_mut::remove(path)
                .map_err(|error| format!("rm {}: {error:?}", self.display(path)))?;
        }
        Ok(())
    }

    fn mv(&mut self, args: &[String]) -> Result<(), String> {
        if args.len() != 2 {
            return Err("mv FROM TO".into());
        }
        let from = self.resolve(&args[0])?;
        let to = self.resolve(&args[1])?;
        let result = vfs_mut::rename(&from, &to).map_err(|error| {
            format!(
                "mv {} {}: {error:?}",
                self.display(&from),
                self.display(&to)
            )
        })?;
        self.out.push_str(&format!(
            "renamed {} -> {} ({})\n",
            self.display(&from),
            self.display(&to),
            result.content_id
        ));
        Ok(())
    }

    fn cp(&mut self, args: &[String]) -> Result<(), String> {
        let recursive = args.iter().any(|arg| arg == "-r" || arg == "-R");
        let paths = args
            .iter()
            .filter(|arg| !arg.starts_with('-'))
            .collect::<Vec<_>>();
        if paths.len() != 2 {
            return Err("cp [-r] FROM TO".into());
        }
        let from = self.resolve(paths[0])?;
        let mut to = self.resolve(paths[1])?;
        let entry = vfs::stat(&from).map_err(|error| format!("cp: {error:?}"))?;
        if entry.kind == Kind::Directory {
            if !recursive {
                return Err("cp: source is a directory; use -r".into());
            }
            if matches!(vfs::stat(&to), Ok(entry) if entry.kind == Kind::Directory) {
                to = format!("{to}/{}", from.rsplit('/').next().unwrap_or("copy"));
            }
            self.copy_tree(&from, &to)?;
        } else {
            let bytes = self.read_file(&from)?;
            let result = vfs_mut::write(&to, &bytes, media_type(&to), None).map_err(|error| {
                format!(
                    "cp {} {}: {error:?}",
                    self.display(&from),
                    self.display(&to)
                )
            })?;
            self.out.push_str(&format!(
                "copied {} -> {} v{}\n",
                self.display(&from),
                self.display(&to),
                result.version
            ));
        }
        Ok(())
    }

    fn copy_tree(&mut self, from: &str, to: &str) -> Result<(), String> {
        match vfs::stat(to) {
            Ok(entry) if entry.kind == Kind::Directory => {}
            Ok(_) => return Err(format!("cp: {} is a file", self.display(to))),
            Err(_) => vfs_mut::mkdir(to)
                .map_err(|error| format!("mkdir {}: {error:?}", self.display(to)))?,
        }
        let mut offset = 0;
        loop {
            let page = vfs::ls(from, offset, 512).map_err(|error| format!("cp: {error:?}"))?;
            for entry in page.entries {
                let target = if to.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{to}/{}", entry.name)
                };
                if entry.kind == Kind::Directory {
                    self.copy_tree(&entry.path, &target)?;
                } else {
                    let bytes = self.read_file(&entry.path)?;
                    vfs_mut::write(&target, &bytes, media_type(&target), None)
                        .map_err(|error| format!("cp {}: {error:?}", self.display(&target)))?;
                }
            }
            match page.next {
                Some(next) if next > offset => offset = next,
                _ => break,
            }
        }
        Ok(())
    }

    fn find(&mut self, args: &[String]) -> Result<(), String> {
        let (root, pattern) = match args {
            [] => (".", None),
            [root] => (root.as_str(), None),
            [root, pattern] => (root.as_str(), Some(pattern.as_str())),
            _ => return Err("find [ROOT] [PATTERN]".into()),
        };
        let root = self.resolve(root)?;
        let mut stack = vec![(root, 0usize)];
        let mut count = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            if depth > 64 {
                return Err("find: maximum depth exceeded".into());
            }
            let mut offset = 0;
            loop {
                let page =
                    vfs::ls(&dir, offset, 512).map_err(|error| format!("find: {error:?}"))?;
                for entry in page.entries {
                    count += 1;
                    if count > 20_000 {
                        return Err("find: too many entries".into());
                    }
                    let path = self.display(&entry.path);
                    if pattern.is_none_or(|pattern| {
                        glob_match(pattern, &entry.name) || glob_match(pattern, &path)
                    }) {
                        self.out.push_str(&path);
                        if entry.kind == Kind::Directory {
                            self.out.push('/');
                        }
                        self.out.push('\n');
                    }
                    if entry.kind == Kind::Directory {
                        stack.push((entry.path, depth + 1));
                    }
                }
                match page.next {
                    Some(next) if next > offset => offset = next,
                    _ => break,
                }
            }
        }
        Ok(())
    }

    fn search(&mut self, args: &[String]) -> Result<(), String> {
        if args.is_empty() {
            return Err("search QUERY".into());
        }
        let page = index::search(&Query {
            text: args.join(" "),
            mode: Some(Mode::Fuzzy),
            parent: None,
            media_type_prefix: None,
            offset: 0,
            limit: 50,
        })
        .map_err(|error| format!("search: {error:?}"))?;
        for hit in page.hits {
            self.out.push_str(&format!(
                "{}\t{}\t{}\n",
                hit.path,
                hit.score,
                hit.snippet.unwrap_or_default()
            ));
        }
        Ok(())
    }

    fn source(&mut self, args: &[String]) -> Result<(), String> {
        let Some(arg) = args.first() else {
            return Err("source SCRIPT".into());
        };
        if self.source_depth >= MAX_SCRIPT_DEPTH {
            return Err("source: nesting too deep".into());
        }
        let path = self.resolve(arg)?;
        let bytes = self.read_file(&path)?;
        let text = String::from_utf8(bytes).map_err(|_| "source: script is not UTF-8")?;
        self.source_depth += 1;
        for line in text.lines() {
            self.run_line(line);
            if self.exit {
                break;
            }
        }
        self.source_depth -= 1;
        Ok(())
    }

    fn set_env(&mut self, args: &[String]) -> Result<(), String> {
        if args.is_empty() {
            for (key, value) in &self.env {
                self.out.push_str(&format!("{key}={value}\n"));
            }
            return Ok(());
        }
        for arg in args {
            let Some((key, value)) = arg.split_once('=') else {
                return Err(format!("export: expected KEY=VALUE, got {arg:?}"));
            };
            if key.is_empty() {
                return Err("export: empty variable name".into());
            }
            self.env.insert(key.into(), value.into());
            if key == "PWD" {
                self.cwd = self.resolve(value).unwrap_or_default();
            }
        }
        Ok(())
    }

    fn unset_env(&mut self, args: &[String]) -> Result<(), String> {
        for arg in args {
            self.env.remove(arg);
        }
        Ok(())
    }

    fn resolve(&self, value: &str) -> Result<String, String> {
        if value.contains('\\') {
            return Err("invalid path".into());
        }
        let value = if value == "~" {
            self.env.get("HOME").cloned().unwrap_or_else(|| "/".into())
        } else if let Some(rest) = value.strip_prefix("~/") {
            format!(
                "{}/{}",
                self.env.get("HOME").cloned().unwrap_or_else(|| "/".into()),
                rest
            )
        } else {
            value.to_string()
        };
        let mut parts: Vec<&str> = if value.starts_with('/') {
            Vec::new()
        } else {
            self.cwd
                .split('/')
                .filter(|part| !part.is_empty())
                .collect()
        };
        for part in value.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
        Ok(parts.join("/"))
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        let mut offset = 0;
        loop {
            let chunk = vfs::read(path, offset, READ_CHUNK)
                .map_err(|error| format!("{}: {error:?}", self.display(path)))?;
            let len = chunk.data.len() as u64;
            bytes.extend(chunk.data);
            if chunk.eof || len == 0 {
                break;
            }
            offset = offset.saturating_add(len);
        }
        Ok(bytes)
    }

    fn display_cwd(&self) -> String {
        self.display(&self.cwd)
    }

    fn display(&self, path: &str) -> String {
        if path.is_empty() {
            "/".into()
        } else {
            format!("/{path}")
        }
    }

    fn prompt(&self) -> String {
        let format = self
            .env
            .get("PS1")
            .cloned()
            .unwrap_or_else(|| "mush:\\w$ ".into());
        let mut prompt = String::new();
        let mut chars = format.chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                prompt.push(ch);
                continue;
            }
            match chars.next() {
                Some('u') => {
                    prompt.push_str(self.env.get("USER").map(String::as_str).unwrap_or("mush"))
                }
                Some('h') => prompt.push_str("mush"),
                Some('w') => prompt.push_str(&self.display_cwd()),
                Some('W') => prompt.push_str(
                    self.cwd
                        .rsplit('/')
                        .next()
                        .filter(|value| !value.is_empty())
                        .unwrap_or("/"),
                ),
                Some('$') => prompt.push('$'),
                Some('n') => prompt.push('\n'),
                Some('t') => prompt.push_str("00:00:00"),
                Some(other) => prompt.push(other),
                None => prompt.push('\\'),
            }
        }
        self.expand(&prompt)
    }

    fn expand(&self, value: &str) -> String {
        let mut out = String::new();
        let mut chars = value.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '$' {
                out.push(ch);
                continue;
            }
            if chars.peek() == Some(&'{') {
                chars.next();
                let mut name = String::new();
                for ch in chars.by_ref() {
                    if ch == '}' {
                        break;
                    }
                    name.push(ch);
                }
                out.push_str(self.env.get(&name).map(String::as_str).unwrap_or(""));
                continue;
            }
            if chars.peek() == Some(&'?') {
                chars.next();
                out.push('0');
                continue;
            }
            let mut name = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_alphanumeric() || ch == '_' {
                    name.push(ch);
                    chars.next();
                } else {
                    break;
                }
            }
            if name.is_empty() {
                out.push('$');
            } else {
                out.push_str(self.env.get(&name).map(String::as_str).unwrap_or(""));
            }
        }
        out
    }

    fn error(&mut self, error: &str) {
        self.out.push_str("mush: ");
        self.out.push_str(error);
        self.out.push('\n');
    }
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

fn split_words(line: &str) -> Result<Vec<String>, String> {
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
        return Err("unterminated quote".into());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

fn parse_redirection(
    mut words: Vec<String>,
) -> Result<(Vec<String>, Option<(bool, String)>), String> {
    let Some(index) = words.iter().position(|word| word == ">" || word == ">>") else {
        return Ok((words, None));
    };
    let append = words[index] == ">>";
    let Some(target) = words.get(index + 1).cloned() else {
        return Err("redirection needs a path".into());
    };
    if words
        .iter()
        .skip(index + 2)
        .any(|word| word == ">" || word == ">>")
    {
        return Err("only one output redirection is supported".into());
    }
    words.truncate(index);
    if words.is_empty() {
        return Err("redirection needs a command".into());
    }
    Ok((words, Some((append, target))))
}

fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            mark = v;
        } else if let Some(star_index) = star {
            p = star_index + 1;
            mark += 1;
            v = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

const HELP: &str = "\
mush — a sandboxed VFS shell

  help                    this text
  pwd                     print workspace path
  cd [DIR|-]              change directory
  ls [-l] [DIR]           list direct children
  mkdir DIR...            create directories
  rmdir DIR...            remove empty directories
  cat PATH                print a file
  write PATH [TEXT...]    create/replace a file
  append PATH [TEXT...]   append a line, preserving history
  touch PATH...           create empty files
  head [-n N] PATH        first lines
  tail [-n N] PATH        last lines
  grep [-i] PAT TEXT...  search file contents
  wc [-l|-c] PATH...      line/byte counts
  stat PATH               file metadata
  tree [DIR]              recursive listing
  rm [-r] PATH...         unlink files or recursively remove a directory
  mv FROM TO              rename a file
  cp FROM TO              copy bytes to a new version
  search QUERY            fuzzy index search
  source SCRIPT           run a Mush script from the VFS
  echo TEXT               print text
  export KEY=VALUE        set an environment variable
  unset KEY...            remove environment variables
  env                     print the environment
  set [KEY=VALUE]         print or set variables
  exit                    leave
";

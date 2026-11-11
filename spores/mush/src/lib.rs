//! Mush Spore: a sandboxed shell over `soil:vfs` and `soil:index`.
//!
//! The native host owns authority and calls one line at a time. This component
//! owns parsing, `PWD`/`OLDPWD`, environment variables, startup files, and
//! command semantics. It has no host filesystem, process, or network access.

wit_bindgen::generate!({ path: "wit", world: "mush", generate_all });

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
};

use regex::{Regex, RegexBuilder};
use serde_json::Value;

use soil::command::command as command_abi;
use soil::index::index::{self, Mode, Query};
use soil::vfs::vfs::{self, Kind};
use soil::vfs_mut::vfs_mut;

const MAX_SCRIPT_DEPTH: u8 = 8;
const READ_CHUNK: u32 = 64 * 1024;
const MAX_TGREP_FILES: usize = 20_000;
const MAX_TGREP_MATCHES: usize = 20_000;

static SHELL: Mutex<Option<Shell>> = Mutex::new(None);

struct Component;

export!(Component);

impl Guest for Component {
    fn start(args: Vec<String>) -> Result<Response, String> {
        let mut shell = Shell::new(args);
        shell.refresh_command_path();
        let output = shell.load_startup();
        let prompt = shell.prompt();
        let exit = shell.exit;
        let stderr = std::mem::take(&mut shell.err);
        *SHELL.lock().map_err(|_| "shell state poisoned")? = Some(shell);
        Ok(Response {
            output,
            stderr,
            exit,
            prompt,
            status: 0,
        })
    }

    fn execute(line: String) -> Result<Response, String> {
        let mut guard = SHELL.lock().map_err(|_| "shell state poisoned")?;
        let shell = guard.as_mut().ok_or("shell not started")?;
        shell.run_line(&line);
        Ok(Response {
            output: std::mem::take(&mut shell.out),
            stderr: std::mem::take(&mut shell.err),
            exit: shell.exit,
            prompt: shell.prompt(),
            status: shell.status,
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

struct TgrepOptions {
    pattern: String,
    extra_patterns: Vec<String>,
    paths: Vec<String>,
    ignore_case: Option<bool>,
    fixed: bool,
    word: bool,
    invert: bool,
    files_only: bool,
    files_without_match: bool,
    count: bool,
    only_matching: bool,
    line_numbers: bool,
    with_filename: Option<bool>,
    quiet: bool,
    no_index: bool,
    glob: Option<String>,
    max_count: usize,
}

impl TgrepOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            pattern: String::new(),
            extra_patterns: Vec::new(),
            paths: Vec::new(),
            ignore_case: None,
            fixed: false,
            word: false,
            invert: false,
            files_only: false,
            files_without_match: false,
            count: false,
            only_matching: false,
            line_numbers: true,
            with_filename: None,
            quiet: false,
            no_index: false,
            glob: None,
            max_count: 0,
        };
        let mut index = 0;
        let mut pattern_seen = false;
        let mut after_dashdash = false;
        while index < args.len() {
            let arg = &args[index];
            if !after_dashdash && arg == "--" {
                after_dashdash = true;
                index += 1;
                continue;
            }
            if !after_dashdash && arg.starts_with('-') && arg != "-" {
                match arg.as_str() {
                    "-i" => options.ignore_case = Some(true),
                    "-s" => options.ignore_case = Some(false),
                    "-F" => options.fixed = true,
                    "-w" => options.word = true,
                    "-v" => options.invert = true,
                    "-l" => options.files_only = true,
                    "--files-without-match" => options.files_without_match = true,
                    "-c" => options.count = true,
                    "-o" => options.only_matching = true,
                    "-n" => options.line_numbers = true,
                    "-N" => options.line_numbers = false,
                    "-H" => options.with_filename = Some(true),
                    "-I" => options.with_filename = Some(false),
                    "-q" => options.quiet = true,
                    "--no-index" => options.no_index = true,
                    "-e" => {
                        index += 1;
                        let value = args.get(index).ok_or("tgrep: -e needs a pattern")?;
                        options.extra_patterns.push(value.clone());
                    }
                    "-m" => {
                        index += 1;
                        let value = args.get(index).ok_or("tgrep: -m needs a number")?;
                        options.max_count = value
                            .parse()
                            .map_err(|_| format!("tgrep: invalid max count {value:?}"))?;
                    }
                    "-g" => {
                        index += 1;
                        options.glob =
                            Some(args.get(index).ok_or("tgrep: -g needs a glob")?.clone());
                    }
                    other => return Err(format!("tgrep: unsupported option {other}")),
                }
                index += 1;
                continue;
            }
            if !pattern_seen {
                options.pattern = arg.clone();
                pattern_seen = true;
            } else {
                options.paths.push(arg.clone());
            }
            index += 1;
        }
        if !pattern_seen || options.pattern.is_empty() {
            return Err("tgrep PATTERN [PATH...]".into());
        }
        Ok(options)
    }

    fn regex(&self) -> Result<Regex, String> {
        let mut parts = Vec::new();
        for pattern in std::iter::once(&self.pattern).chain(self.extra_patterns.iter()) {
            let text = if self.fixed {
                regex::escape(pattern)
            } else {
                pattern.clone()
            };
            parts.push(text);
        }
        let mut pattern = parts.join("|");
        if self.word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        let case_insensitive = self
            .ignore_case
            .unwrap_or_else(|| pattern.chars().all(|ch| !ch.is_uppercase()));
        RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .size_limit(1 << 20)
            .build()
            .map_err(|error| format!("tgrep: invalid pattern: {error}"))
    }

    fn index_pattern(&self) -> String {
        // The host index is case-insensitive. It is a candidate filter only;
        // the local regex below makes the final case decision.
        let mut parts = Vec::new();
        for pattern in std::iter::once(&self.pattern).chain(self.extra_patterns.iter()) {
            parts.push(if self.fixed {
                regex::escape(pattern)
            } else {
                pattern.clone()
            });
        }
        let mut pattern = parts.join("|");
        if self.word {
            pattern = format!(r"\b(?:{pattern})\b");
        }
        pattern
    }
}

struct Shell {
    cwd: String,
    env: BTreeMap<String, String>,
    out: String,
    err: String,
    stdin: Vec<u8>,
    status: i32,
    exit: bool,
    source_depth: u8,
    expand_depth: u8,
    positional: Vec<String>,
    loop_signal: Option<LoopSignal>,
}

impl Shell {
    fn new(args: Vec<String>) -> Self {
        let mut env = BTreeMap::new();
        for (key, value) in [
            ("HOME", "/"),
            ("PWD", "/"),
            ("OLDPWD", "/"),
            ("PATH", "/commands"),
            ("TERM", "dumb"),
            ("USER", "mush"),
            ("LOGNAME", "mush"),
            ("SHELL", "mush"),
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
            } else if let Some(value) = arg.strip_prefix("--principal=") {
                env.insert("MUSH_PRINCIPAL".into(), value.into());
                env.insert("USER".into(), value.into());
                env.insert("LOGNAME".into(), value.into());
            } else if let Some(value) = arg.strip_prefix("--desktop=") {
                env.insert("MUSH_DESKTOP".into(), value.into());
            } else if let Some(value) = arg.strip_prefix("--desktop-root=") {
                env.insert("MUSH_DESKTOP_ROOT".into(), value.into());
            } else if let Some(value) = arg.strip_prefix("--desktop-theme=") {
                env.insert("MUSH_DESKTOP_THEME".into(), value.into());
            }
        }
        Self {
            cwd: String::new(),
            env,
            out: String::new(),
            err: String::new(),
            stdin: Vec::new(),
            status: 0,
            exit: false,
            source_depth: 0,
            expand_depth: 0,
            positional: vec!["mush".into()],
            loop_signal: None,
        }
    }

    fn load_startup(&mut self) -> String {
        for candidate in [".mushrc", "mushrc"] {
            match self.read_file(candidate) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    self.run_script(&text);
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
        let tokens = match tokenize(line) {
            Ok(tokens) => tokens,
            Err(error) => {
                self.error(&error);
                self.status = 2;
                return;
            }
        };
        self.run_tokens(&tokens);
    }

    fn run_script(&mut self, text: &str) {
        let mut buffer = String::new();
        for line in text.lines() {
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(line);
            let Ok(tokens) = tokenize(&buffer) else {
                self.run_line(&buffer);
                buffer.clear();
                continue;
            };
            if tokens.is_empty() {
                buffer.clear();
                continue;
            }
            if block_depth(&tokens) == 0 {
                self.run_line(&buffer);
                buffer.clear();
                if self.exit {
                    return;
                }
            }
        }
        if !buffer.trim().is_empty() {
            self.run_line(&buffer);
        }
    }

    fn run_tokens(&mut self, tokens: &[Token]) -> i32 {
        if tokens.is_empty() {
            return self.status;
        }
        let chains = match parse_chains(tokens) {
            Ok(chains) => chains,
            Err(error) => {
                self.error(&error);
                self.status = 2;
                return self.status;
            }
        };
        for (operator, pipeline) in chains {
            let should_run = match operator {
                ChainOp::Always => true,
                ChainOp::And => self.status == 0,
                ChainOp::Or => self.status != 0,
            };
            if !should_run {
                continue;
            }
            self.status = if is_compound(&pipeline) {
                self.run_compound(&pipeline)
            } else {
                self.run_pipeline(&pipeline)
            };
            if self.exit || self.loop_signal.is_some() {
                break;
            }
        }
        self.status
    }

    fn run_compound(&mut self, tokens: &[Token]) -> i32 {
        match tokens.first().and_then(token_word) {
            Some("if") => self.run_if(tokens),
            Some("for") => self.run_for(tokens),
            Some("while") => self.run_while(tokens),
            _ => {
                self.error("unsupported compound command");
                2
            }
        }
    }

    fn run_if(&mut self, tokens: &[Token]) -> i32 {
        let Some(then_index) = find_block_marker(tokens, 1, &["then"]) else {
            self.error("if: missing then");
            return 2;
        };
        let Some(branch_index) = find_block_marker(tokens, then_index + 1, &["elif", "else", "fi"])
        else {
            self.error("if: missing fi");
            return 2;
        };
        let mut status = self.run_tokens(&tokens[1..then_index]);
        if status == 0 {
            return self.run_tokens(&tokens[then_index + 1..branch_index]);
        }
        let mut branch = branch_index;
        loop {
            match tokens.get(branch).and_then(token_word) {
                Some("fi") => return status,
                Some("else") => {
                    let Some(fi_index) = find_block_marker(tokens, branch + 1, &["fi"]) else {
                        self.error("if: missing fi");
                        return 2;
                    };
                    return self.run_tokens(&tokens[branch + 1..fi_index]);
                }
                Some("elif") => {
                    let Some(next_then) = find_block_marker(tokens, branch + 1, &["then"]) else {
                        self.error("elif: missing then");
                        return 2;
                    };
                    let Some(next_branch) =
                        find_block_marker(tokens, next_then + 1, &["elif", "else", "fi"])
                    else {
                        self.error("elif: missing fi");
                        return 2;
                    };
                    status = self.run_tokens(&tokens[branch + 1..next_then]);
                    if status == 0 {
                        return self.run_tokens(&tokens[next_then + 1..next_branch]);
                    }
                    branch = next_branch;
                }
                _ => {
                    self.error("if: malformed branch");
                    return 2;
                }
            }
        }
    }

    fn run_for(&mut self, tokens: &[Token]) -> i32 {
        let Some(name) = tokens.get(1).and_then(token_word) else {
            self.error("for: missing variable name");
            return 2;
        };
        let Some(do_index) = find_block_marker(tokens, 2, &["do"]) else {
            self.error("for: missing do");
            return 2;
        };
        let Some(done_index) = find_block_marker(tokens, do_index + 1, &["done"]) else {
            self.error("for: missing done");
            return 2;
        };
        let mut values = Vec::new();
        for token in &tokens[2..do_index] {
            if let Token::Word(word) = token {
                if word.text != "in" {
                    values.push(word.text.clone());
                }
            }
        }
        let mut status = 0;
        for value in values {
            let value = self.expand_dynamic(&value);
            self.env.insert(name.to_string(), value);
            status = self.run_tokens(&tokens[do_index + 1..done_index]);
            if self.exit {
                break;
            }
            match self.loop_signal.take() {
                Some(LoopSignal::Break) => break,
                Some(LoopSignal::Continue) => continue,
                None => {}
            }
        }
        status
    }

    fn run_while(&mut self, tokens: &[Token]) -> i32 {
        let Some(do_index) = find_block_marker(tokens, 1, &["do"]) else {
            self.error("while: missing do");
            return 2;
        };
        let Some(done_index) = find_block_marker(tokens, do_index + 1, &["done"]) else {
            self.error("while: missing done");
            return 2;
        };
        let mut status = 0;
        for _ in 0..10_000 {
            status = self.run_tokens(&tokens[1..do_index]);
            if status != 0 {
                break;
            }
            status = self.run_tokens(&tokens[do_index + 1..done_index]);
            if self.exit {
                break;
            }
            match self.loop_signal.take() {
                Some(LoopSignal::Break) => break,
                Some(LoopSignal::Continue) => continue,
                None => {}
            }
            if status != 0 {
                break;
            }
        }
        status
    }

    fn run_pipeline(&mut self, tokens: &[Token]) -> i32 {
        let segments = split_tokens(tokens, Op::Pipe);
        let mut input = Vec::new();
        let mut status = 0;
        let last_index = segments.len().saturating_sub(1);
        for (index, segment) in segments.iter().enumerate() {
            let parsed = match parse_segment(segment) {
                Ok(parsed) => parsed,
                Err(error) => {
                    self.error(&error);
                    return 2;
                }
            };
            let mut words = Vec::new();
            for word in &parsed.words {
                match self.expand_word(word) {
                    Ok(expanded) => words.extend(expanded),
                    Err(error) => {
                        self.error(&error);
                        return 2;
                    }
                }
            }
            let Some(command) = words.first().map(String::as_str) else {
                self.error("pipeline: empty command");
                return 2;
            };
            let args = words[1..].to_vec();
            if let Some(target) = parsed.stdin {
                let target = self.expand_literal(&target);
                let path = match self.resolve(&target) {
                    Ok(path) => path,
                    Err(error) => {
                        self.error(&error);
                        return 2;
                    }
                };
                input = match self.read_file(&path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        self.error(&error);
                        return 2;
                    }
                };
            }
            let saved = std::mem::take(&mut self.out);
            let saved_err = std::mem::take(&mut self.err);
            self.stdin = std::mem::take(&mut input);
            let result = self.run_simple(command, &args);
            let mut produced = std::mem::replace(&mut self.out, saved);
            let mut produced_err = std::mem::replace(&mut self.err, saved_err);
            self.stdin.clear();
            let code = match result {
                Ok(code) => Some(code),
                Err(error) => {
                    produced_err.push_str("mush: ");
                    produced_err.push_str(&error);
                    produced_err.push('\n');
                    status = 1;
                    None
                }
            };
            if parsed.stderr_to_stdout {
                produced.push_str(&produced_err);
                produced_err.clear();
            } else if let Some((append, target)) = parsed.stderr {
                let target = self.expand_literal(&target);
                if let Err(error) = self.redirect(&target, append, produced_err.as_bytes()) {
                    produced_err.clear();
                    produced_err.push_str("mush: ");
                    produced_err.push_str(&error);
                    produced_err.push('\n');
                    status = 1;
                }
            } else {
                self.err.push_str(&produced_err);
            }
            if let Some(code) = code {
                status = code;
            }
            if let Some((append, target)) = parsed.stdout {
                let target = self.expand_literal(&target);
                if let Err(error) = self.redirect(&target, append, produced.as_bytes()) {
                    self.error(&error);
                    status = 1;
                    break;
                }
            } else if index == last_index {
                self.out.push_str(&produced);
            } else {
                input = produced.into_bytes();
            }
            if code.is_none() {
                break;
            }
        }
        status
    }

    fn run_simple(&mut self, command: &str, args: &[String]) -> Result<i32, String> {
        match command {
            "help" => {
                self.out.push_str(HELP);
                self.out.push('\n');
                Ok(0)
            }
            "true" | ":" => Ok(0),
            "false" => Ok(1),
            "break" => {
                self.loop_signal = Some(LoopSignal::Break);
                Ok(0)
            }
            "continue" => {
                self.loop_signal = Some(LoopSignal::Continue);
                Ok(0)
            }
            "exit" | "quit" => {
                let code = match args.first() {
                    Some(value) => value
                        .parse::<i32>()
                        .map_err(|_| format!("exit: invalid status {value:?}"))?,
                    None => self.status,
                };
                self.exit = true;
                Ok(code)
            }
            "pwd" => {
                self.out.push_str(&self.display_cwd());
                self.out.push('\n');
                Ok(0)
            }
            "cd" => self.cd(args.first().map(String::as_str)).map(|_| 0),
            "ls" => self.ls(args).map(|_| 0),
            "mkdir" => self.mkdir(args).map(|_| 0),
            "find" => self.find(args).map(|_| 0),
            "rmdir" => self.rmdir(args).map(|_| 0),
            "cat" => self.cat(args).map(|_| 0),
            "write" => self.write(args).map(|_| 0),
            "append" => self.append(args).map(|_| 0),
            "touch" => self.touch(args).map(|_| 0),
            "head" => self.head(args).map(|_| 0),
            "tail" => self.tail(args).map(|_| 0),
            "grep" => self.grep(args).map(|_| 0),
            "tgrep" => self.tgrep(args),
            "wc" => self.wc(args).map(|_| 0),
            "stat" => self.stat(args).map(|_| 0),
            "tree" => self.tree(args).map(|_| 0),
            "sub" => self.sub(args).map(|_| 0),
            "diff" => self.diff(args),
            "json" => self.json(args).map(|_| 0),
            "rm" => self.rm(args).map(|_| 0),
            "mv" => self.mv(args).map(|_| 0),
            "cp" => self.cp(args).map(|_| 0),
            "search" => self.search(args).map(|_| 0),
            "commands" => self.list_commands(args).map(|_| 0),
            "which" => self.which(args).map(|_| 0),
            "source" => {
                self.source(args)?;
                Ok(self.status)
            }
            "echo" => {
                self.out.push_str(&args.join(" "));
                self.out.push('\n');
                Ok(0)
            }
            "printf" => self.printf(args).map(|_| 0),
            "test" => self.test_status(args),
            "[" => self.test_bracket(args),
            "read" => self.read(args),
            "export" | "set" => self.set_env(args).map(|_| 0),
            "unset" => self.unset_env(args).map(|_| 0),
            "env" => {
                for (key, value) in &self.env {
                    self.out.push_str(&format!("{key}={value}\n"));
                }
                Ok(0)
            }
            other => self.run_external(other, args),
        }
    }

    fn redirect(&mut self, target: &str, append: bool, data: &[u8]) -> Result<(), String> {
        let path = self.resolve(target)?;
        let (mut bytes, version) = match vfs::stat(&path) {
            Ok(entry) if entry.kind == Kind::Directory => {
                return Err(format!("redirect: {} is a directory", self.display(&path)));
            }
            Ok(entry) if append => (self.read_file(&path)?, Some(entry.version)),
            Ok(_) => (Vec::new(), None),
            Err(_) => (Vec::new(), None),
        };
        bytes.extend_from_slice(data);
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
        if args.is_empty() {
            self.out.push_str(&String::from_utf8_lossy(&self.stdin));
            return Ok(());
        }
        for arg in args {
            let path = self.resolve(arg)?;
            let bytes = self.read_file(&path)?;
            self.out.push_str(&String::from_utf8_lossy(&bytes));
        }
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
        let text = match path {
            Some(path) => {
                let path = self.resolve(path)?;
                String::from_utf8_lossy(&self.read_file(&path)?).to_string()
            }
            None => String::from_utf8_lossy(&self.stdin).to_string(),
        };
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
            return Err("grep [-i] PATTERN [PATH...]".into());
        };
        let needle = if ignore_case {
            pattern.to_ascii_lowercase()
        } else {
            pattern.clone()
        };
        let paths = &args[index + 1..];
        if paths.is_empty() {
            for line in String::from_utf8_lossy(&self.stdin).lines() {
                let haystack = if ignore_case {
                    line.to_ascii_lowercase()
                } else {
                    line.to_string()
                };
                if haystack.contains(&needle) {
                    self.out.push_str(line);
                    self.out.push('\n');
                }
            }
        } else {
            for arg in paths {
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
            let bytes = &self.stdin;
            let lines = bytes.iter().filter(|byte| **byte == b'\n').count();
            if lines_only && !bytes_only {
                self.out.push_str(&format!("{lines}\n"));
            } else if bytes_only && !lines_only {
                self.out.push_str(&format!("{}\n", bytes.len()));
            } else {
                self.out.push_str(&format!("{lines}\t{}\n", bytes.len()));
            }
            return Ok(());
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

    /// Regex replacement inside a file, in place, without a host `sed`.
    ///
    /// Multi-line mode, so `^` and `$` mean the start and end of a line the way
    /// they do in `sed`; that is also what makes this a faithful target for a
    /// `sed -i 's/…/…/g'` the host would otherwise run over an overlay.
    fn sub(&mut self, args: &[String]) -> Result<(), String> {
        if args.len() < 3 {
            return Err("sub PATTERN REPLACEMENT PATH...".into());
        }
        let pattern = RegexBuilder::new(&args[0])
            .multi_line(true)
            .build()
            .map_err(|error| format!("sub: bad pattern: {error}"))?;
        let replacement = args[1].as_str();
        for arg in &args[2..] {
            let path = self.resolve(arg)?;
            let entry = vfs::stat(&path)
                .map_err(|error| format!("sub {}: {error:?}", self.display(&path)))?;
            if entry.kind == Kind::Directory {
                return Err(format!("sub: {} is a directory", self.display(&path)));
            }
            let bytes = self.read_file(&path)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("sub: {} is not UTF-8 text", self.display(&path)))?;
            let hits = pattern.find_iter(&text).count();
            if hits == 0 {
                self.out.push_str(&format!("{}: unchanged\n", self.display(&path)));
                continue;
            }
            let replaced = pattern.replace_all(&text, replacement).into_owned();
            // Versioned write: if someone else moved the file first, this fails
            // instead of silently clobbering their version.
            vfs_mut::write(
                &path,
                replaced.as_bytes(),
                media_type(&path),
                Some(entry.version),
            )
            .map_err(|error| format!("sub {}: {error:?}", self.display(&path)))?;
            self.out
                .push_str(&format!("{}: {hits} replacement(s)\n", self.display(&path)));
        }
        Ok(())
    }

    /// `diff [-q] [-U N] LEFT RIGHT` — unified line diff between two VFS files.
    ///
    /// The status follows the diff convention: 0 identical, 1 different, 2 could
    /// not be read. That is also why failures go to stderr instead of through
    /// `Err`, which the shell would report as status 1 and read as "differ".
    fn diff(&mut self, args: &[String]) -> Result<i32, String> {
        let mut quiet = false;
        let mut context = 3usize;
        let mut paths: Vec<&str> = Vec::new();
        let mut literal = false;
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            if literal || arg == "-" || !arg.starts_with('-') {
                paths.push(arg);
            } else {
                match arg {
                    "--" => literal = true,
                    "-q" | "--brief" => quiet = true,
                    "-u" | "--unified" => {}
                    "-U" => {
                        let value = args.get(index + 1).ok_or("diff -U N")?;
                        context = diff_context(value)?;
                        index += 1;
                    }
                    other => match other.strip_prefix("-U") {
                        Some(value) => context = diff_context(value)?,
                        None => return Err(format!("diff: unsupported option {other}")),
                    },
                }
            }
            index += 1;
        }
        if paths.len() != 2 {
            return Err("diff [-q] [-U N] LEFT RIGHT".into());
        }
        let left = self.resolve(paths[0])?;
        let right = self.resolve(paths[1])?;
        let Some(left_bytes) = self.diff_read(&left) else {
            return Ok(2);
        };
        let Some(right_bytes) = self.diff_read(&right) else {
            return Ok(2);
        };
        let left_name = self.display(&left);
        let right_name = self.display(&right);
        let same = left_bytes == right_bytes;
        if quiet {
            if !same {
                self.out
                    .push_str(&format!("Files {left_name} and {right_name} differ\n"));
            }
            return Ok(if same { 0 } else { 1 });
        }
        if same {
            return Ok(0);
        }
        if is_binary(&left_bytes) || is_binary(&right_bytes) {
            self.out
                .push_str(&format!("Binary files {left_name} and {right_name} differ\n"));
            return Ok(1);
        }
        let left_lines = split_lines(&left_bytes);
        let right_lines = split_lines(&right_bytes);
        let ops = diff_ops(&left_lines, &right_lines);
        self.out.push_str(&format!("--- {left_name}\n+++ {right_name}\n"));
        let (old_at, new_at) = line_numbers(&ops);
        for (start, stop) in hunk_ranges(&ops, context) {
            self.out
                .push_str(&diff_hunk_header(&ops, &old_at, &new_at, start, stop));
            for op in &ops[start..stop] {
                match op {
                    LineOp::Equal(line) => push_diff_line(&mut self.out, ' ', left_lines[*line]),
                    LineOp::Delete(line) => push_diff_line(&mut self.out, '-', left_lines[*line]),
                    LineOp::Insert(line) => push_diff_line(&mut self.out, '+', right_lines[*line]),
                }
            }
        }
        Ok(1)
    }

    /// One side of a diff, with directories and unreadable paths reported on
    /// stderr so that status 1 keeps meaning "the files differ".
    fn diff_read(&mut self, path: &str) -> Option<Vec<u8>> {
        let problem = match vfs::stat(path) {
            Ok(entry) if entry.kind == Kind::Directory => {
                format!("{} is a directory", self.display(path))
            }
            Ok(_) => match self.read_file(path) {
                Ok(bytes) => return Some(bytes),
                Err(error) => error,
            },
            Err(error) => format!("{}: {error:?}", self.display(path)),
        };
        self.err.push_str(&format!("mush: diff: {problem}\n"));
        None
    }

    /// `json` — read and edit JSON documents in the VFS.
    ///
    /// Every mutation reads the version it started from and passes it to
    /// `vfs::write`, so a concurrent edit fails the write instead of being
    /// clobbered — the same contract `sub` uses.
    fn json(&mut self, args: &[String]) -> Result<(), String> {
        let Some(verb) = args.first() else {
            return Err("json get|set|del|merge|pretty".into());
        };
        let rest = &args[1..];
        match verb.as_str() {
            "get" => self.json_get(rest),
            "set" => self.json_set(rest),
            "del" | "rm" => self.json_del(rest),
            "merge" => self.json_merge(rest),
            "pretty" | "fmt" => self.json_pretty(rest),
            other => Err(format!(
                "json: unknown verb {other:?}; use get, set, del, merge, or pretty"
            )),
        }
    }

    /// `json get [-j] FILE [PATH]` — PATH defaults to the whole document.
    ///
    /// A string value prints raw, because the result is usually fed to another
    /// command; `-j` prints the JSON encoding instead so `"null"` and `null`
    /// stay distinguishable.
    fn json_get(&mut self, args: &[String]) -> Result<(), String> {
        let mut as_json = false;
        let mut positional: Vec<&str> = Vec::new();
        for arg in args {
            match arg.as_str() {
                "-j" | "--json" => as_json = true,
                other => positional.push(other),
            }
        }
        if positional.is_empty() || positional.len() > 2 {
            return Err("json get [-j] FILE [PATH]".into());
        }
        let path = self.resolve(positional[0])?;
        let query = positional.get(1).copied().unwrap_or("$");
        let segments = parse_json_path(query)?;
        let (_, _, document) = self.json_read(&path)?;
        let Some(found) = json_lookup(&document, &segments) else {
            return Err(format!(
                "json get {}: no value at {query:?}",
                self.display(&path)
            ));
        };
        match (as_json, found) {
            (false, Value::String(text)) => {
                self.out.push_str(text);
                self.out.push('\n');
            }
            (_, value) => {
                self.out.push_str(&value.to_string());
                self.out.push('\n');
            }
        }
        Ok(())
    }

    /// `json set FILE PATH JSON|-` — VALUE must be a JSON literal, and `-` reads
    /// the document fragment from stdin.
    ///
    /// Only the final path segment is created: a missing parent is an error, so
    /// a typo in a key name says so instead of growing a second copy of the tree.
    fn json_set(&mut self, args: &[String]) -> Result<(), String> {
        if args.len() != 3 {
            return Err("json set FILE PATH JSON|-".into());
        }
        let path = self.resolve(&args[0])?;
        let segments = parse_json_path(&args[1])?;
        let literal = if args[2] == "-" {
            let stdin = std::mem::take(&mut self.stdin);
            String::from_utf8(stdin).map_err(|_| "json set: stdin is not UTF-8".to_string())?
        } else {
            args[2].clone()
        };
        let value: Value = serde_json::from_str(&literal).map_err(|error| {
            format!("json set: VALUE must be JSON ({error}); a string needs its JSON quotes: '\"text\"'")
        })?;
        let (original, version, mut document) = self.json_read(&path)?;
        json_assign(&mut document, &segments, value)
            .map_err(|error| format!("json set {}: {error}", self.display(&path)))?;
        self.json_write(&path, &document, &original, version, "set")
    }

    /// `json del FILE PATH` — remove an object key or an array element.
    fn json_del(&mut self, args: &[String]) -> Result<(), String> {
        if args.len() != 2 {
            return Err("json del FILE PATH".into());
        }
        let path = self.resolve(&args[0])?;
        let segments = parse_json_path(&args[1])?;
        let (original, version, mut document) = self.json_read(&path)?;
        json_remove(&mut document, &segments)
            .map_err(|error| format!("json del {}: {error}", self.display(&path)))?;
        self.json_write(&path, &document, &original, version, "del")
    }

    /// `json merge TARGET SOURCE...` — deep-merge each source into TARGET.
    fn json_merge(&mut self, args: &[String]) -> Result<(), String> {
        if args.len() < 2 {
            return Err("json merge TARGET SOURCE...".into());
        }
        let target = self.resolve(&args[0])?;
        let (original, version, mut document) = self.json_read(&target)?;
        for source in &args[1..] {
            let source = self.resolve(source)?;
            let (_, _, patch) = self.json_read(&source)?;
            json_merge_into(&mut document, patch);
        }
        self.json_write(&target, &document, &original, version, "merge")
    }

    /// `json pretty [--indent N] [FILE|-]` — reformat without writing.
    fn json_pretty(&mut self, args: &[String]) -> Result<(), String> {
        let mut width = 2usize;
        let mut positional: Vec<&str> = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--indent" | "-i" => {
                    let value = args.get(index + 1).ok_or("json pretty --indent N")?;
                    width = value
                        .parse()
                        .map_err(|_| format!("json pretty: {value:?} is not a number"))?;
                    index += 1;
                }
                other => positional.push(other),
            }
            index += 1;
        }
        if positional.len() > 1 {
            return Err("json pretty [--indent N] [FILE|-]".into());
        }
        let document = match positional.first() {
            None | Some(&"-") => {
                let stdin = std::mem::take(&mut self.stdin);
                let text = String::from_utf8(stdin)
                    .map_err(|_| "json pretty: stdin is not UTF-8".to_string())?;
                serde_json::from_str(&text).map_err(|error| format!("json pretty: {error}"))?
            }
            Some(file) => {
                let path = self.resolve(file)?;
                let (_, _, document) = self.json_read(&path)?;
                document
            }
        };
        self.out.push_str(&json_pretty_text(&document, width));
        self.out.push('\n');
        Ok(())
    }

    /// Read a document together with the version it was read at, so the write
    /// that follows can fail instead of clobbering a concurrent edit.
    fn json_read(&mut self, path: &str) -> Result<(String, u64, Value), String> {
        let entry = vfs::stat(path).map_err(|error| format!("json {}: {error:?}", self.display(path)))?;
        if entry.kind == Kind::Directory {
            return Err(format!("json: {} is a directory", self.display(path)));
        }
        let bytes = self.read_file(path)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("json {}: not UTF-8 text", self.display(path)))?;
        let value = serde_json::from_str(&text)
            .map_err(|error| format!("json {}: {error}", self.display(path)))?;
        Ok((text, entry.version, value))
    }

    fn json_write(
        &mut self,
        path: &str,
        document: &Value,
        original: &str,
        version: u64,
        action: &str,
    ) -> Result<(), String> {
        let rendered = json_render(document, original);
        let result = vfs_mut::write(path, rendered.as_bytes(), media_type(path), Some(version))
            .map_err(|error| format!("json {action} {}: {error:?}", self.display(path)))?;
        self.out
            .push_str(&format!("{}: {action} (v{})\n", self.display(path), result.version));
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

    /// Walk the VFS and print matching paths.
    ///
    /// Two forms, because both are useful: `find ROOT [PATTERN]` (the original
    /// positional one, matched against the name or the full path) and the
    /// POSIX-ish `find ROOT -name GLOB -type f|d -maxdepth N`.
    fn find(&mut self, args: &[String]) -> Result<(), String> {
        let mut root_arg: Option<&str> = None;
        let mut positional: Option<&str> = None;
        let mut name: Option<&str> = None;
        let mut want: Option<Kind> = None;
        let mut max_depth = 64usize;
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "-name" => {
                    name = Some(args.get(index + 1).ok_or("find -name PATTERN")?);
                    index += 2;
                }
                "-type" => {
                    let value = args.get(index + 1).ok_or("find -type f|d")?;
                    want = Some(match value.as_str() {
                        "f" => Kind::File,
                        "d" => Kind::Directory,
                        other => return Err(format!("find -type: {other} is not f or d")),
                    });
                    index += 2;
                }
                "-maxdepth" => {
                    max_depth = args
                        .get(index + 1)
                        .ok_or("find -maxdepth N")?
                        .parse()
                        .map_err(|_| "find -maxdepth N")?;
                    index += 2;
                }
                other => {
                    if root_arg.is_none() {
                        root_arg = Some(other);
                    } else if positional.is_none() {
                        positional = Some(other);
                    } else {
                        return Err(
                            "find [ROOT] [PATTERN] [-name GLOB] [-type f|d] [-maxdepth N]".into()
                        );
                    }
                    index += 1;
                }
            }
        }
        let pattern = name.or(positional);
        let root = self.resolve(root_arg.unwrap_or("."))?;
        let mut stack = vec![(root, 0usize)];
        let mut count = 0usize;
        while let Some((dir, depth)) = stack.pop() {
            let mut offset = 0;
            loop {
                let page =
                    vfs::ls(&dir, offset, 512).map_err(|error| format!("find: {error:?}"))?;
                for entry in page.entries {
                    count += 1;
                    if count > 20_000 {
                        return Err("find: too many entries".into());
                    }
                    // Depth is the entry's own, not its parent's: `-maxdepth 1`
                    // must not print a grandchild just because it descended.
                    let entry_depth = depth + 1;
                    if entry_depth > max_depth {
                        continue;
                    }
                    let path = self.display(&entry.path);
                    let type_matches = want.map(|kind| entry.kind == kind).unwrap_or(true);
                    let name_matches = pattern.is_none_or(|pattern| {
                        glob_match(pattern, &entry.name) || glob_match(pattern, &path)
                    });
                    if type_matches && name_matches {
                        self.out.push_str(&path);
                        if entry.kind == Kind::Directory {
                            self.out.push('/');
                        }
                        self.out.push('\n');
                    }
                    if entry.kind == Kind::Directory && entry_depth < max_depth {
                        stack.push((entry.path, entry_depth));
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

    fn tgrep(&mut self, args: &[String]) -> Result<i32, String> {
        let options = TgrepOptions::parse(args)?;
        let regex = options.regex()?;
        let index_pattern = options.index_pattern();
        let default_root = self.cwd.clone();
        let roots = if options.paths.is_empty() {
            vec![default_root]
        } else {
            options.paths.clone()
        };
        let mut matched = false;
        let mut emitted = 0usize;
        for root_arg in roots {
            let root = if options.paths.is_empty() {
                root_arg.clone()
            } else {
                self.resolve(&root_arg)?
            };
            let (paths, is_file) = match vfs::stat(&root) {
                Ok(entry) if entry.kind == Kind::File => (vec![root.clone()], true),
                Ok(_) => {
                    let paths = if options.no_index || options.invert || options.files_without_match
                    {
                        self.collect_vfs_files(&root)?
                    } else {
                        match self.tgrep_candidates(&index_pattern, &root) {
                            Ok(paths) => paths,
                            Err(_) => self.collect_vfs_files(&root)?,
                        }
                    };
                    (paths, false)
                }
                Err(error) => {
                    return Err(format!("tgrep {}: {error:?}", self.display(&root)));
                }
            };
            for path in paths {
                if let Some(glob) = &options.glob {
                    let name = path.rsplit('/').next().unwrap_or(&path);
                    if !glob_match(glob, &path) && !glob_match(glob, name) {
                        continue;
                    }
                }
                let bytes = match self.read_file(&path) {
                    Ok(bytes) => bytes,
                    Err(error) if is_file => {
                        return Err(format!("tgrep {}: {error}", self.display(&path)));
                    }
                    Err(_) => continue,
                };
                if bytes.len() > 8 * 1024 * 1024 || bytes.contains(&0) {
                    continue;
                }
                let text = String::from_utf8_lossy(&bytes);
                let display = self.display(&path);
                let show_name = options.with_filename.unwrap_or(true);
                let mut file_matches = 0usize;
                for (line_index, line) in text.lines().enumerate() {
                    let line_matches = regex.find_iter(line).collect::<Vec<_>>();
                    let line_has_match = !line_matches.is_empty();
                    let wanted = if options.invert {
                        !line_has_match
                    } else {
                        line_has_match
                    };
                    if !wanted {
                        continue;
                    }
                    matched = true;
                    if options.quiet {
                        break;
                    }
                    if options.files_only {
                        self.out.push_str(&display);
                        self.out.push('\n');
                        emitted += 1;
                        break;
                    }
                    file_matches += 1;
                    if options.count || options.files_without_match {
                        if options.max_count > 0 && file_matches >= options.max_count {
                            break;
                        }
                        continue;
                    }
                    if options.only_matching && !options.invert {
                        for found in &line_matches {
                            let prefix = if show_name {
                                format!("{display}:")
                            } else {
                                String::new()
                            };
                            if options.line_numbers {
                                self.out.push_str(&format!(
                                    "{prefix}{}:{}\n",
                                    line_index + 1,
                                    &line[found.start()..found.end()]
                                ));
                            } else {
                                self.out.push_str(&format!(
                                    "{prefix}{}\n",
                                    &line[found.start()..found.end()]
                                ));
                            }
                            emitted += 1;
                        }
                    } else {
                        let prefix = if show_name {
                            format!("{display}:")
                        } else {
                            String::new()
                        };
                        if options.line_numbers {
                            self.out
                                .push_str(&format!("{prefix}{}:{line}\n", line_index + 1));
                        } else {
                            self.out.push_str(&format!("{prefix}{line}\n"));
                        }
                        emitted += 1;
                    }
                    if emitted > MAX_TGREP_MATCHES {
                        return Err("tgrep: output limit exceeded".into());
                    }
                    if options.max_count > 0 && file_matches >= options.max_count {
                        break;
                    }
                }
                if options.count && file_matches > 0 {
                    self.out.push_str(&format!("{display}:{file_matches}\n"));
                    emitted += 1;
                }
                if options.files_without_match && file_matches == 0 {
                    self.out.push_str(&display);
                    self.out.push('\n');
                    emitted += 1;
                }
                if options.quiet && matched {
                    break;
                }
                if emitted > MAX_TGREP_MATCHES {
                    return Err("tgrep: output limit exceeded".into());
                }
            }
            if options.quiet && matched {
                break;
            }
        }
        Ok(if matched { 0 } else { 1 })
    }

    fn tgrep_candidates(&self, pattern: &str, root: &str) -> Result<Vec<String>, String> {
        let mut paths = BTreeSet::new();
        let mut offset = 0u64;
        let mut retried = false;
        loop {
            let page = match index::search(&Query {
                text: pattern.to_string(),
                mode: Some(Mode::Regex),
                parent: None,
                media_type_prefix: None,
                offset,
                limit: 512,
            }) {
                Ok(page) => page,
                Err(index::Error::NotReady) if !retried => {
                    index::refresh().map_err(|error| format!("tgrep: index: {error:?}"))?;
                    retried = true;
                    continue;
                }
                Err(error) => return Err(format!("tgrep: index: {error:?}")),
            };
            let hit_count = page.hits.len();
            for hit in page.hits {
                if under_root(root, &hit.path) {
                    paths.insert(hit.path);
                }
            }
            if hit_count < 512 {
                break;
            }
            offset = offset.saturating_add(hit_count as u64);
            if paths.len() > MAX_TGREP_FILES {
                return Err("tgrep: too many candidate files".into());
            }
        }
        Ok(paths.into_iter().collect())
    }

    fn collect_vfs_files(&self, root: &str) -> Result<Vec<String>, String> {
        let mut files = BTreeSet::new();
        let mut stack = vec![root.to_string()];
        while let Some(dir) = stack.pop() {
            let mut offset = 0;
            loop {
                let page =
                    vfs::ls(&dir, offset, 512).map_err(|error| format!("tgrep: {error:?}"))?;
                for entry in page.entries {
                    if entry.kind == Kind::Directory {
                        if entry.name != ".fungos" {
                            stack.push(entry.path);
                        }
                    } else {
                        files.insert(entry.path);
                    }
                    if files.len() > MAX_TGREP_FILES {
                        return Err("tgrep: too many files".into());
                    }
                }
                match page.next {
                    Some(next) if next > offset => offset = next,
                    _ => break,
                }
            }
        }
        Ok(files.into_iter().collect())
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
        let saved = std::mem::replace(&mut self.positional, args.to_vec());
        self.source_depth += 1;
        self.run_script(&text);
        self.source_depth -= 1;
        self.positional = saved;
        Ok(())
    }

    fn refresh_command_path(&mut self) {
        let mut path = String::from("/commands");
        for name in command_abi::path() {
            path.push(':');
            path.push_str(&name);
        }
        self.env.insert("PATH".into(), path);
    }

    fn list_commands(&mut self, args: &[String]) -> Result<(), String> {
        if !args.is_empty() {
            return Err("commands".into());
        }
        for command in command_abi::commands() {
            self.out.push_str(&format!(
                "{}\t{}\t{}\n",
                command.name, command.version, command.summary
            ));
        }
        Ok(())
    }

    fn which(&mut self, args: &[String]) -> Result<(), String> {
        let Some(name) = args.first() else {
            return Err("which COMMAND".into());
        };
        if args.len() != 1 {
            return Err("which COMMAND".into());
        }
        let Some(command) = command_abi::commands()
            .into_iter()
            .find(|command| command.name == *name)
        else {
            return Err(format!("which: {name}: command not found"));
        };
        self.out.push_str(&format!(
            "{}\t{}\t{}\n",
            command.name, command.version, command.summary
        ));
        Ok(())
    }

    fn expand_word(&mut self, word: &Word) -> Result<Vec<String>, String> {
        let text = self.expand_literal(word);
        if !word.glob || !text.bytes().any(|byte| matches!(byte, b'*' | b'?' | b'[')) {
            return Ok(vec![text]);
        }
        match self.glob(&text) {
            Ok(matches) if !matches.is_empty() => Ok(matches),
            _ => Ok(vec![text]),
        }
    }

    fn expand_literal(&mut self, word: &Word) -> String {
        if word.expand {
            self.expand_dynamic(&word.text)
        } else {
            word.text.clone()
        }
    }

    fn glob(&self, pattern: &str) -> Result<Vec<String>, String> {
        let (prefix, base) = match pattern.rfind('/') {
            Some(index) => (&pattern[..=index], &pattern[index + 1..]),
            None => ("", pattern),
        };
        if base.is_empty() {
            return Ok(vec![pattern.to_string()]);
        }
        let dir_text = if prefix.is_empty() {
            "."
        } else {
            prefix.trim_end_matches('/')
        };
        let dir = self.resolve(dir_text)?;
        let mut matches = Vec::new();
        let mut offset = 0;
        loop {
            let page = vfs::ls(&dir, offset, 512).map_err(|error| format!("glob: {error:?}"))?;
            for entry in page.entries {
                if entry.name.starts_with('.') && !base.starts_with('.') {
                    continue;
                }
                if glob_match(base, &entry.name) {
                    matches.push(format!("{prefix}{}", entry.name));
                }
            }
            match page.next {
                Some(next) if next > offset => offset = next,
                _ => break,
            }
        }
        matches.sort();
        Ok(matches)
    }

    fn run_external(&mut self, name: &str, args: &[String]) -> Result<i32, String> {
        let cwd = if self.cwd.is_empty() {
            None
        } else {
            Some(self.cwd.clone())
        };
        let stdin = std::mem::take(&mut self.stdin);
        let output = command_abi::run(name, args, &stdin, cwd.as_deref())
            .map_err(|error| format!("{name}: {error}"))?;
        self.out.push_str(&String::from_utf8_lossy(&output.stdout));
        self.err.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok(output.code)
    }

    fn test_bracket(&self, args: &[String]) -> Result<i32, String> {
        let Some(last) = args.last() else {
            return Err("[ ... ]".into());
        };
        if last != "]" {
            return Err("[ ... ]: missing ]".into());
        }
        self.test_status(&args[..args.len() - 1])
    }

    fn test_status(&self, args: &[String]) -> Result<i32, String> {
        let value = self.eval_test(args)?;
        Ok(if value { 0 } else { 1 })
    }

    fn eval_test(&self, args: &[String]) -> Result<bool, String> {
        if args.is_empty() {
            return Ok(false);
        }
        if args[0] == "!" {
            return Ok(!self.eval_test(&args[1..])?);
        }
        if let Some(index) = args.iter().position(|arg| arg == "-a" || arg == "-o") {
            let left = self.eval_test(&args[..index])?;
            let right = self.eval_test(&args[index + 1..])?;
            return Ok(if args[index] == "-a" {
                left && right
            } else {
                left || right
            });
        }
        match args.len() {
            1 => Ok(!args[0].is_empty()),
            2 => {
                let value = &args[1];
                Ok(match args[0].as_str() {
                    "-e" => self.vfs_stat(value).is_some(),
                    "-f" => self
                        .vfs_stat(value)
                        .is_some_and(|entry| entry.kind == Kind::File),
                    "-d" => self
                        .vfs_stat(value)
                        .is_some_and(|entry| entry.kind == Kind::Directory),
                    "-s" => self.vfs_stat(value).is_some_and(|entry| entry.size > 0),
                    "-r" | "-w" | "-x" => self.vfs_stat(value).is_some(),
                    "-z" => value.is_empty(),
                    "-n" => !value.is_empty(),
                    other => return Err(format!("test: unknown unary operator {other}")),
                })
            }
            3 => {
                let left = &args[0];
                let right = &args[2];
                Ok(match args[1].as_str() {
                    "=" | "==" => left == right,
                    "!=" => left != right,
                    "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
                        let left = left
                            .parse::<i64>()
                            .map_err(|_| format!("test: expected integer, got {left:?}"))?;
                        let right = right
                            .parse::<i64>()
                            .map_err(|_| format!("test: expected integer, got {right:?}"))?;
                        match args[1].as_str() {
                            "-eq" => left == right,
                            "-ne" => left != right,
                            "-lt" => left < right,
                            "-le" => left <= right,
                            "-gt" => left > right,
                            "-ge" => left >= right,
                            _ => unreachable!(),
                        }
                    }
                    other => return Err(format!("test: unknown binary operator {other}")),
                })
            }
            _ => Err("test: too many arguments".into()),
        }
    }

    fn vfs_stat(&self, path: &str) -> Option<vfs::Entry> {
        let path = self.resolve(path).ok()?;
        vfs::stat(&path).ok()
    }

    fn read(&mut self, args: &[String]) -> Result<i32, String> {
        let mut index = 0;
        if args.first().is_some_and(|arg| arg == "-r") {
            index = 1;
        }
        let name = args.get(index).cloned().unwrap_or_else(|| "REPLY".into());
        if name.is_empty() {
            return Err("read: empty variable name".into());
        }
        if self.stdin.is_empty() {
            return Ok(1);
        }
        let take = self
            .stdin
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(self.stdin.len());
        let line = self.stdin.drain(..take).collect::<Vec<_>>();
        let value = String::from_utf8_lossy(&line)
            .trim_end_matches(['\n', '\r'])
            .to_string();
        self.env.insert(name, value);
        Ok(0)
    }

    fn printf(&mut self, args: &[String]) -> Result<(), String> {
        let Some(format) = args.first() else {
            return Err("printf FORMAT [ARG...]".into());
        };
        let mut out = String::new();
        let mut arg_index = 1;
        let mut chars = format.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('0') => out.push('\0'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                },
                '%' => match chars.next() {
                    Some('%') => out.push('%'),
                    Some('s') => {
                        out.push_str(args.get(arg_index).map(String::as_str).unwrap_or(""));
                        arg_index += 1;
                    }
                    Some('d' | 'i') => {
                        let value = args.get(arg_index).cloned().unwrap_or_default();
                        let number = value
                            .parse::<i64>()
                            .map_err(|_| format!("printf: expected integer, got {value:?}"))?;
                        out.push_str(&number.to_string());
                        arg_index += 1;
                    }
                    Some('b') => {
                        let value = args.get(arg_index).cloned().unwrap_or_default();
                        arg_index += 1;
                        let mut escaped = value.chars();
                        while let Some(ch) = escaped.next() {
                            if ch != '\\' {
                                out.push(ch);
                                continue;
                            }
                            match escaped.next() {
                                Some('n') => out.push('\n'),
                                Some('t') => out.push('\t'),
                                Some('r') => out.push('\r'),
                                Some('0') => out.push('\0'),
                                Some('\\') => out.push('\\'),
                                Some(other) => {
                                    out.push('\\');
                                    out.push(other);
                                }
                                None => out.push('\\'),
                            }
                        }
                    }
                    Some(other) => return Err(format!("printf: unsupported conversion %{other}")),
                    None => return Err("printf: trailing %".into()),
                },
                _ => out.push(ch),
            }
        }
        self.out.push_str(&out);
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
        self.expand_env(&prompt)
    }

    fn expand_env(&self, value: &str) -> String {
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
                out.push_str(&self.var_value(&name));
                continue;
            }
            if chars.peek() == Some(&'?') {
                chars.next();
                out.push_str(&self.status.to_string());
                continue;
            }
            if let Some(&next) = chars.peek() {
                if next.is_ascii_digit() {
                    let mut name = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_ascii_digit() {
                            name.push(ch);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push_str(&self.var_value(&name));
                    continue;
                }
                if matches!(next, '#' | '@' | '*') {
                    chars.next();
                    out.push_str(&self.var_value(&next.to_string()));
                    continue;
                }
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
                out.push_str(&self.var_value(&name));
            }
        }
        out
    }

    fn expand_dynamic(&mut self, value: &str) -> String {
        let mut out = String::new();
        let mut chars = value.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '$' {
                out.push(ch);
                continue;
            }
            if chars.peek() == Some(&'(') {
                chars.next();
                let mut script = String::new();
                let mut depth = 1usize;
                for ch in chars.by_ref() {
                    match ch {
                        '(' => {
                            depth += 1;
                            script.push(ch);
                        }
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            script.push(ch);
                        }
                        _ => script.push(ch),
                    }
                }
                if depth != 0 || self.expand_depth >= MAX_SCRIPT_DEPTH {
                    out.push_str("$(");
                    out.push_str(&script);
                    out.push(')');
                    continue;
                }
                let saved_out = std::mem::take(&mut self.out);
                let saved_status = self.status;
                let saved_stdin = std::mem::take(&mut self.stdin);
                self.expand_depth += 1;
                self.run_line(&script);
                self.expand_depth -= 1;
                let captured = std::mem::replace(&mut self.out, saved_out);
                self.status = saved_status;
                self.stdin = saved_stdin;
                out.push_str(captured.trim_end_matches('\n'));
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
                out.push_str(&self.var_value(&name));
                continue;
            }
            if chars.peek() == Some(&'?') {
                chars.next();
                out.push_str(&self.status.to_string());
                continue;
            }
            if let Some(&next) = chars.peek() {
                if next.is_ascii_digit() {
                    let mut name = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch.is_ascii_digit() {
                            name.push(ch);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push_str(&self.var_value(&name));
                    continue;
                }
                if matches!(next, '#' | '@' | '*') {
                    chars.next();
                    out.push_str(&self.var_value(&next.to_string()));
                    continue;
                }
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
                out.push_str(&self.var_value(&name));
            }
        }
        out
    }

    fn var_value(&self, name: &str) -> String {
        match name {
            "0" => self.positional.first().cloned().unwrap_or_default(),
            "#" => self.positional.len().saturating_sub(1).to_string(),
            "@" | "*" => self
                .positional
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            value if value.chars().all(|ch| ch.is_ascii_digit()) => value
                .parse::<usize>()
                .ok()
                .and_then(|index| self.positional.get(index).cloned())
                .unwrap_or_default(),
            _ => self.env.get(name).cloned().unwrap_or_default(),
        }
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

/// One step of a line-level edit script. The payload is a line index into the
/// left list (`Equal`, `Delete`) or the right list (`Insert`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineOp {
    Equal(usize),
    Delete(usize),
    Insert(usize),
}

/// Split a buffer into lines that keep their terminator, so a missing final
/// newline is content a diff can report rather than a detail it ignores.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split_inclusive(|byte| *byte == b'\n').collect()
}

/// A NUL near the start means "not text"; that is how GNU diff decides too, and
/// it keeps a binary file from being printed as mojibake.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

/// Both sides of a diff already live in Spore memory, so the table is bounded.
/// Past the cap the script degrades to "every line replaced": still correct
/// output, just less readable.
const MAX_DIFF_CELLS: usize = 1_000_000;

/// Longest-common-subsequence edit script over two line lists.
fn diff_ops(left: &[&[u8]], right: &[&[u8]]) -> Vec<LineOp> {
    let (rows, columns) = (left.len(), right.len());
    let mut ops = Vec::with_capacity(rows + columns);
    if rows.saturating_mul(columns) > MAX_DIFF_CELLS {
        ops.extend((0..rows).map(LineOp::Delete));
        ops.extend((0..columns).map(LineOp::Insert));
        return ops;
    }
    // lcs[i][j] is the LCS length of left[i..] and right[j..]; a suffix table
    // lets the walk below emit the script in reading order.
    let width = columns + 1;
    let mut lcs = vec![0u32; (rows + 1) * width];
    for row in (0..rows).rev() {
        for column in (0..columns).rev() {
            lcs[row * width + column] = if left[row] == right[column] {
                lcs[(row + 1) * width + column + 1] + 1
            } else {
                lcs[(row + 1) * width + column].max(lcs[row * width + column + 1])
            };
        }
    }
    let (mut row, mut column) = (0, 0);
    while row < rows && column < columns {
        if left[row] == right[column] {
            ops.push(LineOp::Equal(row));
            row += 1;
            column += 1;
        } else if lcs[(row + 1) * width + column] >= lcs[row * width + column + 1] {
            ops.push(LineOp::Delete(row));
            row += 1;
        } else {
            ops.push(LineOp::Insert(column));
            column += 1;
        }
    }
    ops.extend((row..rows).map(LineOp::Delete));
    ops.extend((column..columns).map(LineOp::Insert));
    ops
}

/// Group an edit script into unified-diff hunks: each change run plus up to
/// `context` unchanged lines, merging runs whose gap also fits that window.
fn hunk_ranges(ops: &[LineOp], context: usize) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < ops.len() {
        if matches!(ops[index], LineOp::Equal(_)) {
            index += 1;
            continue;
        }
        let start = index.saturating_sub(context);
        let mut end = index;
        loop {
            while end < ops.len() && !matches!(ops[end], LineOp::Equal(_)) {
                end += 1;
            }
            let mut next = end;
            while next < ops.len() && matches!(ops[next], LineOp::Equal(_)) {
                next += 1;
            }
            if next < ops.len() && next - end <= context * 2 {
                end = next;
            } else {
                break;
            }
        }
        let stop = (end + context).min(ops.len());
        ranges.push((start, stop));
        index = stop;
    }
    ranges
}

/// The 1-based line number each op starts at on either side, so a hunk header
/// can name both ranges without walking the script twice.
fn line_numbers(ops: &[LineOp]) -> (Vec<usize>, Vec<usize>) {
    let mut old = Vec::with_capacity(ops.len());
    let mut new = Vec::with_capacity(ops.len());
    let (mut old_line, mut new_line) = (1, 1);
    for op in ops {
        old.push(old_line);
        new.push(new_line);
        match op {
            LineOp::Equal(_) => {
                old_line += 1;
                new_line += 1;
            }
            LineOp::Delete(_) => old_line += 1,
            LineOp::Insert(_) => new_line += 1,
        }
    }
    (old, new)
}

fn diff_hunk_header(
    ops: &[LineOp],
    old_at: &[usize],
    new_at: &[usize],
    start: usize,
    stop: usize,
) -> String {
    let hunk = &ops[start..stop];
    let old_count = hunk
        .iter()
        .filter(|op| !matches!(op, LineOp::Insert(_)))
        .count();
    let new_count = hunk
        .iter()
        .filter(|op| !matches!(op, LineOp::Delete(_)))
        .count();
    // A zero-length range names the line *before* the insertion point, which is
    // how unified diff spells "insert at the top": `@@ -0,0 +1,3 @@`.
    let old_from = if old_count == 0 {
        old_at[start].saturating_sub(1)
    } else {
        old_at[start]
    };
    let new_from = if new_count == 0 {
        new_at[start].saturating_sub(1)
    } else {
        new_at[start]
    };
    format!(
        "@@ -{} +{} @@\n",
        diff_range(old_from, old_count),
        diff_range(new_from, new_count)
    )
}

fn diff_range(from: usize, count: usize) -> String {
    if count == 1 {
        from.to_string()
    } else {
        format!("{from},{count}")
    }
}

/// Emit one diff line, including the marker that makes a missing final newline
/// visible; without it two files that differ only there would print as equal.
fn push_diff_line(out: &mut String, prefix: char, raw: &[u8]) {
    out.push(prefix);
    let body = raw.strip_suffix(b"\n").unwrap_or(raw);
    out.push_str(&String::from_utf8_lossy(body));
    out.push('\n');
    if !raw.ends_with(b"\n") {
        out.push_str("\\ No newline at end of file\n");
    }
}

fn diff_context(value: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("diff -U: {value:?} is not a line count"))
}

/// One step of a document path: `a.b[0]` is `Key("a")`, `Key("b")`, `Index(0)`.
#[derive(Debug, PartialEq, Eq)]
enum JsonPath {
    Key(String),
    Index(usize),
}

/// Parse a dot/bracket path. `$` or an empty path is the document root, a bare
/// numeric segment counts as an index, and `["a.b"]` addresses a key that
/// contains a dot.
fn parse_json_path(path: &str) -> Result<Vec<JsonPath>, String> {
    let chars: Vec<char> = path.trim().chars().collect();
    let mut segments = Vec::new();
    let mut index = 0;
    if chars.first() == Some(&'$') {
        index = 1;
    }
    if chars.get(index) == Some(&'.') {
        index += 1;
    }
    while index < chars.len() {
        match chars[index] {
            '[' => {
                index += 1;
                match chars.get(index) {
                    Some('"') | Some('\'') => {
                        let quote = chars[index];
                        index += 1;
                        let mut key = String::new();
                        loop {
                            match chars.get(index) {
                                None => return Err("json path: unterminated quoted key".into()),
                                Some('\\') => {
                                    let Some(next) = chars.get(index + 1) else {
                                        return Err("json path: trailing escape".into());
                                    };
                                    key.push(*next);
                                    index += 2;
                                }
                                Some(found) if *found == quote => {
                                    index += 1;
                                    break;
                                }
                                Some(found) => {
                                    key.push(*found);
                                    index += 1;
                                }
                            }
                        }
                        if chars.get(index) != Some(&']') {
                            return Err("json path: expected ] after quoted key".into());
                        }
                        index += 1;
                        segments.push(JsonPath::Key(key));
                    }
                    _ => {
                        let start = index;
                        while index < chars.len() && chars[index].is_ascii_digit() {
                            index += 1;
                        }
                        if start == index || chars.get(index) != Some(&']') {
                            return Err(format!(
                                "json path: expected [index] or [\"key\"] in {path:?}"
                            ));
                        }
                        let digits: String = chars[start..index].iter().collect();
                        index += 1;
                        segments.push(JsonPath::Index(
                            digits
                                .parse()
                                .map_err(|_| format!("json path: bad index {digits:?}"))?,
                        ));
                    }
                }
            }
            _ => {
                let mut key = String::new();
                while index < chars.len() && !matches!(chars[index], '.' | '[') {
                    key.push(chars[index]);
                    index += 1;
                }
                if key.is_empty() {
                    return Err(format!("json path: {path:?} has an empty key"));
                }
                segments.push(JsonPath::Key(key));
            }
        }
        match chars.get(index) {
            None => break,
            Some('.') => {
                index += 1;
                if index == chars.len() {
                    return Err(format!("json path: {path:?} ends with a dot"));
                }
            }
            Some('[') => {}
            Some(_) => return Err(format!("json path: {path:?} is not a dot/bracket path")),
        }
    }
    Ok(segments)
}

fn json_lookup<'a>(document: &'a Value, path: &[JsonPath]) -> Option<&'a Value> {
    let mut current = document;
    for segment in path {
        current = match (segment, current) {
            (JsonPath::Key(key), Value::Object(map)) => map.get(key)?,
            (JsonPath::Key(key), Value::Array(items)) => items.get(key.parse::<usize>().ok()?)?,
            (JsonPath::Index(index), Value::Array(items)) => items.get(*index)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Recursive so the borrow of each level outlives the loop that produced it;
/// a mutable walk of `Value` cannot be written as an iterator chain.
fn json_lookup_mut<'a>(document: &'a mut Value, path: &[JsonPath]) -> Option<&'a mut Value> {
    let Some((segment, rest)) = path.split_first() else {
        return Some(document);
    };
    let next = match segment {
        JsonPath::Key(key) => match document {
            Value::Object(map) => map.get_mut(key),
            Value::Array(items) => items.get_mut(key.parse::<usize>().ok()?),
            _ => None,
        },
        JsonPath::Index(index) => match document {
            Value::Array(items) => items.get_mut(*index),
            _ => None,
        },
    }?;
    json_lookup_mut(next, rest)
}

fn json_assign(document: &mut Value, path: &[JsonPath], value: Value) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        *document = value;
        return Ok(());
    };
    let parent = json_lookup_mut(document, parents).ok_or("no such parent path")?;
    match (last, parent) {
        (JsonPath::Key(key), Value::Object(map)) => {
            map.insert(key.clone(), value);
            Ok(())
        }
        (JsonPath::Key(key), Value::Array(items)) => match key.parse::<usize>() {
            Ok(index) => json_assign_index(items, index, value),
            Err(_) => Err(format!("{key:?} is not an array index")),
        },
        (JsonPath::Index(index), Value::Array(items)) => json_assign_index(items, *index, value),
        (JsonPath::Index(_), _) => Err("array index used on a non-array".into()),
        (JsonPath::Key(_), _) => Err("key used on a non-object".into()),
    }
}

/// Index `len` appends, which is what `json set f.json list[2] x` should mean
/// for a two-element list; anything further is a typo, not a sparse array.
fn json_assign_index(items: &mut Vec<Value>, index: usize, value: Value) -> Result<(), String> {
    match items.len().cmp(&index) {
        std::cmp::Ordering::Greater => {
            items[index] = value;
            Ok(())
        }
        std::cmp::Ordering::Equal => {
            items.push(value);
            Ok(())
        }
        std::cmp::Ordering::Less => Err(format!(
            "array index {index} is past the end ({} elements)",
            items.len()
        )),
    }
}

fn json_remove(document: &mut Value, path: &[JsonPath]) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        return Err("cannot remove the document root".into());
    };
    let parent = json_lookup_mut(document, parents).ok_or("no such parent path")?;
    match (last, parent) {
        (JsonPath::Key(key), Value::Object(map)) => {
            map.remove(key).map(|_| ()).ok_or_else(|| format!("no {key:?} key"))
        }
        (JsonPath::Key(key), Value::Array(items)) => match key.parse::<usize>() {
            Ok(index) => json_remove_index(items, index),
            Err(_) => Err(format!("{key:?} is not an array index")),
        },
        (JsonPath::Index(index), Value::Array(items)) => json_remove_index(items, *index),
        (JsonPath::Index(_), _) => Err("array index used on a non-array".into()),
        (JsonPath::Key(_), _) => Err("key used on a non-object".into()),
    }
}

fn json_remove_index(items: &mut Vec<Value>, index: usize) -> Result<(), String> {
    if index >= items.len() {
        return Err(format!(
            "array index {index} is past the end ({} elements)",
            items.len()
        ));
    }
    items.remove(index);
    Ok(())
}

/// Deep merge: object keys merge recursively, everything else — arrays
/// included — is replaced by the source. Arrays are values, not append targets.
fn json_merge_into(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target), Value::Object(source)) => {
            for (key, value) in source {
                match target.get_mut(&key) {
                    Some(existing) => json_merge_into(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, source) => *target = source,
    }
}

/// The indentation a document was already written with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonIndent {
    /// Single line: the file had no line breaks of its own.
    Compact,
    Spaces(usize),
    Tab,
}

/// Read the file's own indentation so an edit produces a minimal diff instead
/// of reflowing every line. No indented line at all means the file was compact.
fn detect_json_indent(text: &str) -> JsonIndent {
    for line in text.lines().skip(1) {
        if line.starts_with('\t') {
            return JsonIndent::Tab;
        }
        if line.starts_with(' ') {
            let width = line.len() - line.trim_start_matches(' ').len();
            return JsonIndent::Spaces(width.clamp(1, 8));
        }
    }
    JsonIndent::Compact
}

/// Render a document the way the file already looks: same indentation, same
/// trailing-newline habit. Everything else is normalized by `serde_json`, which
/// preserves key order and number values but not their original spelling.
fn json_render(document: &Value, original: &str) -> String {
    let mut text = match detect_json_indent(original) {
        JsonIndent::Compact => document.to_string(),
        JsonIndent::Spaces(width) => json_pretty_text(document, width),
        JsonIndent::Tab => json_reindent(&json_pretty_source(document), "\t"),
    };
    if original.ends_with('\n') && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn json_pretty_text(document: &Value, width: usize) -> String {
    if width == 0 {
        return document.to_string();
    }
    let pretty = json_pretty_source(document);
    if width == 2 {
        pretty
    } else {
        json_reindent(&pretty, &" ".repeat(width))
    }
}

/// `Value` has no non-string map keys, so pretty-printing cannot fail; the
/// fallback keeps this total without an `unwrap` in the shell.
fn json_pretty_source(document: &Value) -> String {
    serde_json::to_string_pretty(document).unwrap_or_else(|_| document.to_string())
}

/// Re-indent pretty output by rewriting each line's leading two-space step as
/// `unit`. JSON escapes newlines inside strings, so every newline here is a
/// formatting newline and no line counts as anything else.
fn json_reindent(text: &str, unit: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_start_matches(' ');
        let depth = (line.len() - body.len()) / 2;
        for _ in 0..depth {
            out.push_str(unit);
        }
        out.push_str(body);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    text: String,
    glob: bool,
    expand: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(Word),
    Op(Op),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Pipe,
    And,
    Or,
    Semi,
    In,
    Out,
    Append,
    Err,
    ErrAppend,
    ErrToOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopSignal {
    Break,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainOp {
    Always,
    And,
    Or,
}

fn tokenize(line: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut glob = false;
    let mut expand = true;
    let mut quote = None;
    let mut escape = false;
    // A quoted word counts even when it is empty: `sub 'a' '' f` has three
    // arguments, and `printf '%s' ''` prints an empty string.
    let mut quoted = false;
    let mut sub_depth = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
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
        if sub_depth > 0 {
            match ch {
                '(' => sub_depth += 1,
                ')' => sub_depth -= 1,
                _ => {}
            }
            word.push(ch);
            continue;
        }
        match ch {
            '#' if word.is_empty() => break,
            '\'' => {
                expand = false;
                quote = Some(ch);
                quoted = true;
            }
            '"' => {
                quote = Some(ch);
                quoted = true;
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                word.push_str("$(");
                sub_depth += 1;
            }
            '|' => {
                push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted);
                if chars.peek() == Some(&'|') {
                    chars.next();
                    tokens.push(Token::Op(Op::Or));
                } else {
                    tokens.push(Token::Op(Op::Pipe));
                }
            }
            '&' => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted);
                    tokens.push(Token::Op(Op::And));
                } else {
                    return Err("background jobs are not supported".into());
                }
            }
            ';' => {
                push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted);
                tokens.push(Token::Op(Op::Semi));
            }
            '<' => {
                push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted);
                tokens.push(Token::Op(Op::In));
            }
            '2' if word.is_empty() && chars.peek() == Some(&'>') => {
                chars.next();
                match chars.peek() {
                    Some('>') => {
                        chars.next();
                        tokens.push(Token::Op(Op::ErrAppend));
                    }
                    Some('&') => {
                        chars.next();
                        if chars.next() == Some('1') {
                            tokens.push(Token::Op(Op::ErrToOut));
                        } else {
                            return Err("only 2>&1 is supported for stderr merge".into());
                        }
                    }
                    _ => tokens.push(Token::Op(Op::Err)),
                }
            }
            '>' => {
                push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted);
                if chars.peek() == Some(&'>') {
                    chars.next();
                    tokens.push(Token::Op(Op::Append));
                } else {
                    tokens.push(Token::Op(Op::Out));
                }
            }
            '*' | '?' | '[' => {
                glob = true;
                word.push(ch);
            }
            ch if ch.is_whitespace() => push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted),
            _ => word.push(ch),
        }
    }
    if quote.is_some() {
        return Err("unterminated quote".into());
    }
    if sub_depth > 0 {
        return Err("unterminated command substitution".into());
    }
    if escape {
        word.push('\\');
    }
    push_word(&mut tokens, &mut word, &mut glob, &mut expand, &mut quoted);
    Ok(tokens)
}

fn push_word(
    tokens: &mut Vec<Token>,
    word: &mut String,
    glob: &mut bool,
    expand: &mut bool,
    quoted: &mut bool,
) {
    if !word.is_empty() || *quoted {
        let text = std::mem::take(word);
        let has_glob = std::mem::take(glob);
        let has_expand = *expand;
        *expand = true;
        tokens.push(Token::Word(Word {
            text,
            glob: has_glob,
            expand: has_expand,
        }));
    } else {
        *glob = false;
        *expand = true;
    }
    *quoted = false;
}

fn parse_chains(tokens: &[Token]) -> Result<Vec<(ChainOp, Vec<Token>)>, String> {
    let mut chains = Vec::new();
    let mut current = Vec::new();
    let mut operator = ChainOp::Always;
    let mut depth = 0usize;
    let mut at_start = true;
    for token in tokens {
        match token {
            Token::Word(word) => {
                if at_start && matches!(word.text.as_str(), "if" | "for" | "while") {
                    depth += 1;
                } else if at_start && matches!(word.text.as_str(), "fi" | "done") {
                    depth = depth.saturating_sub(1);
                }
                at_start = matches!(word.text.as_str(), "then" | "do" | "else" | "elif");
                current.push(token.clone());
            }
            Token::Op(Op::Semi | Op::And | Op::Or) if depth == 0 => {
                if current.is_empty() {
                    return Err("empty command".into());
                }
                let next = match token {
                    Token::Op(Op::Semi) => ChainOp::Always,
                    Token::Op(Op::And) => ChainOp::And,
                    Token::Op(Op::Or) => ChainOp::Or,
                    _ => unreachable!(),
                };
                chains.push((operator, std::mem::take(&mut current)));
                operator = next;
                at_start = true;
            }
            Token::Op(Op::Semi) => {
                at_start = true;
                current.push(token.clone());
            }
            Token::Op(Op::And | Op::Or) if depth > 0 => {
                at_start = true;
                current.push(token.clone());
            }
            Token::Op(_) => {
                at_start = false;
                current.push(token.clone());
            }
        }
    }
    if !current.is_empty() {
        chains.push((operator, current));
    } else if matches!(operator, ChainOp::And | ChainOp::Or) {
        return Err("trailing operator needs a command".into());
    }
    Ok(chains)
}

fn split_tokens(tokens: &[Token], operator: Op) -> Vec<Vec<Token>> {
    let mut parts = vec![Vec::new()];
    for token in tokens {
        if *token == Token::Op(operator) {
            parts.push(Vec::new());
        } else {
            parts.last_mut().expect("pipeline part").push(token.clone());
        }
    }
    parts
}

struct Segment {
    words: Vec<Word>,
    stdin: Option<Word>,
    stdout: Option<(bool, Word)>,
    stderr: Option<(bool, Word)>,
    stderr_to_stdout: bool,
}

fn parse_segment(tokens: &[Token]) -> Result<Segment, String> {
    let mut words = Vec::new();
    let mut stdin = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut stderr_to_stdout = false;
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        match token {
            Token::Word(word) => words.push(word.clone()),
            Token::Op(Op::In) => {
                let Some(Token::Word(target)) = iter.next() else {
                    return Err("input redirection needs a path".into());
                };
                if stdin.replace(target.clone()).is_some() {
                    return Err("only one input redirection is supported".into());
                }
            }
            Token::Op(Op::Out | Op::Append) => {
                let append = matches!(token, Token::Op(Op::Append));
                let Some(Token::Word(target)) = iter.next() else {
                    return Err("redirection needs a path".into());
                };
                if stdout.replace((append, target.clone())).is_some() {
                    return Err("only one output redirection is supported".into());
                }
            }
            Token::Op(Op::Err | Op::ErrAppend) => {
                let append = matches!(token, Token::Op(Op::ErrAppend));
                let Some(Token::Word(target)) = iter.next() else {
                    return Err("stderr redirection needs a path".into());
                };
                if stderr.replace((append, target.clone())).is_some() {
                    return Err("only one stderr redirection is supported".into());
                }
            }
            Token::Op(Op::ErrToOut) => {
                if stderr_to_stdout || stderr.is_some() {
                    return Err("stderr is redirected twice".into());
                }
                stderr_to_stdout = true;
            }
            Token::Op(Op::Pipe | Op::And | Op::Or | Op::Semi) => {
                return Err("unexpected operator".into());
            }
        }
    }
    if words.is_empty() {
        return Err("empty command".into());
    }
    Ok(Segment {
        words,
        stdin,
        stdout,
        stderr,
        stderr_to_stdout,
    })
}

fn token_word(token: &Token) -> Option<&str> {
    match token {
        Token::Word(word) => Some(&word.text),
        Token::Op(_) => None,
    }
}

fn is_compound(tokens: &[Token]) -> bool {
    matches!(
        tokens.first().and_then(token_word),
        Some("if" | "for" | "while")
    )
}

fn find_block_marker(tokens: &[Token], from: usize, targets: &[&str]) -> Option<usize> {
    let mut depth = 1usize;
    let mut at_start = true;
    for (index, token) in tokens.iter().enumerate().skip(from) {
        match token {
            Token::Word(word) => {
                if at_start && matches!(word.text.as_str(), "if" | "for" | "while") {
                    depth += 1;
                    at_start = false;
                    continue;
                }
                if at_start && matches!(word.text.as_str(), "fi" | "done") {
                    if depth == 1 && targets.contains(&word.text.as_str()) {
                        return Some(index);
                    }
                    depth = depth.saturating_sub(1);
                    at_start = false;
                    continue;
                }
                if depth == 1 && targets.contains(&word.text.as_str()) {
                    return Some(index);
                }
                at_start = matches!(word.text.as_str(), "then" | "do" | "else" | "elif");
            }
            Token::Op(Op::Semi) => at_start = true,
            Token::Op(_) => at_start = false,
        }
    }
    None
}

fn block_depth(tokens: &[Token]) -> usize {
    let mut depth = 0usize;
    let mut at_start = true;
    for token in tokens {
        match token {
            Token::Word(word) => {
                if at_start && matches!(word.text.as_str(), "if" | "for" | "while") {
                    depth += 1;
                } else if at_start && matches!(word.text.as_str(), "fi" | "done") {
                    depth = depth.saturating_sub(1);
                }
                at_start = matches!(word.text.as_str(), "then" | "do" | "else" | "elif");
            }
            Token::Op(Op::Semi) => at_start = true,
            Token::Op(_) => at_start = false,
        }
    }
    depth
}

fn under_root(root: &str, path: &str) -> bool {
    root.is_empty() || path == root || path.starts_with(&format!("{root}/"))
}

fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while v < value.len() {
        let mut matched = false;
        let mut next = p;
        if p < pattern.len() {
            match pattern[p] {
                b'*' => {
                    star = Some(p);
                    p += 1;
                    continue;
                }
                b'?' => {
                    matched = true;
                    next = p + 1;
                }
                b'[' => {
                    if let Some(end) = pattern[p + 1..].iter().position(|byte| *byte == b']') {
                        let end = p + 1 + end;
                        matched = glob_class(&pattern[p + 1..end], value[v]);
                        next = end + 1;
                    } else {
                        matched = pattern[p] == value[v];
                        next = p + 1;
                    }
                }
                byte => {
                    matched = byte == value[v];
                    next = p + 1;
                }
            }
        }
        if matched {
            p = next;
            v += 1;
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

fn glob_class(class: &[u8], value: u8) -> bool {
    let (negate, mut class) = match class.first() {
        Some(b'!') | Some(b'^') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut matched = false;
    while let Some((&first, rest)) = class.split_first() {
        if rest.len() >= 2 && rest[0] == b'-' {
            let end = rest[1];
            if first <= value && value <= end {
                matched = true;
            }
            class = &rest[2..];
        } else {
            if first == value {
                matched = true;
            }
            class = rest;
        }
    }
    if negate { !matched } else { matched }
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
  grep [-i] PAT TEXT...  linear search file contents
  tgrep PATTERN [PATH...] indexed regex search over the VFS
  wc [-l|-c] PATH...      line/byte counts
  stat PATH               file metadata
  tree [DIR]              recursive listing
  find [DIR] [-name GLOB] [-type f|d] [-maxdepth N]  paths matching a glob
  sub PATTERN REPLACEMENT PATH...  regex replace in place (all matches, $1 groups)
  diff [-q] [-U N] LEFT RIGHT      unified line diff; 0 same, 1 different, 2 unreadable
  json get [-j] FILE [PATH]        print a JSON value; strings print raw unless -j
  json set FILE PATH JSON|-        replace a value (JSON literal, or - for stdin)
  json del FILE PATH               remove an object key or array element
  json merge TARGET SOURCE...      deep-merge objects into TARGET
  json pretty [--indent N] [FILE]  reformat to stdout (stdin when FILE is - or absent)
  rm [-r] PATH...         unlink files or recursively remove a directory
  mv FROM TO              rename a file
  cp FROM TO              copy bytes to a new version
  search QUERY            fuzzy index search
  commands                list installed wasm commands
  which COMMAND           show a command's package identity
  source SCRIPT [ARG...]  run a Mush script from the VFS

Shell syntax:
  cmd1 | cmd2             pipe stdout to stdin
  cmd1 ; cmd2             run both
  cmd1 && cmd2            run cmd2 if cmd1 succeeds
  cmd1 || cmd2            run cmd2 if cmd1 fails
  cmd < PATH              read stdin from a VFS file
  cmd > PATH              write stdout to a VFS file
  cmd >> PATH             append stdout to a VFS file
  cmd 2> PATH             write stderr to a VFS file
  cmd 2>> PATH            append stderr to a VFS file
  cmd 2>&1                merge stderr into stdout
  *.txt, docs/*.md        glob VFS paths; quoted globs stay literal
  $(cmd)                  command substitution
  # comment               rest of line
  $?                      previous command status
  $0, $1, $#, $@          script name, arguments, count, all arguments

Any other command resolves through the virtual PATH to an installed wasm
package. Commands get explicit args, piped stdin, the VFS cwd, and no host
environment; they never run host binaries.
  echo TEXT               print text
  printf FORMAT [ARG...]  formatted output
  test EXPR               evaluate a condition (or [ EXPR ])
  read [-r] [VAR]         read one line from piped stdin
  if ...; then ...; fi    conditional block
  for VAR in ...; do ...; done
  while ...; do ...; done
  break / continue        leave or continue the nearest loop
  export KEY=VALUE        set an environment variable
  unset KEY...            remove environment variables
  env                     print the environment
  set [KEY=VALUE]         print or set variables
  exit                    leave
";

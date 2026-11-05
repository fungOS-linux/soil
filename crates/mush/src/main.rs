use std::io::{self, BufRead, Write};

use anyhow::Result;
use mush::{Mush, Outcome};

fn main() {
    if let Err(error) = run() {
        eprintln!("mush: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let root = args
        .iter()
        .position(|arg| arg == "--root")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
        .unwrap_or("~/.local/share/fungos/vfs");
    let root = expand_home(root);
    let mut mush = Mush::open(&root)?;
    let script = args
        .iter()
        .position(|arg| arg == "--script")
        .and_then(|index| args.get(index + 1));
    if let Some(script) = script {
        let script = script.strip_prefix('/').unwrap_or(script);
        let command = format!("source {script}");
        if mush.execute_line(&command)? == Outcome::Exit {
            return Ok(());
        }
        return Ok(());
    }
    let stdin = io::stdin();
    loop {
        print!("{}", mush.prompt());
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }
        if mush.execute_line(&line)? == Outcome::Exit {
            break;
        }
    }
    Ok(())
}

fn expand_home(value: &str) -> std::path::PathBuf {
    if value == "~" {
        return std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest);
    }
    std::path::PathBuf::from(value)
}

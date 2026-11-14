use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use soil_command::{CommandRegistry, CommandRunner, RecordsBinding, RunRequest};

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-command: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command_root = std::env::var_os("FUNGOS_COMMAND_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/share/fungos/commands")
        });
    // Explicit `--root` flags replace the default pair; otherwise this sees
    // exactly what a shell sees: hand-installed commands, then installed Spores.
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut records_root = None;
    let mut book = None;
    let mut collection = None;
    let mut list = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                roots.push(PathBuf::from(args.get(i + 1).context("--root DIR")?));
                i += 2;
            }
            "--records-root" => {
                records_root = Some(PathBuf::from(
                    args.get(i + 1).context("--records-root DIR")?,
                ));
                i += 2;
            }
            "--book" => {
                book = Some(args.get(i + 1).context("--book ID")?.clone());
                i += 2;
            }
            "--collection" => {
                collection = Some(args.get(i + 1).context("--collection NAME")?.clone());
                i += 2;
            }
            "--list" => {
                list = true;
                i += 1;
            }
            "run" => {
                i += 1;
                break;
            }
            other => bail!("unknown option {other}"),
        }
    }
    let roots = if roots.is_empty() {
        soil_command::roots(command_root)
    } else {
        roots
    };
    let registry = CommandRegistry::scan_roots(&roots)?;
    // A package that could not be read is worth saying out loud, but it is not a
    // reason to refuse the commands that did load.
    for refusal in registry.refused() {
        eprintln!("soil-command: {}: {}", refusal.name, refusal.reason);
    }
    if list {
        for entry in registry.entries() {
            println!("{}\t{}\t{}", entry.name, entry.version, entry.summary);
        }
        return Ok(());
    }
    let Some(name) = args.get(i).cloned() else {
        bail!("usage: soil-command [--root DIR]... [--list] run NAME [ARGS...]");
    };
    let entry = registry.get(&name).context("unknown command")?.clone();
    let records = match (records_root, book, collection) {
        (Some(root), Some(book), Some(collection)) => Some(RecordsBinding {
            root,
            book,
            collection,
        }),
        (None, None, None) => None,
        _ => bail!("records binding needs --records-root, --book and --collection"),
    };
    let mut stdin = Vec::new();
    // A command run from a prompt has no input. Reading a terminal to end-of-file
    // would wedge the shell that started it, so only a pipe is read.
    if !std::io::stdin().is_terminal() {
        std::io::stdin().read_to_end(&mut stdin)?;
    }
    let output = CommandRunner::new(records)?.run(
        &entry,
        &RunRequest {
            args: args[i + 1..].to_vec(),
            stdin,
            // Commands do not inherit the host environment.
            env: Vec::new(),
            cwd: None,
        },
    )?;
    std::io::stdout().write_all(&output.stdout)?;
    std::io::stderr().write_all(&output.stderr)?;
    if output.code != 0 {
        std::process::exit(output.code);
    }
    Ok(())
}

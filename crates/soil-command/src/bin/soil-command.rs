use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use soil_audio_host::StaticRooms;
use soil_command::{Capabilities, CommandRegistry, CommandRunner, RecordsBinding, RunRequest};

fn main() {
    if let Err(error) = run() {
        eprintln!("soil-command: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = std::env::var_os("FUNGOS_COMMAND_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/share/fungos/commands")
        });
    let mut records_root = None;
    let mut book = None;
    let mut collection = None;
    let mut vfs_root = None;
    let mut appdata_root = None;
    let mut app_id = None;
    let mut user_id = None;
    let mut audio = false;
    let mut list = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                root = PathBuf::from(args.get(i + 1).context("--root DIR")?);
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
            "--vfs-root" => {
                vfs_root = Some(PathBuf::from(args.get(i + 1).context("--vfs-root DIR")?));
                i += 2;
            }
            "--appdata-root" => {
                appdata_root = Some(PathBuf::from(
                    args.get(i + 1).context("--appdata-root DIR")?,
                ));
                i += 2;
            }
            "--app-id" => {
                app_id = Some(args.get(i + 1).context("--app-id ID")?.clone());
                i += 2;
            }
            "--user-id" => {
                user_id = Some(args.get(i + 1).context("--user-id ID")?.clone());
                i += 2;
            }
            "--audio" => {
                audio = true;
                i += 1;
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
    let registry = CommandRegistry::scan(&root)?;
    if list {
        for entry in registry.entries() {
            println!("{}\t{}\t{}", entry.name, entry.version, entry.summary);
        }
        return Ok(());
    }
    let Some(name) = args.get(i).cloned() else {
        bail!("usage: soil-command [--list] run NAME [ARGS...]");
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
    std::io::stdin().read_to_end(&mut stdin)?;
    let capabilities = Capabilities {
        vfs_root,
        appdata_root,
        app_id,
        user_id,
        rooms: audio.then(|| Arc::new(StaticRooms::from_env()) as Arc<dyn soil_audio_host::RoomDirectory>),
    };
    let output = CommandRunner::with_capabilities(records, capabilities)?.run(
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

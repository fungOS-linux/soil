//! Operator path for the content-addressed cache behind `soil:storage`.
//!
//! This cache holds content Soil *ships*: the storefront already publishes a
//! CID for every package (`soil_store::cid::raw` over the packed bytes), and a
//! Spore reads it back by that address. The binary is how an operator fills the
//! local mirror and proves what came out; it mints no authority and is not a
//! place for a user's own files — those belong in a `soil:vfs` workspace.
//!
//!   soil-storage import FILE CID   fill the cache, refusing bytes that do not
//!                                  match the published address
//!   soil-storage get CID OUT       write the bytes out, verifying the address
//!   soil-storage has CID           exit 0 when present
//!   soil-storage size              bytes and object count in the cache

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use soil_cid as cid;
use soil_storage_host::Storage;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-storage: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().context("usage: soil-storage put|get|has|size ...")?;
    let root = root();
    match command.as_str() {
        "import" => {
            let path = PathBuf::from(args.next().context("import needs FILE")?);
            let address = args.next().context("import needs CID")?;
            let storage = Storage::new(&root)?;
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            // The address is the published one, never computed from whatever
            // bytes happen to be at the path: a mismatch is refused, so this
            // cannot mint an address for content Soil never shipped.
            cid::verify(&address, &bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
            storage.write(&address, &bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
            println!("ok {address}");
        }
        "get" => {
            let address = args.next().context("get needs CID")?;
            let out = PathBuf::from(args.next().context("get needs OUT")?);
            let storage = Storage::new(&root)?;
            let bytes = storage
                .read(&address, 256 * 1024 * 1024)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            std::fs::write(&out, &bytes).with_context(|| format!("write {}", out.display()))?;
            println!("ok {} bytes", bytes.len());
        }
        "has" => {
            let address = args.next().context("has needs CID")?;
            let storage = Storage::new(&root)?;
            if storage.contains(&address) {
                println!("present");
            } else {
                bail!("not present");
            }
        }
        "size" => {
            let storage = Storage::new(&root)?;
            println!("{} bytes at {}", storage.used_bytes()?, root.display());
        }
        other => bail!("unknown command {other:?}; expected import, get, has, or size"),
    }
    Ok(())
}

fn root() -> PathBuf {
    std::env::var_os("SOIL_STORAGE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".cache/soil/storage"))
                .unwrap_or_else(|| PathBuf::from(".soil/storage"))
        })
}

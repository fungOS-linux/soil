//! Operator path for the content-addressed cache behind `soil:storage`.
//!
//! A Spore names a CID; something has to put the bytes there and be able to
//! prove what came back out. That is this binary.
//!
//!   soil-storage put FILE          print the CID, store the bytes
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
        "put" => {
            let path = PathBuf::from(args.next().context("put needs FILE")?);
            let storage = Storage::new(&root)?;
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let address = cid::raw(&bytes);
            storage.write(&address, &bytes).map_err(|error| anyhow::anyhow!("{error}"))?;
            println!("{address}");
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
        other => bail!("unknown command {other:?}; expected put, get, has, or size"),
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

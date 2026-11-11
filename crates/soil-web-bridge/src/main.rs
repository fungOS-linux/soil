//! Serve one Spore's Soil capabilities to a browser.
//!
//! ```sh
//! soil-web-bridge --spore ../spores/spores/worklog \
//!   --records-root ~/.local/share/fungos/records \
//!   --book worklog --collection worklog.event
//! ```
//!
//! The spore's `[grants]` table is the whole access policy. A manifest with
//! `soil:records.read` gets a read-only bridge; one with `soil:records` gets both
//! halves. Nothing on the command line can widen that.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use anyhow::{Context, bail};
use clap::Parser;
use soil_web_bridge::{Auth, Bridge, GrantSet, RecordsCapability, Registry};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Bridge a browser to one Spore's Soil capabilities over HTTP and a WebSocket"
)]
struct Args {
    /// Directory containing the Spore's spore.toml
    #[arg(long)]
    spore: PathBuf,

    /// Root of the fungos-records store the bridge reads
    #[arg(long)]
    records_root: PathBuf,

    /// fungos-records book served as this bridge's collection
    #[arg(long, default_value = "worklog")]
    book: String,

    /// Collection name a client must name in `key.collection`
    #[arg(long, default_value = "worklog.event")]
    collection: String,

    #[arg(long, default_value = "127.0.0.1:8795")]
    bind: String,

    /// Bearer token. Without one, only loopback clients are accepted.
    #[arg(long)]
    token: Option<String>,

    /// Origin allowed to script a session. Repeat for several.
    #[arg(long = "origin")]
    origins: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let bind: SocketAddr = args
        .bind
        .parse()
        .with_context(|| format!("{} is not a socket address", args.bind))?;
    if !bind.ip().is_loopback() && args.token.is_none() {
        bail!(
            "--bind {bind} is not loopback; a non-loopback bridge needs --token \
             (or put it behind the gateway and keep this listener on loopback)"
        );
    }

    let manifest = soil_spore::Manifest::load(&args.spore)
        .with_context(|| format!("read {}", args.spore.join("spore.toml").display()))?;
    let spore = manifest.spore.id.clone();
    let grants = GrantSet::from_manifest(&manifest);

    let records = RecordsCapability::open(&args.records_root, &args.book, &args.collection)
        .with_context(|| {
            format!(
                "open records book {:?} at {}",
                args.book,
                args.records_root.display()
            )
        })?;
    let registry = Registry::new().with(records);

    let mut auth = match &args.token {
        Some(token) => Auth::with_token(token),
        None => Auth::loopback_only(),
    };
    for origin in &args.origins {
        auth = auth.allow_origin(origin.clone());
    }

    let bridge = Arc::new(Bridge::new(spore.clone(), grants, registry).with_auth(auth));
    let server = bridge.serve(bind)?;
    let addr = server.addr();

    println!("soil-web-bridge: {spore}");
    println!("  collection {} in book {}", args.collection, args.book);
    println!("  demo    http://{addr}/");
    println!("  call    POST http://{addr}/v1/call");
    println!("  session ws://{addr}/v1/session");
    match &args.token {
        Some(_) => println!("  auth    bearer token"),
        None => println!("  auth    loopback only"),
    }
    if !args.origins.is_empty() {
        println!("  origins {}", args.origins.join(", "));
    }

    // `Server` stops the accept loop when dropped, so park instead of returning.
    thread::park();
    Ok(())
}

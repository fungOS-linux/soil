//! Serve a component exporting `wasi:http/incoming-handler`.
//!
//! ```sh
//! soil-wasi-http \
//!   --listen 127.0.0.1:8080 \
//!   --allow-origin https://api.example.com \
//!   component.wasm
//! ```

use std::{env, fs, path::PathBuf, process::ExitCode, sync::Arc};

use anyhow::{Context, Result, bail};
use soil_host::records_host::RecordsBookHost;
use soil_wasi_host::{
    HttpPolicy, HttpService, NetworkPolicy, UnibusBus, WasiHostBuilder, WasiHostPolicy,
};
use tokio::net::TcpListener;
use wasmtime_wasi::FsPerms;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("soil-wasi-http: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let options = Options::parse(env::args().skip(1))?;
    let bytes = fs::read(&options.component)
        .with_context(|| format!("read {}", options.component.display()))?;
    let policy = WasiHostPolicy {
        http: options.http,
        network: options.network,
    };
    let mut builder = WasiHostBuilder::new(policy)
        .args(options.args)
        .inherit_env(options.inherit_env)
        .inherit_stdio(options.inherit_stdio);
    for (key, value) in options.env {
        builder = builder.env(key, value);
    }
    for (host, guest, perms) in options.preopens {
        builder = builder.preopen_dir(host, guest, perms);
    }
    if let Some((root, book, collection)) = options.records {
        let records = RecordsBookHost::open(&root, book, collection)
            .with_context(|| format!("open records book in {}", root.display()))?;
        builder = builder.records_book(records);
    }
    if let Some(path) = &options.unibus {
        let bus = UnibusBus::from_config_file(path)
            .with_context(|| format!("connect the Unibus hub for {}", path.display()))?;
        builder = builder.bus(Arc::new(bus));
    }
    let service = Arc::new(HttpService::from_bytes(&bytes, builder)?);
    let listener = TcpListener::bind(&options.listen)
        .await
        .with_context(|| format!("bind {}", options.listen))?;
    let address = listener.local_addr()?;
    println!("soil-wasi-http: listening on http://{address}");
    service.serve(listener).await
}

#[derive(Default)]
struct Options {
    listen: String,
    component: PathBuf,
    http: HttpPolicy,
    network: NetworkPolicy,
    args: Vec<String>,
    env: Vec<(String, String)>,
    inherit_env: bool,
    inherit_stdio: bool,
    preopens: Vec<(PathBuf, String, FsPerms)>,
    records: Option<(PathBuf, String, String)>,
    unibus: Option<PathBuf>,
}

impl Options {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self> {
        let mut options = Self {
            listen: "127.0.0.1:8080".to_string(),
            ..Self::default()
        };
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => {
                    options.listen = next(&mut args, "--listen")?;
                }
                "--allow-origin" => {
                    options
                        .http
                        .allow_origin(&next(&mut args, "--allow-origin")?)?;
                }
                "--max-field-size" => {
                    let value = next(&mut args, "--max-field-size")?;
                    options.http.set_max_field_size(
                        value
                            .parse()
                            .with_context(|| format!("parse --max-field-size {value}"))?,
                    );
                }
                "--allow-tcp-connect" => {
                    options
                        .network
                        .allow_tcp_connect(&next(&mut args, "--allow-tcp-connect")?)?;
                }
                "--allow-tcp-bind" => {
                    options
                        .network
                        .allow_tcp_bind(&next(&mut args, "--allow-tcp-bind")?)?;
                }
                "--allow-tcp-accept" => {
                    options
                        .network
                        .allow_tcp_accept(&next(&mut args, "--allow-tcp-accept")?)?;
                }
                "--allow-udp-send" => {
                    options
                        .network
                        .allow_udp_send(&next(&mut args, "--allow-udp-send")?)?;
                }
                "--allow-udp-receive" => {
                    options
                        .network
                        .allow_udp_receive(&next(&mut args, "--allow-udp-receive")?)?;
                }
                "--allow-udp-bind" => {
                    options
                        .network
                        .allow_udp_bind(&next(&mut args, "--allow-udp-bind")?)?;
                }
                "--allow-dns" => options.network.allow_dns(true),
                "--arg" => options.args.push(next(&mut args, "--arg")?),
                "--env" => {
                    let value = next(&mut args, "--env")?;
                    let (key, value) = value
                        .split_once('=')
                        .ok_or_else(|| anyhow::anyhow!("--env requires KEY=VALUE"))?;
                    options.env.push((key.to_string(), value.to_string()));
                }
                "--inherit-env" => options.inherit_env = true,
                "--inherit-stdio" => options.inherit_stdio = true,
                "--dir" => {
                    let value = next(&mut args, "--dir")?;
                    options.preopens.push(parse_dir(&value)?);
                }
                "--records" => {
                    let value = next(&mut args, "--records")?;
                    options.records = Some(parse_records(&value)?);
                }
                "--unibus" => {
                    let value = next(&mut args, "--unibus")?;
                    options.unibus = Some(PathBuf::from(value));
                }
                "--help" | "-h" => {
                    println!("{USAGE}");
                    std::process::exit(0);
                }
                value if value.starts_with('-') => bail!("unknown option {value}\n\n{USAGE}"),
                value => {
                    if !options.component.as_os_str().is_empty() {
                        bail!("unexpected extra argument {value}");
                    }
                    options.component = PathBuf::from(value);
                }
            }
        }
        if options.component.as_os_str().is_empty() {
            bail!("missing COMPONENT.wasm\n\n{USAGE}");
        }
        Ok(options)
    }
}

fn next(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn parse_records(value: &str) -> Result<(PathBuf, String, String)> {
    let mut parts = value.splitn(3, ':');
    let root = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--records requires DIR:BOOK:COLLECTION"))?;
    let book = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--records requires DIR:BOOK:COLLECTION"))?;
    let collection = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--records requires DIR:BOOK:COLLECTION"))?;
    Ok((
        PathBuf::from(root),
        book.to_string(),
        collection.to_string(),
    ))
}

fn parse_dir(value: &str) -> Result<(PathBuf, String, FsPerms)> {
    let mut parts = value.splitn(3, ':');
    let host = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--dir requires HOST:GUEST[:ro|rw]"))?;
    let guest = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| anyhow::anyhow!("--dir requires HOST:GUEST[:ro|rw]"))?;
    let perms = match parts.next() {
        None | Some("ro") => FsPerms::ReadOnly,
        Some("rw") => FsPerms::ReadWrite,
        Some(other) => bail!("--dir mode must be ro or rw, got {other}"),
    };
    Ok((PathBuf::from(host), guest.to_string(), perms))
}

const USAGE: &str = "\
usage: soil-wasi-http [OPTIONS] COMPONENT.wasm

  --listen ADDR               bind address (default 127.0.0.1:8080)
  --allow-origin ORIGIN       allow outgoing wasi:http to an origin (repeatable)
  --max-field-size BYTES      wasi:http field-map limit
  --allow-tcp-connect CIDR    allow outbound TCP to a CIDR (repeatable)
  --allow-tcp-bind CIDR       allow TCP binds/listens to a CIDR (repeatable)
  --allow-tcp-accept CIDR     allow inbound TCP accepts from a CIDR (repeatable)
  --allow-udp-send CIDR       allow UDP sends to a CIDR (repeatable)
  --allow-udp-receive CIDR    allow UDP receives from a CIDR (repeatable)
  --allow-udp-bind CIDR       allow UDP binds to a CIDR (repeatable)
  --allow-dns                 enable wasi:sockets/ip-name-lookup
  --arg VALUE                 append a guest argument (repeatable)
  --env KEY=VALUE             set a guest environment variable (repeatable)
  --inherit-env               inherit the host environment
  --inherit-stdio             inherit stdin/stdout/stderr
  --dir HOST:GUEST[:ro|rw]    preopen a directory (default ro, repeatable)
  --records DIR:BOOK:COLLECTION
                              link soil:records to one book and collection, so a
                              Spore that imports it can store run state. Without
                              this flag the interface is unlinked and such a
                              component fails to instantiate.
  --unibus CONFIG.json       connect the Unibus hub named by an unibus-sdk
                              `ClientConfig` and link soil:unibus, so a Spore
                              that imports it can poll and publish frames. The
                              credential stays in the host.";

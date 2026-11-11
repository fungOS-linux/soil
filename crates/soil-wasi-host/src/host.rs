//! Reusable `wasi:cli` + `wasi:http` store state.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::Result;
use http_body_util::BodyExt;
use soil_host::http_host::HttpTransport;
use soil_host::records_host::{RecordsBookHost, records};
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    DEFAULT_FORBIDDEN_HEADERS, Error, ForbiddenHeaderFilter, RequestOptions, WasiBody, WasiHttpCtx,
    WasiHttpCtxView, WasiHttpHooks, WasiHttpView,
};

use crate::{
    context::HttpContext,
    creds::CredentialProvider,
    policy::{HttpPolicy, WasiHostPolicy},
    unibus::Bus,
};

/// A `soil:http` installation for a Spore that serves `wasi:http`.
///
/// The policy is the same one the `runner`-world host uses — the allowlist, the
/// header rules and the budgets live in one place — and only the transport
/// differs. That is what lets a Spore reach the network through a browser
/// instead of this process's socket.
pub struct HttpCapability {
    pub(crate) policy: soil_host::http_host::HttpPolicy,
    pub(crate) transport: Arc<dyn HttpTransport>,
}

impl HttpCapability {
    pub fn new(policy: soil_host::http_host::HttpPolicy, transport: Arc<dyn HttpTransport>) -> Self {
        Self { policy, transport }
    }
}

/// One preopened host directory.
#[derive(Clone, Debug)]
pub struct Preopen {
    host: PathBuf,
    guest: String,
    perms: FsPerms,
}

/// Builder for a per-request [`WasiHost`].
#[derive(Clone, Default)]
pub struct WasiHostBuilder {
    policy: WasiHostPolicy,
    args: Vec<String>,
    env: Vec<(String, String)>,
    inherit_env: bool,
    inherit_stdio: bool,
    preopens: Vec<Preopen>,
    credentials: Option<Arc<dyn CredentialProvider>>,
    bus: Option<Arc<dyn Bus>>,
    /// Shared across requests on purpose: one SQLite store per process, not one
    /// per invocation.
    records: Option<Arc<Mutex<RecordsBookHost>>>,
    /// `soil:http` policy and route. Absent means the interface is not linked.
    http: Option<Arc<HttpCapability>>,
}

impl WasiHostBuilder {
    pub fn new(policy: WasiHostPolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args.extend(args);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn inherit_env(mut self, inherit: bool) -> Self {
        self.inherit_env = inherit;
        self
    }

    pub fn inherit_stdio(mut self, inherit: bool) -> Self {
        self.inherit_stdio = inherit;
        self
    }

    pub fn preopen_dir(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        perms: FsPerms,
    ) -> Self {
        self.preopens.push(Preopen {
            host: host.into(),
            guest: guest.into(),
            perms,
        });
        self
    }

    pub fn policy(&self) -> &WasiHostPolicy {
        &self.policy
    }

    /// Attach the host-side credential backend exposed as `soil:creds`.
    ///
    /// Without a provider, every `resolve`, `sign`, and `verify` call fails
    /// with `unavailable`; credentials are denied rather than defaulted.
    pub fn credential_provider(mut self, provider: Arc<dyn CredentialProvider>) -> Self {
        self.credentials = Some(provider);
        self
    }

    /// Attach the host-side bus exposed as `soil:unibus`.
    ///
    /// Without a bus, `publish` and `poll` fail with `unavailable`; a Spore
    /// cannot fall back to opening its own connection.
    pub fn bus(mut self, bus: Arc<dyn Bus>) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Link `soil:records` for this host. A component that imports the
    /// interface without a book configured fails at instantiation, naming the
    /// import, rather than answering `unsupported` on the first call.
    pub fn records_book(mut self, book: RecordsBookHost) -> Self {
        self.records = Some(Arc::new(Mutex::new(book)));
        self
    }

    pub fn has_records(&self) -> bool {
        self.records.is_some()
    }

    /// Link `soil:http` for this host.
    ///
    /// The capability decides what may be reached and how; without it the
    /// interface is not linked at all, so a component that imports it fails at
    /// instantiation instead of answering `unsupported` on the first fetch.
    pub fn with_http(mut self, capability: HttpCapability) -> Self {
        self.http = Some(Arc::new(capability));
        self
    }

    pub fn has_http(&self) -> bool {
        self.http.is_some()
    }

    /// Create fresh WASI state for one component invocation.
    pub fn build(&self) -> Result<WasiHost> {
        let mut wasi = WasiCtxBuilder::new();
        if self.inherit_stdio {
            wasi.inherit_stdio();
        }
        wasi.args(&self.args);
        if self.inherit_env {
            wasi.inherit_env();
        }
        wasi.envs(&self.env);
        for preopen in &self.preopens {
            wasi.preopened_dir(&preopen.host, &preopen.guest, preopen.perms)?;
        }
        self.policy.network.apply(&mut wasi);
        let mut http = WasiHttpCtx::new();
        http.set_field_size_limit(self.policy.http.max_field_size());
        Ok(WasiHost {
            wasi: wasi.build(),
            http,
            table: ResourceTable::new(),
            hooks: HttpHooks {
                policy: self.policy.http.clone(),
            },
            context: None,
            credentials: self.credentials.clone(),
            bus: self.bus.clone(),
            records: self.records.clone(),
            soil_http: self.http.clone(),
        })
    }

    /// Convenience for hosts that only need a single preopen.
    pub fn preopen_read_only(self, host: impl AsRef<Path>, guest: impl Into<String>) -> Self {
        self.preopen_dir(host.as_ref(), guest, FsPerms::ReadOnly)
    }
}

/// Store data for a component that imports WASI.
pub struct WasiHost {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
    hooks: HttpHooks,
    context: Option<HttpContext>,
    credentials: Option<Arc<dyn CredentialProvider>>,
    bus: Option<Arc<dyn Bus>>,
    records: Option<Arc<Mutex<RecordsBookHost>>>,
    soil_http: Option<Arc<HttpCapability>>,
}

impl soil_host::http_host::api::Host for WasiHost {
    fn fetch(
        &mut self,
        request: soil_host::http_host::api::Request,
    ) -> Result<soil_host::http_host::api::Response, soil_host::http_host::api::Error> {
        let Some(capability) = self.soil_http.as_ref() else {
            return Err(soil_host::http_host::api::Error::Unsupported);
        };
        soil_host::http_host::fetch(
            &capability.policy,
            &soil_host::http_host::NoCredentials,
            capability.transport.as_ref(),
            request,
        )
    }
}

impl WasiHost {
    pub fn new(policy: WasiHostPolicy) -> Result<Self> {
        WasiHostBuilder::new(policy).build()
    }

    /// Set the context reported by `fungos:http-context/current`.
    pub fn set_http_context(&mut self, context: HttpContext) {
        self.context = Some(context);
    }

    /// The current request context, when this store belongs to an HTTP call.
    pub fn http_context(&self) -> Option<&HttpContext> {
        self.context.as_ref()
    }

    pub(crate) fn credential_provider(&self) -> Option<Arc<dyn CredentialProvider>> {
        self.credentials.clone()
    }

    pub(crate) fn bus(&self) -> Option<Arc<dyn Bus>> {
        self.bus.clone()
    }

    /// Run one `soil:records` call under the shared book lock.
    ///
    /// The guard never crosses an await: every trait method is synchronous, so a
    /// blocked writer delays the next request instead of deadlocking the runtime.
    fn with_records<T>(
        &mut self,
        call: impl FnOnce(&mut RecordsBookHost) -> std::result::Result<T, records::Error>,
    ) -> std::result::Result<T, records::Error> {
        let Some(records) = &self.records else {
            return Err(records::Error::Unsupported);
        };
        let mut book = records.lock().map_err(|_| records::Error::Io)?;
        call(&mut book)
    }
}

impl records::Host for WasiHost {
    fn get(&mut self, key: records::Key) -> std::result::Result<records::Entry, records::Error> {
        self.with_records(|book| records::Host::get(book, key))
    }

    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<records::Page, records::Error> {
        self.with_records(|book| records::Host::scan(book, collection, cursor, limit))
    }

    fn upsert(
        &mut self,
        entry: records::Entry,
    ) -> std::result::Result<records::Entry, records::Error> {
        self.with_records(|book| records::Host::upsert(book, entry))
    }

    fn compare_and_swap(
        &mut self,
        key: records::Key,
        expected: Option<records::Version>,
        entry: records::Entry,
    ) -> std::result::Result<records::CasResult, records::Error> {
        self.with_records(|book| records::Host::compare_and_swap(book, key, expected, entry))
    }

    fn delete(&mut self, key: records::Key) -> std::result::Result<(), records::Error> {
        self.with_records(|book| records::Host::delete(book, key))
    }

    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> std::result::Result<records::Page, records::Error> {
        self.with_records(|book| records::Host::changes(book, since, limit))
    }

    fn apply(
        &mut self,
        entries: Vec<records::Entry>,
    ) -> std::result::Result<records::ApplyResult, records::Error> {
        self.with_records(|book| records::Host::apply(book, entries))
    }
}

impl WasiView for WasiHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for WasiHost {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: &mut self.hooks,
        }
    }
}

struct HttpHooks {
    policy: HttpPolicy,
}

impl WasiHttpHooks for HttpHooks {
    fn forbidden_header_filter(&mut self) -> ForbiddenHeaderFilter {
        std::sync::Arc::new(|name| DEFAULT_FORBIDDEN_HEADERS.contains(name))
    }

    fn send_request(
        &mut self,
        request: http::Request<WasiBody>,
        options: Option<RequestOptions>,
        fut: Box<dyn Future<Output = std::result::Result<(), Error>> + Send>,
    ) -> Box<
        dyn Future<
                Output = std::result::Result<
                    (
                        http::Response<WasiBody>,
                        Box<dyn Future<Output = std::result::Result<(), Error>> + Send>,
                    ),
                    Error,
                >,
            > + Send,
    > {
        if !self.policy.allows_uri(request.uri()) {
            drop(fut);
            return Box::new(async { Err(Error::HttpRequestDenied) });
        }
        Box::new(async move {
            let (response, body) =
                wasmtime_wasi_http::default_send_request(request, options).await?;
            let body: Box<dyn Future<Output = std::result::Result<(), Error>> + Send> =
                Box::new(body);
            Ok((response.map(BodyExt::boxed_unsync), body))
        })
    }
}

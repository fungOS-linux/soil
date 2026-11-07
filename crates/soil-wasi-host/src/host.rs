//! Reusable `wasi:cli` + `wasi:http` store state.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use http_body_util::BodyExt;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    DEFAULT_FORBIDDEN_HEADERS, Error, ForbiddenHeaderFilter, RequestOptions, WasiBody, WasiHttpCtx,
    WasiHttpCtxView, WasiHttpHooks, WasiHttpView,
};

use crate::{
    context::HttpContext,
    creds::CredentialProvider,
    events::EventSink,
    policy::{HttpPolicy, WasiHostPolicy},
    unibus::Bus,
};

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
    events: Option<Arc<dyn EventSink>>,
    bus: Option<Arc<dyn Bus>>,
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

    /// Attach the host-side event transport exposed as `fungos:events`.
    ///
    /// Without a sink, `publish` fails with `unavailable`; a Spore cannot fall
    /// back to opening its own connection.
    pub fn event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.events = Some(sink);
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
            events: self.events.clone(),
            bus: self.bus.clone(),
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
    events: Option<Arc<dyn EventSink>>,
    bus: Option<Arc<dyn Bus>>,
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

    pub(crate) fn event_sink(&self) -> Option<Arc<dyn EventSink>> {
        self.events.clone()
    }

    pub(crate) fn bus(&self) -> Option<Arc<dyn Bus>> {
        self.bus.clone()
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

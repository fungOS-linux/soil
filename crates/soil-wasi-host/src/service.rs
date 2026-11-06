//! A minimal host for a component exporting `wasi:http/incoming-handler`.
//!
//! The host owns the listener, HTTP parsing, TLS boundary, timeouts and policy.
//! The Spore sees only `wasi:http` requests and responses.

use std::{convert::Infallible, sync::Arc};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use tokio::net::TcpListener;
use wasmtime::{
    Config, Engine, Store,
    component::{Component, Linker},
};
use wasmtime_wasi_http::{
    WasiHttpView,
    io::TokioIo,
    p2::{
        bindings::{ProxyPre, http::types::Scheme},
        body::HyperOutgoingBody,
    },
};

use crate::host::{WasiHost, WasiHostBuilder};

/// A compiled component plus the policy used to create per-request stores.
pub struct HttpService {
    pre: ProxyPre<WasiHost>,
    builder: WasiHostBuilder,
}

impl HttpService {
    pub fn from_bytes(bytes: &[u8], builder: WasiHostBuilder) -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let component = Component::new(&engine, bytes)
            .map_err(anyhow::Error::from)
            .context("compile wasi:http component")?;
        Self::from_component(component, builder)
    }

    pub fn from_component(component: Component, builder: WasiHostBuilder) -> Result<Self> {
        let mut linker = Linker::new(component.engine());
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link WASI p2")?;
        wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker)
            .map_err(anyhow::Error::from)
            .context("link wasi:http")?;
        let pre = ProxyPre::new(linker.instantiate_pre(&component)?)
            .map_err(anyhow::Error::from)
            .context("component must export wasi:http/incoming-handler")?;
        Ok(Self { pre, builder })
    }

    /// Serve HTTP/1.1 until the listener fails or the task is cancelled.
    pub async fn serve(self: Arc<Self>, listener: TcpListener) -> Result<()> {
        loop {
            let (client, remote) = listener.accept().await?;
            let service = Arc::clone(&self);
            tokio::spawn(async move {
                let result = http1::Builder::new()
                    .keep_alive(true)
                    .serve_connection(
                        TokioIo::new(client),
                        service_fn(move |request| {
                            let service = Arc::clone(&service);
                            async move {
                                Ok::<_, Infallible>(service.handle_request(request).await)
                            }
                        }),
                    )
                    .await;
                if let Err(error) = result {
                    eprintln!("soil-wasi-http: {remote}: {error}");
                }
            });
        }
    }

    async fn handle_request(
        &self,
        request: hyper::Request<Incoming>,
    ) -> hyper::Response<HyperOutgoingBody> {
        match self.try_handle(request).await {
            Ok(response) => response,
            Err(error) => error_response(format!("{error:#}")),
        }
    }

    async fn try_handle(
        &self,
        request: hyper::Request<Incoming>,
    ) -> Result<hyper::Response<HyperOutgoingBody>> {
        let mut store = Store::new(self.pre.engine(), self.builder.build()?);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let request = store
            .data_mut()
            .http()
            .new_incoming_request(Scheme::Http, request)?;
        let outparam = store.data_mut().http().new_response_outparam(sender)?;
        let pre = self.pre.clone();
        let task = tokio::spawn(async move {
            let proxy = pre.instantiate_async(&mut store).await?;
            proxy
                .wasi_http_incoming_handler()
                .call_handle(store, request, outparam)
                .await?;
            Ok::<(), anyhow::Error>(())
        });

        match receiver.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => {
                let _ = task.await;
                bail!("guest returned {error:?}");
            }
            Err(_) => match task.await {
                Ok(Ok(())) => bail!("guest never set a response"),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(error.into()),
            },
        }
    }
}

fn error_response(message: String) -> hyper::Response<HyperOutgoingBody> {
    let body = Full::new(Bytes::from(message))
        .map_err(|never| match never {})
        .boxed_unsync();
    hyper::Response::builder()
        .status(hyper::StatusCode::INTERNAL_SERVER_ERROR)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .expect("static response")
}

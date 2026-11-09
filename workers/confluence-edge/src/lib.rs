//! Cloudflare edge shim for Confluence.
//!
//! The Worker accepts the provider webhook, then hands the same body to a
//! Confluence relay. The relay owns the Iroh hop; a Worker cannot dial Iroh
//! directly (Workers are HTTP/TCP, Iroh is QUIC/UDP).

use worker::*;

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get_async("/health", |_req, _ctx| async move {
            Response::from_json(&serde_json::json!({"ok": true}))
        })
        .post_async("/webhooks/:provider", handler)
        .run(req, env)
        .await
}

async fn handler(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let provider = ctx
        .param("provider")
        .ok_or_else(|| Error::RustError("missing provider".into()))?
        .to_string();
    let relay = ctx.env.var("CONFLUENCE_RELAY_URL")?.to_string();
    let body = req.bytes().await?;
    let content_type = req
        .headers()
        .get("content-type")?
        .unwrap_or_else(|| "application/json".into());

    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_body(Some(body.into()));
    let target = format!("{}/webhooks/{provider}", relay.trim_end_matches('/'));
    let mut outbound = Request::new_with_init(&target, &init)?;
    outbound.headers_mut()?.set("content-type", &content_type)?;

    for name in [
        "webhook-id",
        "webhook-timestamp",
        "webhook-signature",
        "x-event-type",
        "x-github-event",
        "x-github-delivery",
    ] {
        if let Some(value) = req.headers().get(name)? {
            outbound.headers_mut()?.set(name, &value)?;
        }
    }

    let mut response = Fetch::Request(outbound).send().await?;
    let status = response.status_code();
    let body = response.text().await?;
    Response::from_json(&serde_json::json!({"relay_status": status, "relay_body": body}))
}

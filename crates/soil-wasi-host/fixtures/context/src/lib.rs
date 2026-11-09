//! Proves `fungos:http-context` over a real `wasi:http` handler.
//!
//! The response is a single line of host-attested fields, which the host test
//! asserts. Nothing here reads a request header for identity.

use std::io::Write as _;

use wasip2::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

mod context {
    wit_bindgen::generate!({
        path: "../../../../wit/fungos-http-context.wit",
        world: "guest",
    });
}

struct ContextFixture;

impl wasip2::exports::http::incoming_handler::Guest for ContextFixture {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let context = context::fungos::http_context::context::current();
        let body = format!(
            "{}|{}|{}|{}|{}|{}",
            context.request_id,
            context.spore,
            context.route,
            context.peer.unwrap_or_default(),
            context.tls_peer.unwrap_or_default(),
            context.received_at_ms,
        );

        let response = OutgoingResponse::new(Fields::new());
        let response_body = response.body().unwrap();
        ResponseOutparam::set(response_out, Ok(response));

        let mut out = response_body.write().unwrap();
        out.write_all(body.as_bytes()).unwrap();
        out.flush().unwrap();
        drop(out);

        OutgoingBody::finish(response_body, None).unwrap();
    }
}

wasip2::http::proxy::export!(ContextFixture);

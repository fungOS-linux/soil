use std::io::Write as _;

use wasip2::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

wasip2::http::proxy::export!(Echo);

struct Echo;

impl wasip2::exports::http::incoming_handler::Guest for Echo {
    fn handle(_request: IncomingRequest, response_out: ResponseOutparam) {
        let response = OutgoingResponse::new(Fields::new());
        let body = response.body().unwrap();
        ResponseOutparam::set(response_out, Ok(response));

        let mut out = body.write().unwrap();
        out.write_all(b"hello from wasi:http\n").unwrap();
        out.flush().unwrap();
        drop(out);

        OutgoingBody::finish(body, None).unwrap();
    }
}

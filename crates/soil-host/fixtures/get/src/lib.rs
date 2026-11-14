wit_bindgen::generate!({ path: "wit", world: "get", generate_all });

use soil::http::http::{self, Header, Method, Request};

struct Component;

export!(Component);

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<String, String> {
        let url = args.first().cloned().ok_or_else(|| "usage: get URL [credential]".to_string())?;
        let credential = args.get(1).cloned();
        let response = http::fetch(&Request {
            url,
            method: Method::Get,
            headers: vec![Header {
                name: "accept".into(),
                value: "application/rss+xml, application/atom+xml, application/xml, text/xml, */*".into(),
            }],
            body: None,
            credential,
            max_bytes: 1024 * 1024,
            timeout_ms: 10_000,
        })
        .map_err(|error| format!("{error:?}"))?;
        println!("status={} final={} bytes={}", response.status, response.final_url, response.body.len());
        print!("{}", String::from_utf8_lossy(&response.body));
        Ok(String::new())
    }
}

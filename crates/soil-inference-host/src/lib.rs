//! Host implementation of `soil:inference`.
//!
//! The host owns endpoint discovery, Geode credentials, scrubbing, model alias
//! resolution and transport. A Spore names a model, messages and an optional
//! stream; it never sees a provider route or credential.

use std::io::Read;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use soil_inference::{GeodeResolver, SoilConfig};
use soil_scrub::Scrubber;
use wasmtime::component::{HasSelf, Linker, Resource, ResourceTable};

wasmtime::component::bindgen!({
    path: "wit",
    world: "host",
    with: {
        "soil:inference/inference.completion-stream": CompletionStream,
    },
});

use soil::inference::inference as api;

const MAX_CHUNK: usize = 16 * 1024;

pub struct InferenceHost {
    table: ResourceTable,
    agent: ureq::Agent,
    base_url: String,
    api_key: Option<String>,
    provider: String,
    model: String,
    scrubber: Scrubber,
}

pub struct CompletionStream {
    reader: Box<dyn Read + Send + Sync>,
    done: bool,
}

impl InferenceHost {
    pub fn open(config_path: impl AsRef<std::path::Path>, profile: Option<&str>) -> Result<Self> {
        let config = SoilConfig::load(config_path.as_ref())?;
        Self::from_config(config, profile)
    }

    pub fn from_config(config: SoilConfig, profile: Option<&str>) -> Result<Self> {
        let resolved = config.resolve(profile)?;
        let base_url = resolved
            .base_url
            .clone()
            .context("inference profile has no base URL or service endpoint")?;
        let api_key = match resolved.api_key.clone() {
            Some(key) => Some(key),
            None => resolved
                .credential
                .as_deref()
                .map(|handle| GeodeResolver::new(config.geode.clone()).resolve(handle))
                .transpose()?
                .flatten(),
        };
        if api_key.is_none() && resolved.credential.is_some() {
            anyhow::bail!("Geode credential did not resolve");
        }
        Ok(Self::new(
            base_url,
            api_key,
            resolved.provider.unwrap_or_else(|| "unknown".into()),
            resolved.model.unwrap_or_else(|| "default".into()),
        ))
    }

    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            table: ResourceTable::new(),
            agent: ureq::AgentBuilder::new().build(),
            base_url: base_url.into(),
            api_key,
            provider: provider.into(),
            model: model.into(),
            scrubber: Scrubber::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/{}", path.strip_prefix("v1/").unwrap_or(path))
        } else {
            format!("{base}/{path}")
        }
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        let mut request = self.agent.request(method, url);
        if let Some(key) = &self.api_key {
            request = request.set("authorization", &format!("Bearer {key}"));
        }
        request
    }

    fn body(&mut self, request: &api::Request, stream: bool) -> Value {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|message| {
                let mut value = json!({
                    "role": role_name(message.role),
                    "content": self.scrubber.scrub(&message.content),
                });
                if let Some(name) = &message.name {
                    value["name"] = json!(name);
                }
                value
            })
            .collect();
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
        });
        if let Some(max) = request.max_output_tokens {
            body["max_tokens"] = json!(max);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        body
    }

    fn error(error: ureq::Error) -> api::Error {
        match error {
            ureq::Error::Status(401 | 403, _) => api::Error::Auth,
            ureq::Error::Status(408, _) => api::Error::Unavailable,
            ureq::Error::Status(429, _) => api::Error::OverBudget,
            ureq::Error::Status(_, _) | ureq::Error::Transport(_) => api::Error::Unavailable,
        }
    }
}

impl api::Host for InferenceHost {
    fn models(&mut self) -> Result<Vec<api::Model>, api::Error> {
        let response = self
            .request("GET", &self.url("/v1/models"))
            .call()
            .map_err(Self::error)?;
        let value: Value = response.into_json().map_err(|_| api::Error::Io)?;
        let models = value
            .get("data")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|model| {
                        let id = model.get("id")?.as_str()?.to_string();
                        let context = model
                            .get("context_window")
                            .or_else(|| model.get("context_length"))
                            .and_then(Value::as_u64)
                            .and_then(|value| u32::try_from(value).ok());
                        Some(api::Model {
                            id,
                            provider: self.provider.clone(),
                            context_window: context,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(models)
    }

    fn complete(&mut self, request: api::Request) -> Result<api::Response, api::Error> {
        let body = self.body(&request, false);
        let response = self
            .request("POST", &self.url("/v1/chat/completions"))
            .set("content-type", "application/json")
            .send_json(body)
            .map_err(Self::error)?;
        let value: Value = response.into_json().map_err(|_| api::Error::Io)?;
        let choice = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .ok_or(api::Error::Io)?;
        let text = choice
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let usage = value.get("usage").and_then(|usage| {
            Some(api::Usage {
                prompt_tokens: u32::try_from(usage.get("prompt_tokens")?.as_u64()?).ok()?,
                completion_tokens: u32::try_from(usage.get("completion_tokens")?.as_u64()?).ok()?,
                total_tokens: u32::try_from(usage.get("total_tokens")?.as_u64()?).ok()?,
            })
        });
        Ok(api::Response {
            model: value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(&self.model)
                .to_string(),
            text,
            finish_reason,
            usage,
        })
    }

    fn open_stream(
        &mut self,
        request: api::Request,
    ) -> Result<Resource<CompletionStream>, api::Error> {
        let body = self.body(&request, true);
        let response = self
            .request("POST", &self.url("/v1/chat/completions"))
            .set("content-type", "application/json")
            .set("accept", "text/event-stream")
            .send_json(body)
            .map_err(Self::error)?;
        self.table
            .push(CompletionStream {
                reader: response.into_reader(),
                done: false,
            })
            .map_err(|_| api::Error::OverBudget)
    }

    fn state(&mut self) -> Result<api::InferenceState, api::Error> {
        Ok(api::InferenceState {
            ready: true,
            provider: self.provider.clone(),
            model: self.model.clone(),
        })
    }
}

impl api::HostCompletionStream for InferenceHost {
    fn next(
        &mut self,
        stream: Resource<CompletionStream>,
        _timeout_ms: u32,
    ) -> Result<Option<api::Chunk>, api::Error> {
        let stream = self
            .table
            .get_mut(&stream)
            .map_err(|_| api::Error::Invalid)?;
        if stream.done {
            return Ok(Some(api::Chunk {
                data: Vec::new(),
                done: true,
            }));
        }
        let mut buffer = vec![0u8; MAX_CHUNK];
        match stream.reader.read(&mut buffer) {
            Ok(0) => {
                stream.done = true;
                Ok(Some(api::Chunk {
                    data: Vec::new(),
                    done: true,
                }))
            }
            Ok(read) => {
                buffer.truncate(read);
                Ok(Some(api::Chunk {
                    data: buffer,
                    done: false,
                }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => Ok(None),
            Err(_) => Err(api::Error::Io),
        }
    }

    fn cancel(&mut self, stream: Resource<CompletionStream>) -> Result<(), api::Error> {
        let stream = self
            .table
            .get_mut(&stream)
            .map_err(|_| api::Error::Invalid)?;
        stream.done = true;
        Ok(())
    }

    fn drop(&mut self, stream: Resource<CompletionStream>) -> Result<(), wasmtime::Error> {
        if let Ok(mut stream) = self.table.delete(stream) {
            stream.done = true;
        }
        Ok(())
    }
}

fn role_name(role: api::Role) -> &'static str {
    match role {
        api::Role::System => "system",
        api::Role::User => "user",
        api::Role::Assistant => "assistant",
        api::Role::Tool => "tool",
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn lists_models_and_completes_through_an_upstream() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            for _ in 0..2 {
                let request = server.recv().unwrap();
                if request.url().ends_with("/models") {
                    request
                        .respond(
                            tiny_http::Response::from_string(
                                r#"{"data":[{"id":"laya","context_window":4096}]}"#,
                            )
                            .with_header(
                                tiny_http::Header::from_bytes("content-type", "application/json")
                                    .unwrap(),
                            ),
                        )
                        .unwrap();
                } else {
                    request
                        .respond(
                            tiny_http::Response::from_string(
                                r#"{"model":"laya","choices":[{"message":{"content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
                            )
                            .with_header(
                                tiny_http::Header::from_bytes(
                                    "content-type",
                                    "application/json",
                                )
                                .unwrap(),
                            ),
                        )
                        .unwrap();
                }
            }
        });
        let mut host = InferenceHost::new(format!("http://{address}"), None, "umie", "laya");
        let models = api::Host::models(&mut host).unwrap();
        assert_eq!(models[0].id, "laya");
        let response = api::Host::complete(
            &mut host,
            api::Request {
                model: "laya".into(),
                messages: vec![api::Message {
                    role: api::Role::User,
                    content: "hi".into(),
                    name: None,
                }],
                max_output_tokens: None,
                temperature: None,
            },
        )
        .unwrap();
        assert_eq!(response.text, "hello");
        worker.join().unwrap();
    }

    #[test]
    fn streams_sse_chunks() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let request = server.recv().unwrap();
            request
                .respond(
                    tiny_http::Response::from_string(
                        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                    )
                    .with_header(
                        tiny_http::Header::from_bytes("content-type", "text/event-stream").unwrap(),
                    ),
                )
                .unwrap();
        });
        let mut host = InferenceHost::new(format!("http://{address}"), None, "umie", "laya");
        let stream = api::Host::open_stream(
            &mut host,
            api::Request {
                model: "laya".into(),
                messages: vec![],
                max_output_tokens: None,
                temperature: None,
            },
        )
        .unwrap();
        let chunk = api::HostCompletionStream::next(&mut host, stream, 1000)
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&chunk.data).contains("data:"));
        worker.join().unwrap();
    }
}

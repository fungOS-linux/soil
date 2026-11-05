//! Host implementation of `soil:inference`.
//!
//! The host owns endpoint discovery, Geode credentials, scrubbing, model alias
//! resolution and transport. A Spore names a model, messages and an optional
//! stream; it never sees a provider route or credential.

use std::io::Read;

use anyhow::{Context, Result};
use base64::Engine;
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
const BASE64_STANDARD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

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

    fn body(&mut self, request: &api::Request, stream: bool) -> Result<Value, api::Error> {
        let messages: Vec<Value> = request
            .messages
            .iter()
            .map(|message| {
                let scrubbed = message
                    .content
                    .as_deref()
                    .map(|content| self.scrubber.scrub(content));
                let mut value = json!({
                    "role": role_name(message.role),
                    "content": scrubbed.clone().map(Value::String).unwrap_or(Value::Null),
                });
                if !message.attachments.is_empty() {
                    let mut parts = Vec::new();
                    if let Some(text) = scrubbed.filter(|text| !text.is_empty()) {
                        parts.push(json!({"type": "text", "text": text}));
                    }
                    for attachment in &message.attachments {
                        if !attachment.mime_type.starts_with("image/") {
                            return Err(api::Error::Invalid);
                        }
                        if let Some(text) = &attachment.extracted_text {
                            parts.push(json!({
                                "type": "text",
                                "text": format!("[{}]\n{}", attachment.name, self.scrubber.scrub(text)),
                            }));
                        }
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!(
                                    "data:{};base64,{}",
                                    attachment.mime_type,
                                    BASE64_STANDARD.encode(&attachment.data),
                                ),
                            },
                        }));
                    }
                    value["content"] = Value::Array(parts);
                }
                if let Some(name) = &message.name {
                    value["name"] = json!(name);
                }
                if let Some(call_id) = &message.tool_call_id {
                    value["tool_call_id"] = json!(call_id);
                }
                if !message.tool_calls.is_empty() {
                    value["tool_calls"] = json!(
                        message
                            .tool_calls
                            .iter()
                            .map(|call| json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": self.scrubber.scrub(&call.arguments),
                                },
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                Ok(value)
            })
            .collect::<Result<_, api::Error>>()?;
        let mut body = json!({
            "model": request.model,
            "messages": messages,
            "stream": stream,
        });
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    let parameters: Value =
                        serde_json::from_str(&tool.parameters).map_err(|_| api::Error::Invalid)?;
                    Ok(json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": self.scrubber.scrub(&tool.description),
                            "parameters": parameters,
                        },
                    }))
                })
                .collect::<Result<_, api::Error>>()?;
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(max) = request.max_output_tokens {
            body["max_tokens"] = json!(max);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(hint) = &request.prompt_cache_key {
            body["prompt_cache_key"] = json!(hint);
        }
        if stream {
            body["stream_options"] = json!({"include_usage": true});
        }
        Ok(body)
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
        let body = self.body(&request, false)?;
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
        let message = choice.get("message").ok_or(api::Error::Io)?;
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string);
        let reasoning = message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| {
                calls
                    .iter()
                    .enumerate()
                    .filter_map(|(index, call)| {
                        let function = call.get("function")?;
                        Some(api::ToolCall {
                            id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .filter(|id| !id.is_empty())
                                .map(str::to_string)
                                .unwrap_or_else(|| format!("call_{index}")),
                            name: function.get("name")?.as_str()?.to_string(),
                            arguments: function
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
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
            message: api::Message {
                role: api::Role::Assistant,
                content,
                name: None,
                tool_call_id: None,
                tool_calls,
                attachments: Vec::new(),
                reasoning,
            },
            finish_reason,
            usage,
        })
    }

    fn open_stream(
        &mut self,
        request: api::Request,
    ) -> Result<Resource<CompletionStream>, api::Error> {
        let body = self.body(&request, true)?;
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
                    content: Some("hi".into()),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                    attachments: Vec::new(),
                    reasoning: None,
                }],
                tools: Vec::new(),
                max_output_tokens: None,
                temperature: None,
                prompt_cache_key: None,
            },
        )
        .unwrap();
        assert_eq!(response.message.content.as_deref(), Some("hello"));
        worker.join().unwrap();
    }

    #[test]
    fn maps_tools_and_tool_calls_through_the_host_contract() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let worker = thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(body["tools"][0]["function"]["name"], "read_file");
            assert_eq!(body["tools"][0]["function"]["parameters"]["type"], "object");
            assert_eq!(body["messages"][1]["role"], "tool");
            assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
            request
                .respond(
                    tiny_http::Response::from_string(
                        r#"{"model":"laya","choices":[{"message":{"content":null,"tool_calls":[{"id":"call_2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"notes.md\"}"}}]},"finish_reason":"tool_calls"}]}"#,
                    )
                    .with_header(
                        tiny_http::Header::from_bytes("content-type", "application/json").unwrap(),
                    ),
                )
                .unwrap();
        });
        let mut host = InferenceHost::new(format!("http://{address}"), None, "umie", "laya");
        let response = api::Host::complete(
            &mut host,
            api::Request {
                model: "laya".into(),
                messages: vec![
                    api::Message {
                        role: api::Role::User,
                        content: Some("read notes".into()),
                        name: None,
                        tool_call_id: None,
                        tool_calls: Vec::new(),
                        attachments: Vec::new(),
                        reasoning: None,
                    },
                    api::Message {
                        role: api::Role::Tool,
                        content: Some("contents".into()),
                        name: None,
                        tool_call_id: Some("call_1".into()),
                        tool_calls: Vec::new(),
                        attachments: Vec::new(),
                        reasoning: None,
                    },
                ],
                tools: vec![api::Tool {
                    name: "read_file".into(),
                    description: "read a file".into(),
                    parameters: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
                        .into(),
                }],
                max_output_tokens: None,
                temperature: None,
                prompt_cache_key: None,
            },
        )
        .unwrap();
        assert!(response.message.content.is_none());
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(response.message.tool_calls[0].id, "call_2");
        assert_eq!(response.message.tool_calls[0].name, "read_file");
        assert_eq!(
            response.message.tool_calls[0].arguments,
            r#"{"path":"notes.md"}"#
        );
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
                tools: Vec::new(),
                max_output_tokens: None,
                temperature: None,
                prompt_cache_key: None,
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

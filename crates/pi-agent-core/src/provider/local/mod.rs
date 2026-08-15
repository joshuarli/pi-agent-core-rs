//! Local OpenAI-compatible Chat Completions provider.
//!
//! This adapter is intentionally transport-specific but server-agnostic: the caller supplies a
//! base URL and model, and the adapter sends one finite `chat/completions` request through the
//! local `curl` binary. It does not discover a server, read credentials, inspect the home
//! directory, or select a model from the environment. oMLX is the first supported local server;
//! its Laguna XS 2.1 endpoint is represented by [`LocalConfig::laguna_xs_2_1`].

mod config;
mod payload;
mod response;
mod transport;

pub use config::{LocalConfig, LocalConfigError};
use payload::{cancelled_stream, local_payload};
use response::parse_local_response;
use transport::{process_capture_file, split_curl_status, wait_for_child_or_cancellation};

use crate::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use crate::state::Usage;
use std::fmt;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

/// The model ID exposed by the documented 5-bit Laguna checkpoint.
pub const LAGUNA_XS_2_1_MODEL: &str = "Laguna-XS-2.1-5bit";

/// Default local OpenAI-compatible API root used by oMLX.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000/v1";

/// A finite-response local OpenAI-compatible provider.
pub struct LocalProvider {
    config: LocalConfig,
}

impl fmt::Debug for LocalProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalProvider")
            .field("config", &self.config)
            .finish()
    }
}

impl LocalProvider {
    /// Construct a provider from already validated explicit configuration.
    pub fn new(config: LocalConfig) -> Self {
        Self { config }
    }

    fn response_stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelStream {
        if cancellation.is_cancelled() {
            return cancelled_stream();
        }
        match self.complete(request, &cancellation) {
            Ok((mut events, usage)) => {
                if cancellation.is_cancelled() {
                    return cancelled_stream();
                }
                let terminal = events
                    .pop()
                    .expect("local response parser always returns a terminal event");
                events.push(ModelStreamEvent::Usage(usage));
                events.push(terminal);
                ModelStream { events }
            }
            Err(_message) if cancellation.is_cancelled() => cancelled_stream(),
            Err(message) => ModelStream {
                events: vec![ModelStreamEvent::Error { message }],
            },
        }
    }

    fn complete(
        &self,
        request: ModelRequest,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<ModelStreamEvent>, Usage), String> {
        let model = request
            .model
            .as_ref()
            .ok_or_else(|| "local request omitted its model descriptor".to_owned())?;
        if model.provider != "local" || model.model != self.config.model {
            return Err(format!(
                "local provider received model {}/{} but serves local/{}",
                model.provider, model.model, self.config.model
            ));
        }
        let payload = local_payload(&self.config, request)?;
        let (stdout_path, stdout) = process_capture_file("stdout")?;
        let (stderr_path, stderr) = match process_capture_file("stderr") {
            Ok(capture) => capture,
            Err(error) => {
                let _ = fs::remove_file(&stdout_path);
                return Err(error);
            }
        };
        let endpoint = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let output: Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), String> = (|| {
            let timeout = self.config.request_timeout.as_secs().max(1).to_string();
            let mut child = Command::new("/usr/bin/curl")
                .args([
                    "--silent",
                    "--show-error",
                    "--connect-timeout",
                    "10",
                    "--max-time",
                    timeout.as_str(),
                    "--header",
                    "Content-Type: application/json",
                    "--request",
                    "POST",
                    "--data-binary",
                    "@-",
                    "--write-out",
                    "\n%{http_code}",
                    endpoint.as_str(),
                ])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .stdin(Stdio::piped())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|error| format!("could not start local transport: {error}"))?;
            child
                .stdin
                .as_mut()
                .ok_or_else(|| "local transport did not expose request stdin".to_owned())?
                .write_all(payload.as_bytes())
                .map_err(|error| format!("could not write local request: {error}"))?;
            drop(child.stdin.take());
            let status = wait_for_child_or_cancellation(&mut child, cancellation)?;
            let stdout = fs::read(&stdout_path)
                .map_err(|error| format!("cannot read local response capture: {error}"))?;
            let stderr = fs::read(&stderr_path)
                .map_err(|error| format!("cannot read local error capture: {error}"))?;
            Ok((status, stdout, stderr))
        })();
        let _ = fs::remove_file(&stdout_path);
        let _ = fs::remove_file(&stderr_path);
        let (status, stdout, stderr) = output?;
        if cancellation.is_cancelled() {
            return Err("local transport cancelled".to_owned());
        }
        if !status.success() {
            return Err(format!(
                "local transport failed before a provider response: {}",
                String::from_utf8_lossy(&stderr).trim()
            ));
        }
        let (body, status) = split_curl_status(&stdout)?;
        parse_local_response(body, status)
    }
}

impl ModelProvider for LocalProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self.response_stream(request, cancellation);
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        local_payload, parse_local_response, LocalConfig, LocalProvider, LAGUNA_XS_2_1_MODEL,
    };
    use crate::scheduler::{CancellationToken, ModelRequest, ModelStreamEvent};
    use crate::state::{ModelDescriptor, ThinkingLevel, Usage};
    use crate::tool::ToolDefinition;
    use pi_agent_protocol::JsonValue;
    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::net::TcpListener;

    #[test]
    fn laguna_defaults_target_o_mlx_without_ambient_configuration() {
        let config = LocalConfig::laguna_xs_2_1("http://127.0.0.1:8000/v1");
        assert_eq!(config.model(), LAGUNA_XS_2_1_MODEL);
        assert!(config.validate().is_ok());
        assert!(format!("{config:?}").contains("enable_thinking: true"));
    }

    #[test]
    fn payload_uses_o_mlx_thinking_and_openai_tool_shapes() {
        let config = LocalConfig::laguna_xs_2_1("http://127.0.0.1:8000/v1");
        let payload = local_payload(
            &config,
            crate::scheduler::ModelRequest {
                system_prompt: "system".into(),
                context: "[{\"role\":\"user\",\"content\":\"hello\"}]".into(),
                tools: vec![ToolDefinition {
                    name: "write".into(),
                    description: "write a file".into(),
                    schema: JsonValue::object([("type", JsonValue::from("object"))]),
                    execution_mode: crate::tool::ToolExecutionMode::Parallel,
                }],
                model: Some(ModelDescriptor {
                    provider: "local".into(),
                    model: LAGUNA_XS_2_1_MODEL.into(),
                    revision: None,
                }),
                thinking_level: ThinkingLevel::High,
            },
        )
        .expect("payload should serialize");
        assert!(payload.contains("chat_template_kwargs"));
        assert!(payload.contains("enable_thinking"));
        assert!(payload.contains("\"tools\""));
        assert!(payload.contains("\"write\""));
    }

    #[cfg(unix)]
    #[test]
    fn transport_posts_the_serialized_body_to_the_local_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
        let address = listener.local_addr().expect("mock server address");
        let response = br#"{"choices":[{"finish_reason":"stop","message":{"content":"READY","tool_calls":[]}}],"usage":{"prompt_tokens":4,"completion_tokens":1}}"#;
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("mock server should accept");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let body_start = loop {
                let read = stream.read(&mut buffer).expect("mock request should read");
                assert!(read > 0, "mock client closed before headers");
                request.extend_from_slice(&buffer[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&request[..body_start]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")?
                        .trim()
                        .parse::<usize>()
                        .ok()
                })
                .expect("curl should send a content length");
            while request.len() < body_start + content_length {
                let read = stream.read(&mut buffer).expect("mock body should read");
                assert!(read > 0, "mock client closed before body");
                request.extend_from_slice(&buffer[..read]);
            }
            let body = String::from_utf8_lossy(&request[body_start..body_start + content_length]);
            assert!(body.contains("\"model\":\"Laguna-XS-2.1-5bit\""));
            assert!(body.contains("\"enable_thinking\":true"));
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.len()
            );
            stream
                .write_all(header.as_bytes())
                .expect("mock headers should write");
            stream.write_all(response).expect("mock body should write");
        });
        let config = LocalConfig::laguna_xs_2_1(format!("http://{address}/v1"))
            .with_request_timeout(std::time::Duration::from_secs(5));
        let provider = LocalProvider::new(config);
        let request = ModelRequest {
            system_prompt: "system".into(),
            context: "[{\"role\":\"user\",\"content\":\"hello\"}]".into(),
            tools: Vec::new(),
            model: Some(ModelDescriptor {
                provider: "local".into(),
                model: LAGUNA_XS_2_1_MODEL.into(),
                revision: None,
            }),
            thinking_level: ThinkingLevel::High,
        };
        let (events, usage) = provider
            .complete(request, &CancellationToken::new())
            .expect("mock local response should parse");
        server.join().expect("mock server should finish");
        assert!(
            matches!(events.first(), Some(ModelStreamEvent::TextDelta(text)) if text == "READY")
        );
        assert_eq!(usage.input_tokens, Some(4));
        assert_eq!(usage.output_tokens, Some(1));
    }

    #[test]
    fn parses_o_mlx_tool_calls_and_usage() {
        let response = br#"{
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": null,
                    "reasoning_content": "think",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "write", "arguments": "{\"path\":\"a.py\"}"}
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "prompt_tokens_details": {"cached_tokens": 3}
            }
        }"#;
        let (events, usage) = parse_local_response(response, 200).expect("response should parse");
        assert!(matches!(events[0], ModelStreamEvent::ToolCall(_)));
        assert!(matches!(events.last(), Some(ModelStreamEvent::End(_))));
        assert_eq!(
            usage,
            Usage {
                input_tokens: Some(10),
                output_tokens: Some(20),
                cache_read_tokens: Some(3),
                ..Usage::default()
            }
        );
    }
}

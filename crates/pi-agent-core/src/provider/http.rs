//! Small, synchronous HTTPS boundary shared by the opt-in provider adapters.
//!
//! The core deliberately does not expose an HTTP client. Provider features use one explicit
//! `ureq` agent configured with rustls, no ambient proxy discovery, and bounded request phases.
//! Responses are collected before the adapter returns its finite model stream; provider modules
//! retain ownership of status/error classification and response parsing.

#![allow(dead_code)] // provider features consume different request methods and response fields

use super::super::scheduler::CancellationToken;
use std::io::Read;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Method {
    Get,
    Post,
}

#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) timeout: Duration,
    pub(crate) stall_timeout: Option<Duration>,
}

impl Request {
    pub(crate) fn get(url: impl Into<String>, timeout: Duration) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout,
            stall_timeout: None,
        }
    }

    pub(crate) fn post(
        url: impl Into<String>,
        body: impl Into<Vec<u8>>,
        timeout: Duration,
    ) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: body.into(),
            timeout,
            stall_timeout: None,
        }
    }

    pub(crate) fn query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    pub(crate) fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    pub(crate) fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }
}

#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) status_code: u16,
    pub(crate) body: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) message: String,
    pub(crate) status_code: Option<u16>,
    pub(crate) body: Vec<u8>,
    pub(crate) timeout: Option<ureq::Timeout>,
}

impl Failure {
    pub(crate) fn is_stall(&self) -> bool {
        matches!(
            self.timeout,
            Some(ureq::Timeout::RecvResponse | ureq::Timeout::RecvBody)
        )
    }
}

/// Execute one finite request with explicit timeout and cancellation checkpoints.
///
/// `ureq` is intentionally used synchronously: these adapters return finite streams and the
/// repository has no executor-owned blocking bridge. Cancellation is checked before opening the
/// request, between received body chunks, and after the bounded ureq operation settles; the
/// configured total timeout keeps an unresponsive provider from holding the run forever.
pub(crate) fn send(
    request: Request,
    cancellation: &CancellationToken,
) -> Result<Response, Failure> {
    if cancellation.is_cancelled() {
        return Err(Failure {
            message: "HTTP request cancelled".into(),
            status_code: None,
            body: Vec::new(),
            timeout: None,
        });
    }

    let timeout = request.timeout;
    let stall_timeout = request.stall_timeout;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .timeout_resolve(Some(Duration::from_secs(10)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .build()
        .into();

    let result = match request.method {
        Method::Get => {
            let mut builder = agent.get(&request.url);
            for (key, value) in &request.query {
                builder = builder.query(key, value);
            }
            for (key, value) in &request.headers {
                builder = builder.header(key, value);
            }
            let mut config = builder.config().http_status_as_error(false);
            if let Some(stall_timeout) = stall_timeout {
                config = config
                    .timeout_recv_response(Some(stall_timeout))
                    .timeout_recv_body(Some(stall_timeout));
            }
            config.build().call()
        }
        Method::Post => {
            let mut builder = agent.post(&request.url);
            for (key, value) in &request.query {
                builder = builder.query(key, value);
            }
            for (key, value) in &request.headers {
                builder = builder.header(key, value);
            }
            let mut config = builder.config().http_status_as_error(false);
            if let Some(stall_timeout) = stall_timeout {
                config = config
                    .timeout_recv_response(Some(stall_timeout))
                    .timeout_recv_body(Some(stall_timeout));
            }
            config.build().send(&request.body)
        }
    };

    let response = match result {
        Ok(response) => response,
        Err(error) => {
            let timeout = timeout_kind(&error);
            return Err(Failure {
                message: format!("HTTP request failed: {error}"),
                status_code: None,
                body: Vec::new(),
                timeout,
            });
        }
    };
    let status_code = response.status().as_u16();
    let mut body = Vec::new();
    let mut reader = response.into_body().into_reader();
    let mut buffer = [0_u8; 8 * 1024];
    let read_result = loop {
        if cancellation.is_cancelled() {
            return Err(Failure {
                message: "HTTP request cancelled".into(),
                status_code: Some(status_code),
                body,
                timeout: None,
            });
        }
        match reader.read(&mut buffer) {
            Ok(0) => break Ok(()),
            Ok(read) => body.extend_from_slice(&buffer[..read]),
            Err(error) => break Err(error),
        }
    };
    if let Err(error) = read_result {
        let timeout = timeout_kind_from_io(&error);
        return Err(Failure {
            message: format!("HTTP response body read failed: {error}"),
            status_code: Some(status_code),
            body,
            timeout,
        });
    }
    if cancellation.is_cancelled() {
        return Err(Failure {
            message: "HTTP request cancelled".into(),
            status_code: Some(status_code),
            body,
            timeout: None,
        });
    }
    Ok(Response { status_code, body })
}

fn timeout_kind(error: &ureq::Error) -> Option<ureq::Timeout> {
    match error {
        ureq::Error::Timeout(timeout) => Some(*timeout),
        _ => None,
    }
}

fn timeout_kind_from_io(error: &std::io::Error) -> Option<ureq::Timeout> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<ureq::Error>())
        .and_then(timeout_kind)
}

#[cfg(test)]
mod tests {
    use super::{send, Request};
    use crate::scheduler::CancellationToken;
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn preserves_non_success_status_and_response_body_for_provider_parsers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock HTTP server should bind");
        let address = listener.local_addr().expect("mock HTTP server address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("native client should connect");
            stream
                .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 3\r\n\r\nbad")
                .expect("mock response should write");
        });

        let response = send(
            Request::get(format!("http://{address}/status"), Duration::from_secs(2)),
            &CancellationToken::new(),
        )
        .expect("HTTP status errors remain responses for provider parsers");
        assert_eq!(response.status_code, 429);
        assert_eq!(response.body, b"bad");
        server.join().expect("mock HTTP server should finish");
    }

    #[test]
    fn returns_partial_body_when_the_configured_receive_timeout_fires() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock HTTP server should bind");
        let address = listener.local_addr().expect("mock HTTP server address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("native client should connect");
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
                .expect("mock response prefix should write");
            std::thread::sleep(Duration::from_millis(200));
        });

        let failure = send(
            Request::get(format!("http://{address}/stall"), Duration::from_secs(2))
                .with_stall_timeout(Duration::from_millis(50)),
            &CancellationToken::new(),
        )
        .expect_err("a stalled response should be classified as a transport failure");
        assert!(failure.is_stall());
        assert_eq!(failure.status_code, Some(200));
        assert_eq!(failure.body, b"abc");
        server.join().expect("mock HTTP server should finish");
    }
}

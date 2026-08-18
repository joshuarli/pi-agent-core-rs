use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

const ROWS: u16 = 24;
const COLUMNS: u16 = 100;

struct PtyApp {
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    parser: vt100::Parser,
    output: Receiver<Vec<u8>>,
    _reader: thread::JoinHandle<()>,
}

impl PtyApp {
    fn launch(openrouter_url: &str) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: ROWS,
                cols: COLUMNS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("test PTY should open");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_pi-agent"));
        command.args([
            "--provider",
            "openrouter",
            "--model",
            "openai/gpt-5.6-luna",
        ]);
        command.env_clear();
        command.env("TERM", "xterm-256color");
        command.env("OPENROUTER_API_KEY", "offline-test-key");
        command.env("PI_AGENT_TUI_TEST_OPENROUTER_URL", openrouter_url);
        let child = pair
            .slave
            .spawn_command(command)
            .expect("real pi-agent binary should start in the PTY");
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .expect("test PTY should clone output reader");
        let writer = pair
            .master
            .take_writer()
            .expect("test PTY should own input writer");
        let (sender, output) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut buffer = [0_u8; 4_096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) if sender.send(buffer[..count].to_vec()).is_err() => break,
                    Ok(_) => {}
                }
            }
        });
        Self {
            writer,
            child,
            parser: vt100::Parser::new(ROWS, COLUMNS, 0),
            output,
            _reader: reader,
        }
    }

    fn send(&mut self, input: &[u8]) {
        self.writer
            .write_all(input)
            .expect("PTY should accept test input");
        self.writer.flush().expect("PTY input should flush");
    }

    fn wait_for_text(&mut self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            self.drain_output();
            let screen = self.screen_text();
            if screen.contains(expected) {
                return;
            }
            if Instant::now() >= deadline {
                panic!("terminal never displayed {expected:?}; screen was:\n{screen}");
            }
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(20)) {
                self.parser.process(&bytes);
            }
        }
    }

    fn screen_text(&mut self) -> String {
        self.drain_output();
        let screen = self.parser.screen();
        (0..ROWS)
            .map(|row| {
                (0..COLUMNS)
                    .map(|column| {
                        screen
                            .cell(row, column)
                            .and_then(|cell| cell.contents().chars().next())
                            .unwrap_or(' ')
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("real pi-agent process state should be readable")
            {
                assert!(status.success(), "pi-agent exited unsuccessfully: {status}");
                return;
            }
            if Instant::now() >= deadline {
                panic!("real pi-agent binary did not exit");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn drain_output(&mut self) {
        while let Ok(bytes) = self.output.try_recv() {
            self.parser.process(&bytes);
        }
    }
}

impl Drop for PtyApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

struct OpenRouterFixture {
    first_delta: Receiver<()>,
    release_response: Sender<()>,
    server: thread::JoinHandle<()>,
    url: String,
}

impl OpenRouterFixture {
    fn start() -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("offline mock HTTP server should bind");
        let address = listener
            .local_addr()
            .expect("offline mock HTTP server address");
        let (first_delta_sent, first_delta) = mpsc::channel();
        let (release_response, wait_for_release) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("OpenRouter request should connect");
            let first = br#"data: {"id":"offline","choices":[{"delta":{"content":"first "},"finish_reason":null}]}

"#;
            let second = br#"data: {"id":"offline","choices":[{"delta":{"content":"second"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":2}}

data: [DONE]

"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        first.len() + second.len()
                    )
                    .as_bytes(),
                )
                .expect("mock response headers should write");
            socket
                .write_all(first)
                .expect("first SSE record should write");
            socket.flush().expect("first SSE record should flush");
            first_delta_sent
                .send(())
                .expect("test waits for the first SSE record");
            wait_for_release
                .recv()
                .expect("test releases the final SSE records");
            socket
                .write_all(second)
                .expect("final SSE records should write");
        });
        Self {
            first_delta,
            release_response,
            server,
            url: format!("http://{address}/v1/chat/completions"),
        }
    }

    fn wait_for_first_delta(&self) {
        self.first_delta
            .recv_timeout(Duration::from_secs(3))
            .expect("pi-agent should request the offline OpenRouter fixture");
    }

    fn release(self) {
        self.release_response
            .send(())
            .expect("offline fixture should still await settlement");
        self.server
            .join()
            .expect("offline mock HTTP server should finish");
    }
}

#[test]
fn real_binary_renders_openrouter_text_before_the_mock_response_settles() {
    let fixture = OpenRouterFixture::start();
    let mut app = PtyApp::launch(&fixture.url);
    app.wait_for_text("openrouter/openai/gpt-5.6-luna");

    app.send(b"stream offline response\r");
    fixture.wait_for_first_delta();
    app.wait_for_text("assistant: first");
    assert!(
        !app.screen_text().contains("second"),
        "terminal displayed unreleased response content"
    );

    fixture.release();
    app.wait_for_text("assistant: first second");
    app.wait_for_text("openrouter/openai/gpt-5.6-luna | idle");
    app.send(b"\x03");
    app.wait_for_exit();
}

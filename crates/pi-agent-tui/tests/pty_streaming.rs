use ptytest::{CommandSpec, ExitStatus, Key, ProtocolProfile, PtyTest, Scenario, Size, TestEnv};
use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

const ROWS: u16 = 24;
const COLUMNS: u16 = 100;

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
            let (mut socket, _) = listener
                .accept()
                .expect("OpenRouter request should connect");
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
    let scenario = Scenario::new("OpenRouter streaming")
        .expect("valid scenario label")
        .command(
            CommandSpec::new(env!("CARGO_BIN_EXE_pi-agent"))
                .args(["--provider", "openrouter", "--model", "openai/gpt-5.6-luna"])
                .secret_env("OPENROUTER_API_KEY", "offline-test-key")
                .env("PI_AGENT_TUI_TEST_OPENROUTER_URL", &fixture.url),
        )
        .size(Size::new(COLUMNS, ROWS).expect("constant terminal size"))
        .environment(TestEnv::hermetic().expect("create hermetic test environment"))
        .protocol_profile(ProtocolProfile::xterm_minimal_v1());
    let mut terminal =
        PtyTest::spawn(scenario).expect("real pi-agent binary should start in a PTY");
    let baseline = terminal.terminal_baseline();

    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "model readiness",
            |screen| screen.contains("𝒑i-agent") && screen.contains("yolo · gpt-5.6-luna"),
        )
        .expect("model selection should render");
    let active = terminal.terminal_state();
    assert!(
        active.modes.alternate_screen,
        "TerminalGuard enters the alternate screen"
    );
    assert!(
        active.modes.bracketed_paste,
        "TerminalGuard enables bracketed paste"
    );
    assert!(
        active.modes.cursor_visible,
        "the local composer owns a visible cursor"
    );
    terminal
        .resize(Size::new(40, 10).expect("constant narrow terminal size"))
        .expect("resize through the kernel PTY");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "narrow redraw",
            |screen| {
                screen.size() == Size::new(40, 10).expect("constant narrow terminal size")
                    && screen.contains("yolo · gpt-5.6-luna")
            },
        )
        .expect("application remains rendered after terminal resize");

    terminal
        .send_text(
            terminal.deadline(Duration::from_secs(3)),
            "stream offline response",
        )
        .expect("send streaming prompt");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "typed prompt",
            |screen| screen.contains("stream offline response"),
        )
        .expect("typed prompt should render");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Enter)
        .expect("submit streaming command");
    fixture.wait_for_first_delta();
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "first released streaming token",
            |screen| screen.contains("first"),
        )
        .expect("first token should render before fixture settlement");
    terminal
        .drain(terminal.deadline(Duration::from_secs(3)))
        .expect("drain available output");
    assert!(
        !terminal.screen().contains("second"),
        "terminal displayed unreleased response content"
    );

    fixture.release();
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "stream completion",
            |screen| screen.contains("first") && screen.contains("second"),
        )
        .expect("complete response should render");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "idle after completion",
            |screen| screen.contains("yolo · gpt-5.6-luna"),
        )
        .expect("application should become idle");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Ctrl('c'))
        .expect("send clean interrupt");
    assert_eq!(
        terminal
            .wait_for_exit(terminal.deadline(Duration::from_secs(3)))
            .expect("wait for pi-agent exit"),
        ExitStatus::Code(0)
    );
    terminal
        .assert_terminal_restored(&baseline)
        .expect("normal exit restores applicable terminal modes");
    terminal
        .finish(terminal.deadline(Duration::from_secs(3)))
        .expect("reap pi-agent");
}

#[test]
fn real_binary_keeps_native_multiline_editing_and_history_inside_a_pty() {
    let scenario = Scenario::new("native composer interaction")
        .expect("valid scenario label")
        .command(
            CommandSpec::new(env!("CARGO_BIN_EXE_pi-agent"))
                .args(["--provider", "local", "--model", "Laguna-XS-2.1-5bit"]),
        )
        .size(Size::new(80, 16).expect("constant terminal size"))
        .environment(TestEnv::hermetic().expect("create hermetic test environment"))
        .protocol_profile(ProtocolProfile::xterm_minimal_v1());
    let mut terminal =
        PtyTest::spawn(scenario).expect("real pi-agent binary should start in a PTY");
    let baseline = terminal.terminal_baseline();

    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "local model readiness",
            |screen| {
                screen.contains("𝒑i-agent")
                    && screen.contains("yolo · Laguna-XS-2.1-5bit")
                    && screen.row(1).is_some_and(|row| row.starts_with("┃"))
            },
        )
        .expect("local model selection should render");

    terminal
        .send_bytes(
            terminal.deadline(Duration::from_secs(3)),
            b"\x1b[200~first line\n  second line\x1b[201~",
        )
        .expect("send bracketed multiline paste");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "multiline composer",
            |screen| screen.contains("first line") && screen.contains("second line"),
        )
        .expect("multiline paste should remain visible in the composer");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Ctrl('c'))
        .expect("clear multiline composer");

    terminal
        .send_text(terminal.deadline(Duration::from_secs(3)), "/model")
        .expect("send model command");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Enter)
        .expect("open model selector");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "cross-provider model selector",
            |screen| {
                screen.contains("Models")
                    && screen.contains("OpenRouter · openai/gpt-5.6-luna")
                    && screen.contains("Local OpenAI-compatible server · Laguna-XS-2.1-5bit")
            },
        )
        .expect("model selector should show compiled models across providers");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Escape)
        .expect("close model selector");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "model selector closed",
            |screen| !screen.contains("Models"),
        )
        .expect("Esc should close the model selector");

    terminal
        .send_text(terminal.deadline(Duration::from_secs(3)), "/he")
        .expect("send command prefix");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Tab)
        .expect("complete command prefix");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "command completion",
            |screen| screen.contains("/help"),
        )
        .expect("Tab should complete the command");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Enter)
        .expect("submit help command");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "help output",
            |screen| screen.contains("keys: Enter submit"),
        )
        .expect("help command should render its key summary");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Up)
        .expect("recall command history");
    terminal
        .wait_for_screen(
            terminal.deadline(Duration::from_secs(3)),
            "history recall",
            |screen| screen.contains("/help"),
        )
        .expect("Up should restore the submitted command");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Ctrl('c'))
        .expect("clear recalled command");
    terminal
        .send_key(terminal.deadline(Duration::from_secs(3)), Key::Ctrl('c'))
        .expect("send clean interrupt");
    assert_eq!(
        terminal
            .wait_for_exit(terminal.deadline(Duration::from_secs(3)))
            .expect("wait for pi-agent exit"),
        ExitStatus::Code(0)
    );
    terminal
        .assert_terminal_restored(&baseline)
        .expect("normal exit restores applicable terminal modes");
    terminal
        .finish(terminal.deadline(Duration::from_secs(3)))
        .expect("reap pi-agent");
}

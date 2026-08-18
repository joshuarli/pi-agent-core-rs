use super::*;
use pi_agent_core::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use pi_agent_core::state::{Message, MessageId, SerializedJson, ToolCallId};
use pi_agent_core::tool::ToolUpdate;
use pi_agent_core::{AgentToolResult, DefaultCodingTools, ModelDescriptor, ThinkingLevel, Usage};
use std::ffi::OsString;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;

#[derive(Debug)]
struct ContextCheckingProvider;

impl ModelProvider for ContextCheckingProvider {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let events = if request.context == r#"[{"content":"hello","role":"user"}]"# {
            vec![
                ModelStreamEvent::TextDelta("ok".into()),
                ModelStreamEvent::End(pi_agent_core::state::StopReason::EndTurn),
            ]
        } else {
            vec![ModelStreamEvent::Error {
                message: "OpenRouter received invalid converted context".into(),
            }]
        };
        Box::pin(std::future::ready(
            Ok(Box::new(ModelStream { events }) as _),
        ))
    }
}

#[test]
fn cli_rejects_ambiguous_and_unknown_inputs() {
    assert!(matches!(
        CliOptions::parse(
            ["pi-agent", "--provider", "one", "--provider", "two"].map(OsString::from)
        ),
        Err(CliError::DuplicateOption("--provider"))
    ));
    assert!(matches!(
        CliOptions::parse(["pi-agent", "unexpected"].map(OsString::from)),
        Err(CliError::UnexpectedArgument(_))
    ));
}

#[test]
fn cli_help_accepts_short_and_long_forms() {
    assert_eq!(
        CliOptions::parse_command(["pi-agent", "-h"].map(OsString::from)),
        Ok(CliCommand::Help)
    );
    assert_eq!(
        CliOptions::parse_command(["pi-agent", "--help"].map(OsString::from)),
        Ok(CliCommand::Help)
    );
    assert!(CliOptions::help_text().contains("--provider <id>"));
}

#[test]
fn cli_parses_one_shot_prompt_and_thinking_level() {
    let CliCommand::Options(options) = CliOptions::parse_command(
        [
            "pi-agent",
            "--provider",
            "openrouter",
            "--model",
            "poolside/laguna-xs-2.1:free",
            "--thinking",
            "high",
            "-p",
            "say hi",
        ]
        .map(OsString::from),
    )
    .expect("one-shot options parse")
    else {
        panic!("one-shot options unexpectedly parsed as help");
    };
    assert_eq!(options.provider(), Some(std::ffi::OsStr::new("openrouter")));
    assert_eq!(
        options.model(),
        Some(std::ffi::OsStr::new("poolside/laguna-xs-2.1:free"))
    );
    assert_eq!(options.prompt(), Some(std::ffi::OsStr::new("say hi")));
    assert_eq!(options.thinking_level(), ThinkingLevel::High);
}

#[test]
fn cli_rejects_unknown_thinking_level() {
    assert!(matches!(
        CliOptions::parse(["pi-agent", "--thinking", "turbo"].map(OsString::from)),
        Err(CliError::InvalidValue {
            flag: "--thinking",
            ..
        })
    ));
}

#[test]
fn event_projection_keeps_streaming_text_as_one_raw_line() {
    let mut state = AppState::new();
    let message = Message::Assistant {
        id: MessageId(2),
        content: "hello".into(),
        tool_calls: Vec::new(),
        stop_reason: None,
        error_message: None,
    };
    state.apply_event(&pi_agent_core::AgentEvent {
        run_id: pi_agent_core::RunId(1),
        sequence: pi_agent_core::EventSequence(1),
        kind: pi_agent_core::event::AgentEventKind::MessageUpdate {
            message: message.clone(),
            text_delta: Some("hel".into()),
        },
    });
    state.apply_event(&pi_agent_core::AgentEvent {
        run_id: pi_agent_core::RunId(1),
        sequence: pi_agent_core::EventSequence(2),
        kind: pi_agent_core::event::AgentEventKind::MessageUpdate {
            message,
            text_delta: Some("lo".into()),
        },
    });
    assert_eq!(state.transcript().len(), 1);
    assert_eq!(state.transcript()[0].text, "assistant: hello");
}

#[test]
fn event_projection_groups_a_tool_lifecycle_in_one_readable_row() {
    let mut state = AppState::new();
    let call_id = ToolCallId::new("call-1").expect("fixture ID");
    let event = |sequence, kind| pi_agent_core::AgentEvent {
        run_id: pi_agent_core::RunId(1),
        sequence: pi_agent_core::EventSequence(sequence),
        kind,
    };
    state.apply_event(&event(
        1,
        pi_agent_core::AgentEventKind::ToolExecutionStart {
            tool_call_id: call_id.clone(),
            tool_name: "shell".into(),
            arguments: SerializedJson::new(r#"{"command":"cargo test"}"#),
        },
    ));
    state.apply_event(&event(
        2,
        pi_agent_core::AgentEventKind::ToolExecutionUpdate {
            tool_call_id: call_id.clone(),
            tool_name: "shell".into(),
            update: ToolUpdate {
                content: "compiling".into(),
                details: None,
            },
        },
    ));
    state.apply_event(&event(
        3,
        pi_agent_core::AgentEventKind::ToolExecutionEnd {
            tool_call_id: call_id.clone(),
            tool_name: "shell".into(),
            result: AgentToolResult {
                tool_call_id: call_id,
                content: "exit 1".into(),
                details: None,
                usage: None,
                added_tool_names: Vec::new(),
                terminate: false,
                is_error: true,
                failure: None,
            },
        },
    ));

    assert_eq!(state.transcript().len(), 1);
    assert_eq!(state.transcript()[0].text, "tool shell — failed: exit 1");
}

#[test]
fn event_projection_makes_provider_failure_and_abort_explicit() {
    let mut state = AppState::new();
    state.apply_event(&pi_agent_core::AgentEvent {
        run_id: pi_agent_core::RunId(1),
        sequence: pi_agent_core::EventSequence(1),
        kind: pi_agent_core::AgentEventKind::MessageEnd {
            message: Message::Assistant {
                id: MessageId(2),
                content: String::new(),
                tool_calls: Vec::new(),
                stop_reason: Some(pi_agent_core::state::StopReason::Error),
                error_message: Some("provider rejected the request".into()),
            },
        },
    });
    state.apply_event(&pi_agent_core::AgentEvent {
        run_id: pi_agent_core::RunId(1),
        sequence: pi_agent_core::EventSequence(2),
        kind: pi_agent_core::AgentEventKind::TurnEnd {
            turn_id: pi_agent_core::TurnId(1),
            reason: pi_agent_core::state::StopReason::Aborted,
        },
    });

    assert_eq!(
        state
            .transcript()
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>(),
        ["assistant error: provider rejected the request", "turn aborted"]
    );
}

#[test]
fn accounting_does_not_render_unknown_as_zero() {
    assert_eq!(
        format_usage(&Usage::default()),
        "provider reported no accounting"
    );
    assert_eq!(
        format_usage(&Usage {
            output_tokens: Some(0),
            ..Usage::default()
        }),
        "out 0"
    );
    assert_eq!(
        support::format_footer_usage(&Usage::default()),
        "in unknown out unknown reasoning unknown cache-read unknown cache-write unknown cost unknown"
    );
    assert_eq!(
        support::format_footer_usage(&Usage {
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(7),
            cost: Some("0.000001".into()),
            ..Usage::default()
        }),
        "in unknown out unknown reasoning unknown cache-read 0 cache-write 7 cost 0.000001"
    );
}

#[test]
fn footer_reports_unknown_context_and_unavailable_compaction_without_guessing() {
    let state = AppState::new();
    let registry = pi_agent_core::provider::ProviderRegistry::new();
    assert_eq!(
        state.footer_lines(&registry)[1],
        "context unknown/unknown; automatic compaction unavailable"
    );
}

#[test]
fn civil_date_epoch_is_stable_without_a_time_dependency() {
    assert_eq!(support::civil_from_days(0), (1970, 1, 1));
    assert_eq!(support::civil_from_days(20_000), (2024, 10, 4));
}

#[test]
fn headless_host_agent_sends_openai_compatible_context() {
    smol::block_on(async {
        let workspace = std::env::current_dir().expect("test workspace");
        let tools = DefaultCodingTools::new(workspace).expect("default tools");
        let agent = build_host_agent(tools)
            .expect("host agent builder")
            .model(ModelDescriptor {
                provider: "openrouter".into(),
                model: "inclusionai/ling-3.0-tiny:free".into(),
                revision: None,
            })
            .model_provider(Arc::new(ContextCheckingProvider))
            .build();

        agent
            .start_prompt("hello")
            .expect("start prompt")
            .drive()
            .await
            .expect("headless host request should be valid JSON");
    });
}

#[test]
fn clear_refuses_an_active_core_agent_without_cancelling_it() {
    let agent = pi_agent_core::Agent::builder().build();
    let active = agent.start_prompt("active").expect("run starts");
    let mut app = App::new(CliOptions::default());
    app.attach_agent(agent.clone());

    app.dispatch_command("/clear").expect("command is handled");

    assert!(matches!(
        app.state().status(),
        UiStatus::Notice(notice) if notice == "/clear is unavailable while a run is active"
    ));
    assert!(matches!(
        agent.snapshot().phase,
        pi_agent_core::state::AgentPhase::Running(_)
    ));
    active.abort().expect("fixture cleanup");
}

#[test]
fn active_commands_refuse_without_replacing_or_compacting_the_agent() {
    let agent = pi_agent_core::Agent::builder().build();
    let active = agent.start_prompt("active").expect("run starts");
    let mut app = App::new(CliOptions::default());
    app.attach_agent(agent.clone());

    for command in ["/provider", "/model", "/compact", "/clear"] {
        app.dispatch_command(command).expect("command is handled");
        assert!(matches!(
            app.state().status(),
            UiStatus::Notice(notice) if notice == &format!("{command} is unavailable while a run is active")
        ));
        assert!(matches!(
            agent.snapshot().phase,
            pi_agent_core::state::AgentPhase::Running(_)
        ));
    }
    active.abort().expect("fixture cleanup");
}

#[test]
fn queue_commands_project_core_owned_steering_and_follow_up_prompts() {
    let agent = pi_agent_core::Agent::builder().build();
    let mut app = App::new(CliOptions::default());
    app.attach_agent(agent.clone());

    app.dispatch_command("/steer inspect the error")
        .expect("steering command is handled");
    app.dispatch_command("/followup summarize the result")
        .expect("follow-up command is handled");

    assert_eq!(
        app.state()
            .queued_lines()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "queued steering #1 (next active turn; one at a time): inspect the error",
            "queued follow-up #1 (next idle boundary; one at a time): summarize the result",
        ]
    );
    let queues = agent.queue_snapshot();
    assert_eq!(queues.steering.snapshot()[0].content, "inspect the error");
    assert_eq!(
        queues.follow_up.snapshot()[0].content,
        "summarize the result"
    );
}

#[test]
fn provider_failure_restores_the_submitted_prompt_for_an_explicit_resubmit() {
    let mut app = App::new(CliOptions::default());
    app.submitted_prompt = Some("inspect the failing test".into());
    let (sender, receiver) = sync_channel(1);
    sender
        .send(Err(pi_agent_core::CoreError::ModelError {
            message: "rate limited".into(),
        }))
        .expect("test receiver remains open");
    app.active_task = Some(receiver);

    app.reap_task();

    assert_eq!(app.state().composer().text(), "inspect the failing test");
    assert!(matches!(
        app.state().status(),
        UiStatus::Notice(notice) if notice.contains("prompt restored for explicit re-submit")
    ));
}

use super::*;
use pi_agent_core::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use pi_agent_core::state::{Message, MessageId};
use pi_agent_core::{DefaultCodingTools, ModelDescriptor, Usage};
use std::ffi::OsString;
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
        UiStatus::Notice(notice) if notice == "cannot clear while the agent is active"
    ));
    assert!(matches!(
        agent.snapshot().phase,
        pi_agent_core::state::AgentPhase::Running(_)
    ));
    active.abort().expect("fixture cleanup");
}

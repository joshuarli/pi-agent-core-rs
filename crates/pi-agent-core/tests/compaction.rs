use pi_agent_core::compaction::{
    CompactionContext, CompactionError, CompactionFuture, CompactionResult, Compactor,
};
use pi_agent_core::event::{AgentEventKind, CompactionOutcome};
use pi_agent_core::scheduler::{
    CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
};
use pi_agent_core::state::StopReason;
use pi_agent_core::{Agent, CoreError};
use std::sync::{Arc, Mutex};

struct FixtureProvider {
    streams: Mutex<Vec<ModelStream>>,
}

impl ModelProvider for FixtureProvider {
    fn stream<'a>(
        &'a self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelFuture<'a> {
        let stream = self
            .streams
            .lock()
            .expect("fixture provider mutex poisoned")
            .remove(0);
        Box::pin(std::future::ready(Ok(Box::new(stream) as _)))
    }
}

struct KeepFirstMessage;

impl Compactor for KeepFirstMessage {
    fn compact<'a>(
        &'a self,
        mut context: CompactionContext,
        _cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        context.messages.truncate(1);
        Box::pin(std::future::ready(Ok(CompactionResult::new(
            context.messages,
        ))))
    }
}

struct DuplicateMessage;

impl Compactor for DuplicateMessage {
    fn compact<'a>(
        &'a self,
        context: CompactionContext,
        _cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        let message = context.messages[0].clone();
        Box::pin(std::future::ready(Ok(CompactionResult::new(vec![
            message.clone(),
            message,
        ]))))
    }
}

struct FailingCompactor;

impl Compactor for FailingCompactor {
    fn compact<'a>(
        &'a self,
        _context: CompactionContext,
        _cancellation: CancellationToken,
    ) -> CompactionFuture<'a> {
        Box::pin(std::future::ready(Err(CompactionError::failed(
            "fixture compactor failed",
        ))))
    }
}

fn provider_with_answers(answers: &[&str]) -> Arc<FixtureProvider> {
    Arc::new(FixtureProvider {
        streams: Mutex::new(
            answers
                .iter()
                .map(|answer| ModelStream {
                    events: vec![
                        ModelStreamEvent::TextDelta((*answer).into()),
                        ModelStreamEvent::End(StopReason::EndTurn),
                    ],
                })
                .collect(),
        ),
    })
}

#[test]
fn compaction_replaces_context_emits_its_grammar_and_allows_reuse() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(provider_with_answers(&["first answer", "second answer"]))
            .compactor(Arc::new(KeepFirstMessage))
            .build();
        agent
            .start_prompt("first prompt")
            .expect("first run starts")
            .drive()
            .await
            .expect("first run succeeds");

        let compaction = agent.start_compaction().expect("compaction starts");
        compaction.drive().await.expect("compaction succeeds");

        let snapshot = agent.snapshot();
        assert_eq!(snapshot.messages.len(), 1);
        assert!(compaction.events().iter().any(|event| {
            matches!(
                event.kind,
                AgentEventKind::CompactionStart {
                    source_message_count: 2
                }
            )
        }));
        assert!(compaction.events().iter().any(|event| {
            matches!(
                event.kind,
                AgentEventKind::CompactionResult {
                    retained_message_count: 1,
                    ..
                }
            )
        }));
        assert!(matches!(
            compaction.events().last().map(|event| &event.kind),
            Some(AgentEventKind::CompactionEnd {
                outcome: CompactionOutcome::Succeeded {
                    retained_message_count: 1
                }
            })
        ));

        agent
            .start_prompt("second prompt")
            .expect("compacted agent is idle and reusable")
            .drive()
            .await
            .expect("second run succeeds");
    });
}

#[test]
fn invalid_replacement_and_compactor_failure_preserve_history() {
    smol::block_on(async {
        let invalid_agent = Agent::builder()
            .model_provider(provider_with_answers(&["answer"]))
            .compactor(Arc::new(DuplicateMessage))
            .build();
        invalid_agent
            .start_prompt("prompt")
            .expect("run starts")
            .drive()
            .await
            .expect("run succeeds");
        let original = invalid_agent.snapshot().messages;
        let error = invalid_agent
            .start_compaction()
            .expect("compaction reserves idle agent")
            .drive()
            .await
            .expect_err("duplicate message IDs are invalid");
        assert!(matches!(error, CoreError::Compaction(_)));
        assert_eq!(invalid_agent.snapshot().messages, original);

        let failing_agent = Agent::builder()
            .model_provider(provider_with_answers(&["answer"]))
            .compactor(Arc::new(FailingCompactor))
            .build();
        failing_agent
            .start_prompt("prompt")
            .expect("run starts")
            .drive()
            .await
            .expect("run succeeds");
        let original = failing_agent.snapshot().messages;
        let error = failing_agent
            .start_compaction()
            .expect("compaction reserves idle agent")
            .drive()
            .await
            .expect_err("compactor failure is surfaced");
        assert!(matches!(error, CoreError::Compaction(_)));
        assert_eq!(failing_agent.snapshot().messages, original);
    });
}

#[test]
fn compaction_rejects_an_active_run_and_cancellation_preserves_history() {
    smol::block_on(async {
        let agent = Agent::builder()
            .model_provider(provider_with_answers(&["answer"]))
            .compactor(Arc::new(KeepFirstMessage))
            .build();
        let active = agent.start_prompt("active").expect("run starts");
        assert!(matches!(
            agent.start_compaction(),
            Err(CoreError::ActiveRun { .. })
        ));
        active.abort().expect("created run aborts");

        let original = agent.snapshot().messages;
        let cancellable = agent.start_compaction().expect("compaction starts");
        agent.abort();
        assert!(matches!(
            cancellable.drive().await,
            Err(CoreError::Cancelled)
        ));
        assert!(matches!(
            cancellable.events().last().map(|event| &event.kind),
            Some(AgentEventKind::CompactionEnd {
                outcome: CompactionOutcome::Cancelled
            })
        ));
        assert_eq!(agent.snapshot().messages, original);
    });
}

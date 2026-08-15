use crate::composer::Composer;
use pi_agent_core::event::{
    AgentEventKind, AutomaticCompactionOutcome, CompactionOutcome, ProviderRequestSkipReason,
};
use pi_agent_core::provider::ProviderRegistry;
use pi_agent_core::state::AgentSnapshot;
use pi_agent_core::{AgentEvent, ModelDescriptor};

use super::host::{missing_credential, model_candidates, overlay_lines, provider_candidates};
use super::support::format_usage;

/// One display row derived from a core event, never a second source of state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptLine {
    /// Core event sequence, or `None` for a local command/help notice.
    pub sequence: Option<u64>,
    /// Raw, deliberately unrendered text for the v0 terminal projection.
    pub text: String,
}

/// Presentation-only status for the fixed status line.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub enum UiStatus {
    /// No operation currently owns the core agent.
    #[default]
    Idle,
    /// A model/tool or compaction operation is active.
    Active,
    /// A concise local notice is displayed.
    Notice(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Picker {
    Provider {
        filter: String,
        selected: usize,
    },
    Model {
        provider: String,
        filter: String,
        selected: usize,
    },
    CustomModel {
        provider: String,
        input: String,
    },
}

/// Terminal-owned state: event-derived rows plus local input and overlay state.
#[derive(Clone, Debug, Default)]
pub struct AppState {
    pub(super) transcript: Vec<TranscriptLine>,
    pub(super) composer: Composer,
    pub(super) status: UiStatus,
    pub(super) viewport_offset: usize,
    pub(super) follow_output: bool,
    pub(super) visible_transcript_lines: usize,
    pub(super) transcript_rows: usize,
    pub(super) last_snapshot: Option<AgentSnapshot>,
    pub(super) selected_model: Option<ModelDescriptor>,
    pub(super) picker: Option<Picker>,
    pub(super) streaming_line: Option<usize>,
}

impl AppState {
    /// Create an empty projection.
    pub fn new() -> Self {
        Self {
            follow_output: true,
            ..Self::default()
        }
    }

    /// Apply one typed core event after its reducer has committed state.
    pub fn apply_event(&mut self, event: &AgentEvent) {
        let sequence = Some(event.sequence.0);
        match &event.kind {
            AgentEventKind::AgentStart => self.status = UiStatus::Active,
            AgentEventKind::MessageStart { message } => {
                if let pi_agent_core::Message::User { content, .. } = message {
                    self.push(sequence, format!("you: {content}"));
                }
            }
            AgentEventKind::MessageUpdate {
                message,
                text_delta,
            } => {
                if let (pi_agent_core::Message::Assistant { .. }, Some(delta)) =
                    (message, text_delta)
                {
                    if let Some(index) = self.streaming_line {
                        if let Some(line) = self.transcript.get_mut(index) {
                            line.text.push_str(delta);
                        }
                    } else {
                        self.push(sequence, format!("assistant: {delta}"));
                        self.streaming_line = self.transcript.len().checked_sub(1);
                    }
                }
            }
            AgentEventKind::MessageEnd { message } => {
                if let pi_agent_core::Message::Assistant { content, .. } = message {
                    if self.streaming_line.is_none() {
                        self.push(sequence, format!("assistant: {content}"));
                    }
                    self.streaming_line = None;
                }
            }
            AgentEventKind::ToolExecutionStart {
                tool_name,
                arguments,
                ..
            } => self.push(
                sequence,
                format!("tool {tool_name}: {}", arguments.as_str()),
            ),
            AgentEventKind::ToolExecutionUpdate {
                tool_name, update, ..
            } => {
                self.push(sequence, format!("tool {tool_name}: {}", update.content));
            }
            AgentEventKind::ToolExecutionEnd {
                tool_name, result, ..
            } => {
                let label = if result.is_error { "error" } else { "result" };
                self.push(
                    sequence,
                    format!("tool {tool_name} {label}: {}", result.content),
                );
            }
            AgentEventKind::ModelTurnUsage { accounting } => self.push(
                sequence,
                format!("cost: {}", format_usage(&accounting.usage)),
            ),
            AgentEventKind::CompactionStart {
                source_message_count,
            } => {
                self.status = UiStatus::Active;
                self.push(
                    sequence,
                    format!("compacting {source_message_count} messages"),
                );
            }
            AgentEventKind::CompactionResult {
                retained_message_count,
                usage,
            } => {
                let usage = usage
                    .as_ref()
                    .map(|usage| format!(" ({})", format_usage(usage)))
                    .unwrap_or_default();
                self.push(
                    sequence,
                    format!("compaction retained {retained_message_count} messages{usage}"),
                );
            }
            AgentEventKind::CompactionEnd { outcome } => match outcome {
                CompactionOutcome::Succeeded {
                    retained_message_count,
                } => self.push(
                    sequence,
                    format!("compaction complete: {retained_message_count} messages"),
                ),
                CompactionOutcome::Failed { message } => {
                    self.push(sequence, format!("compaction failed: {message}"));
                }
                CompactionOutcome::Cancelled => self.push(sequence, "compaction cancelled".into()),
            },
            AgentEventKind::AutomaticCompactionStart {
                source_message_count,
                reason,
                count,
                ..
            } => {
                self.status = UiStatus::Active;
                self.push(
                    sequence,
                    format!(
                        "automatic compaction #{count} ({reason:?}): {source_message_count} messages"
                    ),
                );
            }
            AgentEventKind::AutomaticCompactionEnd { outcome, .. } => match outcome {
                AutomaticCompactionOutcome::Succeeded { .. } => {
                    self.push(sequence, "automatic compaction complete".into())
                }
                AutomaticCompactionOutcome::Failed { message } => {
                    self.push(sequence, format!("automatic compaction failed: {message}"))
                }
                AutomaticCompactionOutcome::Cancelled => {
                    self.push(sequence, "automatic compaction cancelled".into())
                }
                AutomaticCompactionOutcome::LimitReached => {
                    self.push(sequence, "automatic compaction limit reached".into())
                }
                AutomaticCompactionOutcome::StillAboveThreshold => self.push(
                    sequence,
                    "automatic compaction complete; retained context remains above threshold"
                        .into(),
                ),
                AutomaticCompactionOutcome::Unavailable => {
                    self.push(sequence, "automatic compaction unavailable".into())
                }
            },
            AgentEventKind::ContextEstimate {
                estimated_context_tokens,
                message_count,
                ..
            } => self.push(
                sequence,
                format!(
                    "context estimate: {} tokens across {message_count} messages",
                    estimated_context_tokens
                        .map(|tokens| tokens.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ),
            ),
            AgentEventKind::ProviderRequestSkipped { reason } => self.push(
                sequence,
                match reason {
                    ProviderRequestSkipReason::AutomaticCompaction => {
                        "provider request deferred for automatic compaction".into()
                    }
                    ProviderRequestSkipReason::ToolCircuitBreaker => {
                        "provider request skipped after terminal tool failure".into()
                    }
                },
            ),
            AgentEventKind::ToolFailureObserved {
                disposition,
                consecutive_count,
                terminal,
                ..
            } => self.push(
                sequence,
                format!(
                    "tool failure {disposition:?} (consecutive {consecutive_count}){}",
                    if *terminal { "; ending run" } else { "" }
                ),
            ),
            AgentEventKind::AgentEnd { .. } => self.status = UiStatus::Idle,
            AgentEventKind::TurnStart { .. } | AgentEventKind::TurnEnd { .. } => {}
        }
    }

    /// Replace the displayed inspection snapshot.
    pub fn set_snapshot(&mut self, snapshot: AgentSnapshot) {
        self.selected_model = snapshot.model.clone();
        self.last_snapshot = Some(snapshot);
    }

    /// Borrow the event-derived transcript.
    pub fn transcript(&self) -> &[TranscriptLine] {
        &self.transcript
    }

    /// Borrow the local composer.
    pub fn composer(&self) -> &Composer {
        &self.composer
    }

    /// Mutably borrow the local composer.
    pub fn composer_mut(&mut self) -> &mut Composer {
        &mut self.composer
    }

    /// Borrow the presentation status.
    pub fn status(&self) -> &UiStatus {
        &self.status
    }

    /// Return the requested transcript top row for manual scrolling.
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// Whether output should continue to follow the newest event.
    pub fn follows_output(&self) -> bool {
        self.follow_output
    }

    /// Return the latest core snapshot, if one has been attached.
    pub fn snapshot(&self) -> Option<&AgentSnapshot> {
        self.last_snapshot.as_ref()
    }

    /// Return v0 picker lines for the renderer, if an overlay is active.
    pub fn picker_lines(&self, registry: &ProviderRegistry) -> Option<Vec<String>> {
        let picker = self.picker.as_ref()?;
        Some(match picker {
            Picker::Provider { filter, selected } => {
                let candidates = provider_candidates(registry, filter);
                let display = candidates
                    .iter()
                    .map(|provider| match missing_credential(provider) {
                        Some(reason) => format!("{provider} ({reason})"),
                        None => provider.clone(),
                    })
                    .collect::<Vec<_>>();
                overlay_lines("provider", filter, &display, *selected)
            }
            Picker::Model {
                provider,
                filter,
                selected,
            } => {
                let candidates = model_candidates(registry, provider, filter);
                overlay_lines("model", filter, &candidates, *selected)
            }
            Picker::CustomModel { provider, input } => vec![
                format!("custom model for {provider}"),
                format!("> {input}"),
                "Enter selects; Esc cancels".into(),
            ],
        })
    }

    pub(super) fn push(&mut self, sequence: Option<u64>, text: String) {
        self.transcript.push(TranscriptLine { sequence, text });
    }

    pub(super) fn notice(&mut self, text: impl Into<String>) {
        self.status = UiStatus::Notice(text.into());
    }

    pub(super) fn local_line(&mut self, text: impl Into<String>) {
        self.push(None, text.into());
    }

    pub(super) fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.streaming_line = None;
        self.viewport_offset = 0;
        self.follow_output = true;
    }

    pub(super) fn page_up(&mut self, lines: usize) {
        let current = if self.follow_output {
            self.visible_transcript_lines
                .saturating_sub(self.transcript_rows)
        } else {
            self.viewport_offset
        };
        self.follow_output = false;
        self.viewport_offset = current.saturating_sub(lines);
    }

    pub(super) fn page_down(&mut self, lines: usize) {
        self.viewport_offset = self.viewport_offset.saturating_add(lines);
        if self.viewport_offset
            >= self
                .visible_transcript_lines
                .saturating_sub(self.transcript_rows)
        {
            self.follow_output = true;
        }
    }

    pub(super) fn follow_end(&mut self) {
        self.follow_output = true;
        self.viewport_offset = self.transcript.len();
    }

    pub(super) fn set_viewport_metrics(
        &mut self,
        visible_transcript_lines: usize,
        transcript_rows: usize,
    ) {
        self.visible_transcript_lines = visible_transcript_lines;
        self.transcript_rows = transcript_rows;
    }
}

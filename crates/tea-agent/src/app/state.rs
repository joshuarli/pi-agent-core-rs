use crate::composer::Composer;
use tea_core::event::{
    AgentEventKind, AutomaticCompactionOutcome, CompactionOutcome, ProviderRequestSkipReason,
};
use tea_core::provider::ProviderRegistry;
use tea_core::state::{AgentMessage, AgentSnapshot, ToolCallId};
use tea_core::{Agent, AgentEvent, ModelDescriptor};
use std::collections::BTreeMap;
use std::num::NonZeroU64;

use super::host::{model_candidates, overlay_lines};
use super::session::SessionSummary;
use super::support::format_usage;

/// One display row derived from a core event, never a second source of state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptLine {
    /// Core event sequence, or `None` for a local command/help notice.
    pub sequence: Option<u64>,
    /// Raw, deliberately unrendered text for the v0 terminal projection.
    pub text: String,
    /// Semantic presentation class retained alongside the event text.
    pub kind: TranscriptKind,
}

/// Presentation classes used by the terminal renderer. Core semantics remain
/// in [`AgentEvent`]; this is only the host's stable visual projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranscriptKind {
    /// Startup identity and help hint.
    Welcome,
    /// A submitted user prompt.
    User,
    /// Incrementally streamed assistant text.
    Assistant,
    /// A generic tool lifecycle row.
    Tool {
        /// Model-visible tool name.
        name: String,
        /// Current lifecycle phase.
        state: ToolState,
    },
    /// Informational host/core notice.
    Notice,
    /// Error or cancellation notice.
    Error,
}

/// Generic tool lifecycle state for compact rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolState {
    /// Tool has been admitted and started.
    Started,
    /// Tool has emitted an update.
    Progress,
    /// Tool completed successfully.
    Completed,
    /// Tool completed with an error.
    Failed,
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
    Model {
        filter: String,
        selected: usize,
    },
    CustomModel {
        provider: String,
        input: String,
    },
    Session {
        filter: String,
        selected: usize,
        entries: Vec<SessionSummary>,
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
    /// Presentation projection of the selected model's installed compactor/policy; core remains
    /// the source of compaction truth.
    pub(super) automatic_compaction_enabled: bool,
    /// Effective context capacity selected by the host. This may be an explicit local override;
    /// the registry remains the fallback for catalog-backed models.
    pub(super) selected_context_window: Option<NonZeroU64>,
    pub(super) picker: Option<Picker>,
    pub(super) streaming_line: Option<usize>,
    /// Active generic tool rows keyed by the core-owned call identity.
    pub(super) active_tool_lines: BTreeMap<ToolCallId, usize>,
    /// The most recent core-emitted context estimate. `None` means the core has not supplied
    /// capacity-policy evidence for this projection; it is never inferred from rendered text.
    pub(super) context_estimate: Option<ContextEstimate>,
    /// Read-only projection of the two queues the core owns and drains.
    pub(super) queued_prompts: Vec<QueuedPrompt>,
    /// In-memory prompt history for the current terminal invocation.
    pub(super) history: Vec<String>,
    /// Current history cursor; `None` means the live composer draft.
    pub(super) history_index: Option<usize>,
    /// Draft saved when history navigation first leaves the live composer.
    pub(super) history_draft: Option<String>,
    /// Whether multiline tool output and diff bodies are expanded.
    pub(super) tool_output_expanded: bool,
}

/// Context-policy information carried by the core event stream for footer projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ContextEstimate {
    pub(super) tokens: Option<u64>,
    pub(super) message_count: usize,
}

/// A prompt awaiting a named core queue boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct QueuedPrompt {
    pub(super) sequence: u64,
    pub(super) content: String,
    pub(super) delivery: QueueDelivery,
    pub(super) mode: tea_core::queue::QueueMode,
}

/// The core queue whose boundary will place the prompt in the transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QueueDelivery {
    Steering,
    FollowUp,
}

impl AppState {
    /// Create an empty projection.
    pub fn new() -> Self {
        Self {
            follow_output: true,
            tool_output_expanded: true,
            ..Self::default()
        }
    }

    pub(super) fn toggle_tool_output(&mut self) {
        self.tool_output_expanded = !self.tool_output_expanded;
        self.notice(if self.tool_output_expanded {
            "tool output expanded"
        } else {
            "tool output collapsed"
        });
    }

    /// Apply one typed core event after its reducer has committed state.
    pub fn apply_event(&mut self, event: &AgentEvent) {
        let sequence = Some(event.sequence.0);
        match &event.kind {
            AgentEventKind::AgentStart => self.status = UiStatus::Active,
            AgentEventKind::MessageStart { message } => {
                if let tea_core::Message::User { content, .. } = message {
                    self.push_kind(sequence, format!("you: {content}"), TranscriptKind::User);
                }
            }
            AgentEventKind::MessageUpdate {
                message,
                text_delta,
            } => {
                if let (tea_core::Message::Assistant { .. }, Some(delta)) =
                    (message, text_delta)
                {
                    if let Some(index) = self.streaming_line {
                        if let Some(line) = self.transcript.get_mut(index) {
                            line.text.push_str(delta);
                        }
                    } else {
                        self.push_kind(
                            sequence,
                            format!("assistant: {delta}"),
                            TranscriptKind::Assistant,
                        );
                        self.streaming_line = self.transcript.len().checked_sub(1);
                    }
                }
            }
            AgentEventKind::MessageEnd { message } => {
                if let tea_core::Message::Assistant {
                    content,
                    error_message,
                    ..
                } = message
                {
                    if self.streaming_line.is_none() {
                        if let Some(error) = error_message {
                            self.push_kind(
                                sequence,
                                format!("assistant error: {error}"),
                                TranscriptKind::Error,
                            );
                        } else {
                            self.push_kind(
                                sequence,
                                format!("assistant: {content}"),
                                TranscriptKind::Assistant,
                            );
                        }
                    }
                    self.streaming_line = None;
                }
            }
            AgentEventKind::ToolExecutionStart {
                tool_call_id,
                tool_name,
                arguments,
            } => {
                self.push_kind(
                    sequence,
                    format!("tool {tool_name} — started: {}", arguments.as_str()),
                    TranscriptKind::Tool {
                        name: tool_name.clone(),
                        state: ToolState::Started,
                    },
                );
                let index = self.transcript.len().saturating_sub(1);
                self.active_tool_lines.insert(tool_call_id.clone(), index);
            }
            AgentEventKind::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                update,
            } => {
                self.update_tool_line(
                    tool_call_id,
                    sequence,
                    format!("tool {tool_name} — progress: {}", update.content),
                    ToolState::Progress,
                    tool_name,
                );
            }
            AgentEventKind::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                ..
            } => {
                let label = if result.is_error {
                    "failed"
                } else {
                    "completed"
                };
                self.update_tool_line(
                    tool_call_id,
                    sequence,
                    format!("tool {tool_name} — {label}: {}", result.content),
                    if result.is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Completed
                    },
                    tool_name,
                );
                self.active_tool_lines.remove(tool_call_id);
            }
            AgentEventKind::ModelTurnUsage { accounting } => self.push_kind(
                sequence,
                format!("cost: {}", format_usage(&accounting.usage)),
                TranscriptKind::Notice,
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
            } => {
                self.context_estimate = Some(ContextEstimate {
                    tokens: *estimated_context_tokens,
                    message_count: *message_count,
                });
            }
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
            AgentEventKind::TurnEnd { reason, .. } => match reason {
                tea_core::state::StopReason::Error => self.push(
                    sequence,
                    "turn failed; prompt remains available to retry".into(),
                ),
                tea_core::state::StopReason::Aborted => {
                    self.push(sequence, "turn aborted".into())
                }
                tea_core::state::StopReason::Cancelled => {
                    self.push(sequence, "turn cancelled".into())
                }
                _ => {}
            },
            AgentEventKind::AgentEnd { .. } => self.status = UiStatus::Idle,
            AgentEventKind::TurnStart { .. } => {}
        }
    }

    /// Replace the displayed inspection snapshot.
    pub fn set_snapshot(&mut self, snapshot: AgentSnapshot) {
        self.selected_model = snapshot.model.clone();
        self.last_snapshot = Some(snapshot);
    }

    /// Rebuild the visible transcript from a restored canonical conversation.
    ///
    /// These rows deliberately have no event sequence: loading a session is a host projection,
    /// not a replay of historical core events. Future events continue from the live subscription.
    pub(super) fn restore_messages(&mut self, messages: &[AgentMessage]) {
        self.clear_transcript();
        for message in messages {
            match message {
                AgentMessage::User { content, .. } => {
                    self.push_kind(None, format!("you: {content}"), TranscriptKind::User);
                }
                AgentMessage::Assistant {
                    content,
                    error_message,
                    ..
                } => {
                    if let Some(error) = error_message {
                        self.push_kind(
                            None,
                            format!("assistant error: {error}"),
                            TranscriptKind::Error,
                        );
                    } else if !content.is_empty() {
                        self.push_kind(
                            None,
                            format!("assistant: {content}"),
                            TranscriptKind::Assistant,
                        );
                    }
                }
                AgentMessage::ToolResult {
                    tool_call_id: _,
                    tool_name,
                    content,
                    is_error,
                    ..
                } => {
                    let state = if *is_error {
                        ToolState::Failed
                    } else {
                        ToolState::Completed
                    };
                    let label = if *is_error { "failed" } else { "completed" };
                    self.push_kind(
                        None,
                        format!("tool {tool_name} — {label}: {content}"),
                        TranscriptKind::Tool {
                            name: tool_name.clone(),
                            state,
                        },
                    );
                }
            }
        }
    }

    /// Refresh the visible queue projection from the agent's owned inspection boundary.
    pub(super) fn set_queue_snapshot(&mut self, agent: &Agent) {
        let queues = agent.queue_snapshot();
        let steering_mode = agent.steering_mode();
        let follow_up_mode = agent.follow_up_mode();
        self.queued_prompts = queues
            .steering
            .snapshot()
            .into_iter()
            .map(|entry| QueuedPrompt {
                sequence: entry.sequence,
                content: entry.content,
                delivery: QueueDelivery::Steering,
                mode: steering_mode,
            })
            .chain(
                queues
                    .follow_up
                    .snapshot()
                    .into_iter()
                    .map(|entry| QueuedPrompt {
                        sequence: entry.sequence,
                        content: entry.content,
                        delivery: QueueDelivery::FollowUp,
                        mode: follow_up_mode,
                    }),
            )
            .collect();
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

    pub(crate) fn tool_output_expanded(&self) -> bool {
        self.tool_output_expanded
    }

    /// Whether the transcript row is still receiving assistant deltas.
    pub(crate) fn is_streaming_transcript(&self, index: usize) -> bool {
        self.streaming_line == Some(index)
    }

    /// Return the latest core snapshot, if one has been attached.
    pub fn snapshot(&self) -> Option<&AgentSnapshot> {
        self.last_snapshot.as_ref()
    }

    /// Return the compact, event-derived telemetry lines for the fixed footer.
    pub(crate) fn footer_lines(&self, registry: &ProviderRegistry) -> [String; 2] {
        let selected = self.selected_model.as_ref();
        let model = selected
            .map(|model| compact_model_label(&model.model))
            .unwrap_or_else(|| "provider/model unknown".into());
        let hint = if self.composer.text().starts_with('/') {
            "commands: /help · /model · /cost · /compact · /reload-extensions · /clear · /quit"
                .into()
        } else {
            match &self.status {
                UiStatus::Idle => format!("yolo · {model}"),
                UiStatus::Active => format!("⏺ Asking · yolo · {model}"),
                UiStatus::Notice(ref notice) => format!("yolo · {model} · {notice}"),
            }
        };
        let capacity = self
            .selected_context_window
            .map(NonZeroU64::get)
            .or_else(|| {
                selected
                    .and_then(|model| registry.provider(&model.provider)?.model(&model.model))
                    .and_then(|model| model.context_window)
            });
        let compaction = if self.automatic_compaction_enabled {
            "automatic compaction available"
        } else {
            "automatic compaction unavailable"
        };
        let context = match &self.context_estimate {
            Some(estimate) => format!(
                "context {}% used ({}/{}; {} messages); {compaction}",
                format_context_percent(estimate.tokens, capacity),
                estimate
                    .tokens
                    .map(|tokens| tokens.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                capacity
                    .map(|tokens| tokens.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                estimate.message_count
            ),
            None => format!(
                "context unknown% used (unknown/{}); {compaction}",
                capacity
                    .map(|tokens| tokens.to_string())
                    .unwrap_or_else(|| "unknown".into())
            ),
        };
        let telemetry = self
            .last_snapshot
            .as_ref()
            .map(|snapshot| &snapshot.accounting.aggregate)
            .filter(|usage| {
                usage.input_tokens.is_some()
                    || usage.output_tokens.is_some()
                    || usage.reasoning_tokens.is_some()
                    || usage.cache_read_tokens.is_some()
                    || usage.cache_write_tokens.is_some()
                    || usage.cost.is_some()
            })
            .map(super::support::format_footer_usage);
        let context = telemetry
            .map(|usage| format!("{context}; {usage}"))
            .unwrap_or(context);
        [hint, context]
    }

    pub(crate) fn queued_lines(&self) -> Vec<String> {
        self.queued_prompts
            .iter()
            .map(|prompt| {
                let (delivery, boundary) = match prompt.delivery {
                    QueueDelivery::Steering => ("steering", "next active turn"),
                    QueueDelivery::FollowUp => ("follow-up", "next idle boundary"),
                };
                let mode = match prompt.mode {
                    tea_core::queue::QueueMode::OneAtATime => "one at a time",
                    tea_core::queue::QueueMode::All => "all queued prompts",
                };
                format!(
                    "queued {delivery} #{} ({boundary}; {mode}): {}",
                    prompt.sequence, prompt.content
                )
            })
            .collect()
    }

    /// Return v0 picker lines for the renderer, if an overlay is active.
    pub fn picker_lines(&self, registry: &ProviderRegistry) -> Option<Vec<String>> {
        self.picker_lines_visible(registry, usize::MAX)
    }

    pub(crate) fn picker_lines_visible(
        &self,
        registry: &ProviderRegistry,
        max_rows: usize,
    ) -> Option<Vec<String>> {
        let picker = self.picker.as_ref()?;
        Some(match picker {
            Picker::Model { filter, selected } => {
                let candidates = model_candidates(registry, filter);
                let display = candidates
                    .iter()
                    .copied()
                    .map(|candidate| candidate.label())
                    .collect::<Vec<_>>();
                overlay_lines("Models", filter, &display, *selected, max_rows)
            }
            Picker::CustomModel { provider, input } => vec![
                format!("custom model for {provider}"),
                format!("> {input}"),
                "Enter selects; Esc cancels".into(),
            ],
            Picker::Session {
                filter,
                selected,
                entries,
            } => {
                let filter_lower = filter.to_ascii_lowercase();
                let rows = entries
                    .iter()
                    .filter(|entry| {
                        let model = entry
                            .model
                            .as_ref()
                            .map(|model| format!("{} {}", model.provider, model.model))
                            .unwrap_or_default();
                        format!("{} {model}", entry.id)
                            .to_ascii_lowercase()
                            .contains(&filter_lower)
                    })
                    .map(|entry| {
                        let model = entry
                            .model
                            .as_ref()
                            .map(|model| format!("{}/{}", model.provider, model.model))
                            .unwrap_or_else(|| "unknown model".into());
                        format!("{} · {model} · {} messages", entry.id, entry.message_count)
                    })
                    .collect::<Vec<_>>();
                overlay_lines("Sessions", filter, &rows, *selected, max_rows)
            }
        })
    }

    pub(super) fn push(&mut self, sequence: Option<u64>, text: String) {
        self.push_kind(sequence, text, TranscriptKind::Notice);
    }

    fn push_kind(&mut self, sequence: Option<u64>, text: String, kind: TranscriptKind) {
        self.transcript.push(TranscriptLine {
            sequence,
            text,
            kind,
        });
    }

    fn update_tool_line(
        &mut self,
        tool_call_id: &ToolCallId,
        sequence: Option<u64>,
        text: String,
        state: ToolState,
        tool_name: &str,
    ) {
        if let Some(index) = self.active_tool_lines.get(tool_call_id).copied() {
            if let Some(line) = self.transcript.get_mut(index) {
                line.sequence = sequence;
                line.text = text;
                line.kind = TranscriptKind::Tool {
                    name: tool_name.to_owned(),
                    state,
                };
                return;
            }
        }
        self.push_kind(
            sequence,
            text,
            TranscriptKind::Tool {
                name: tool_name.to_owned(),
                state,
            },
        );
        let index = self.transcript.len().saturating_sub(1);
        self.active_tool_lines.insert(tool_call_id.clone(), index);
    }

    pub(super) fn notice(&mut self, text: impl Into<String>) {
        self.status = UiStatus::Notice(text.into());
    }

    pub(super) fn local_line(&mut self, text: impl Into<String>) {
        self.push(None, text.into());
    }

    pub(super) fn welcome_line(&mut self) {
        self.push_kind(
            None,
            format!(
                "𝒑i-agent v{} · Run /help for commands",
                env!("CARGO_PKG_VERSION")
            ),
            TranscriptKind::Welcome,
        );
    }

    pub(super) fn record_history(&mut self, prompt: &str) {
        if prompt.trim().is_empty() {
            return;
        }
        if self
            .history
            .last()
            .is_none_or(|previous| previous != prompt)
        {
            self.history.push(prompt.to_owned());
        }
        self.history_index = None;
        self.history_draft = None;
    }

    pub(super) fn begin_history_navigation(&mut self) {
        if self.history_index.is_none() {
            self.history_draft = Some(self.composer.text().to_owned());
        }
    }

    pub(super) fn history_previous(&mut self) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        let index = self
            .history_index
            .unwrap_or(self.history.len())
            .saturating_sub(1);
        self.history_index = Some(index);
        self.history.get(index).cloned()
    }

    pub(super) fn history_next(&mut self) -> Option<String> {
        let Some(index) = self.history_index else {
            return None;
        };
        let next = index + 1;
        if next >= self.history.len() {
            self.history_index = None;
            return Some(self.history_draft.take().unwrap_or_default());
        }
        self.history_index = Some(next);
        self.history.get(next).cloned()
    }

    pub(super) fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.streaming_line = None;
        self.active_tool_lines.clear();
        self.viewport_offset = 0;
        self.follow_output = true;
    }

    pub(super) fn clear_history(&mut self) {
        self.history.clear();
        self.history_index = None;
        self.history_draft = None;
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

fn format_context_percent(tokens: Option<u64>, capacity: Option<u64>) -> String {
    match (tokens, capacity.filter(|capacity| *capacity != 0)) {
        (Some(tokens), Some(capacity)) => {
            let percent = u128::from(tokens).saturating_mul(100) / u128::from(capacity);
            percent.to_string()
        }
        _ => "unknown".into(),
    }
}

fn compact_model_label(model: &str) -> String {
    let bare = model.rsplit('/').next().unwrap_or(model);
    bare.strip_prefix("claude-").map_or_else(
        || bare.to_owned(),
        |name| {
            name.replace("opus-", "opus ")
                .replace("sonnet-", "sonnet ")
                .replace("haiku-", "haiku ")
        },
    )
}

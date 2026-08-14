//! Application assembly, event projection, and direct keyboard dispatch.

use crate::composer::Composer;
use crate::editor::EditorError;
use crate::grid::Grid;
use crate::render;
use crate::terminal::TerminalError;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use pi_agent_core::compaction::CompactionHandle;
use pi_agent_core::event::{AgentEventKind, CompactionOutcome};
use pi_agent_core::provider::{
    openai::OpenAiContextHook, ConfiguredProvider, ProviderConfiguration, ProviderRegistry,
    RegistryError,
};
use pi_agent_core::state::AgentPhase;
use pi_agent_core::{
    Agent, AgentEvent, AgentSnapshot, CoreError, DefaultCodingTools, LosslessEventSubscription,
    ModelDescriptor, RunHandle, Usage,
};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::sync::{
    mpsc::{sync_channel, Receiver, TryRecvError},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Explicit command-line inputs accepted by the v0 terminal host.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CliOptions {
    provider: Option<OsString>,
    model: Option<OsString>,
    cwd: Option<PathBuf>,
}

impl CliOptions {
    /// Parse `pi-agent [--provider <id>] [--model <id>] [--cwd <path>]`.
    pub fn parse<I>(args: I) -> Result<Self, CliError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut arguments = args.into_iter();
        let _program = arguments.next();
        let mut options = Self::default();
        while let Some(argument) = arguments.next() {
            let slot = match argument.to_string_lossy().as_ref() {
                "--provider" => OptionSlot::Provider,
                "--model" => OptionSlot::Model,
                "--cwd" => OptionSlot::Cwd,
                _ if argument.as_os_str().to_string_lossy().starts_with('-') => {
                    return Err(CliError::UnknownOption(argument));
                }
                _ => return Err(CliError::UnexpectedArgument(argument)),
            };
            let value = arguments
                .next()
                .ok_or_else(|| CliError::MissingValue(slot.name()))?;
            if value.is_empty() {
                return Err(CliError::EmptyValue(slot.name()));
            }
            options.set(slot, value)?;
        }
        Ok(options)
    }

    /// Borrow the explicitly selected provider, if supplied.
    pub fn provider(&self) -> Option<&OsStr> {
        self.provider.as_deref()
    }

    /// Borrow the explicitly selected model, if supplied.
    pub fn model(&self) -> Option<&OsStr> {
        self.model.as_deref()
    }

    /// Borrow the explicit workspace authority, if supplied.
    pub fn cwd(&self) -> Option<&std::path::Path> {
        self.cwd.as_deref()
    }

    fn set(&mut self, slot: OptionSlot, value: OsString) -> Result<(), CliError> {
        let destination = match slot {
            OptionSlot::Provider => &mut self.provider,
            OptionSlot::Model => &mut self.model,
            OptionSlot::Cwd => {
                if self.cwd.replace(PathBuf::from(value)).is_some() {
                    return Err(CliError::DuplicateOption(slot.name()));
                }
                return Ok(());
            }
        };
        if destination.replace(value).is_some() {
            Err(CliError::DuplicateOption(slot.name()))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OptionSlot {
    Provider,
    Model,
    Cwd,
}

impl OptionSlot {
    const fn name(self) -> &'static str {
        match self {
            Self::Provider => "--provider",
            Self::Model => "--model",
            Self::Cwd => "--cwd",
        }
    }
}

/// Errors produced by direct command-line parsing.
#[derive(Debug, Eq, PartialEq)]
pub enum CliError {
    /// An option had no following value.
    MissingValue(&'static str),
    /// An option was supplied more than once.
    DuplicateOption(&'static str),
    /// An option was supplied with an empty value.
    EmptyValue(&'static str),
    /// The option is not part of v0.
    UnknownOption(OsString),
    /// Positional arguments are not part of v0.
    UnexpectedArgument(OsString),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(flag) => write!(formatter, "missing value for {flag}"),
            Self::DuplicateOption(flag) => write!(formatter, "duplicate option {flag}"),
            Self::EmptyValue(flag) => write!(formatter, "empty value for {flag}"),
            Self::UnknownOption(option) => write!(formatter, "unknown option {option:?}"),
            Self::UnexpectedArgument(argument) => {
                write!(formatter, "unexpected argument {argument:?}")
            }
        }
    }
}

impl std::error::Error for CliError {}

/// Local application failures. Provider and core failures retain their typed source.
#[derive(Debug)]
pub enum AppError {
    /// Command-line parsing failed.
    Cli(CliError),
    /// Terminal setup, input, output, or restoration failed.
    Terminal(TerminalError),
    /// `$EDITOR` integration failed before it could replace the composer.
    Editor(EditorError),
    /// The explicit workspace or startup selection was invalid.
    Setup(String),
    /// Registry model resolution or adapter construction failed.
    Registry(RegistryError),
    /// A core state-machine operation failed.
    Core(CoreError),
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cli(error) => error.fmt(formatter),
            Self::Terminal(error) => error.fmt(formatter),
            Self::Editor(error) => error.fmt(formatter),
            Self::Setup(message) => formatter.write_str(message),
            Self::Registry(error) => error.fmt(formatter),
            Self::Core(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cli(error) => Some(error),
            Self::Terminal(error) => Some(error),
            Self::Editor(error) => Some(error),
            Self::Registry(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Setup(_) => None,
        }
    }
}

impl From<CliError> for AppError {
    fn from(error: CliError) -> Self {
        Self::Cli(error)
    }
}

impl From<TerminalError> for AppError {
    fn from(error: TerminalError) -> Self {
        Self::Terminal(error)
    }
}

impl From<EditorError> for AppError {
    fn from(error: EditorError) -> Self {
        Self::Editor(error)
    }
}

impl From<RegistryError> for AppError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<CoreError> for AppError {
    fn from(error: CoreError) -> Self {
        Self::Core(error)
    }
}

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
enum Picker {
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
    transcript: Vec<TranscriptLine>,
    composer: Composer,
    status: UiStatus,
    viewport_offset: usize,
    follow_output: bool,
    visible_transcript_lines: usize,
    transcript_rows: usize,
    last_snapshot: Option<AgentSnapshot>,
    selected_model: Option<ModelDescriptor>,
    picker: Option<Picker>,
    streaming_line: Option<usize>,
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

    fn push(&mut self, sequence: Option<u64>, text: String) {
        self.transcript.push(TranscriptLine { sequence, text });
    }

    fn notice(&mut self, text: impl Into<String>) {
        self.status = UiStatus::Notice(text.into());
    }

    fn local_line(&mut self, text: impl Into<String>) {
        self.push(None, text.into());
    }

    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.streaming_line = None;
        self.viewport_offset = 0;
        self.follow_output = true;
    }

    fn page_up(&mut self, lines: usize) {
        let current = if self.follow_output {
            self.visible_transcript_lines
                .saturating_sub(self.transcript_rows)
        } else {
            self.viewport_offset
        };
        self.follow_output = false;
        self.viewport_offset = current.saturating_sub(lines);
    }

    fn page_down(&mut self, lines: usize) {
        self.viewport_offset = self.viewport_offset.saturating_add(lines);
        if self.viewport_offset
            >= self
                .visible_transcript_lines
                .saturating_sub(self.transcript_rows)
        {
            self.follow_output = true;
        }
    }

    fn follow_end(&mut self) {
        self.follow_output = true;
        self.viewport_offset = self.transcript.len();
    }

    fn set_viewport_metrics(&mut self, visible_transcript_lines: usize, transcript_rows: usize) {
        self.visible_transcript_lines = visible_transcript_lines;
        self.transcript_rows = transcript_rows;
    }
}

/// Assembled v0 terminal application.
#[derive(Debug)]
pub struct App {
    options: CliOptions,
    state: AppState,
    core: Option<Agent>,
    registry: ProviderRegistry,
    workspace: Option<PathBuf>,
    subscription: Option<LosslessEventSubscription>,
    active_task: Option<Receiver<Result<(), CoreError>>>,
    previous_grid: Option<Grid>,
    quitting: bool,
}

impl App {
    /// Assemble an application from explicit command-line values.
    pub fn new(options: CliOptions) -> Self {
        Self {
            options,
            state: AppState::new(),
            core: None,
            registry: ProviderRegistry::new(),
            workspace: None,
            subscription: None,
            active_task: None,
            previous_grid: None,
            quitting: false,
        }
    }

    /// Initialize the core boundary and run the terminal loop on Smol.
    pub fn run(&mut self) -> Result<(), AppError> {
        self.assemble_agent()?;
        let mut terminal = crate::terminal::TerminalGuard::enter()?;
        smol::block_on(self.event_loop(&mut terminal))
    }

    /// Borrow startup options.
    pub fn options(&self) -> &CliOptions {
        &self.options
    }

    /// Borrow presentation-only state.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Mutably borrow presentation-only state.
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// Attach an explicitly configured agent for non-terminal integration tests.
    pub fn attach_agent(&mut self, agent: Agent) {
        self.state.set_snapshot(agent.snapshot());
        self.subscription = Some(agent.subscribe_lossless());
        self.core = Some(agent);
    }

    /// Borrow the attached core agent, if one exists.
    pub fn agent(&self) -> Option<&Agent> {
        self.core.as_ref()
    }

    fn assemble_agent(&mut self) -> Result<(), AppError> {
        if self.core.is_some() {
            return Ok(());
        }
        let workspace = match self.options.cwd() {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir().map_err(|error| {
                AppError::Setup(format!("cannot read current directory: {error}"))
            })?,
        };
        let tools = DefaultCodingTools::new(&workspace)
            .map_err(|error| AppError::Setup(format!("invalid --cwd: {error}")))?;
        self.workspace = Some(tools.workspace().as_path().to_path_buf());
        let builder = build_host_agent(tools)?;
        self.attach_agent(builder.build());

        match (self.options.provider(), self.options.model()) {
            (None, None) => self.open_provider_picker(),
            (Some(provider), None) => self.open_model_picker(os_text(provider, "--provider")?)?,
            (Some(provider), Some(model)) => {
                self.select_model(os_text(provider, "--provider")?, os_text(model, "--model")?)?
            }
            (None, Some(_)) => {
                return Err(AppError::Setup(
                    "--model requires an explicit --provider".into(),
                ));
            }
        }
        Ok(())
    }

    async fn event_loop(
        &mut self,
        terminal: &mut crate::terminal::TerminalGuard,
    ) -> Result<(), AppError> {
        loop {
            self.drain_events();
            self.reap_task();
            if self.quitting && self.active_task.is_none() {
                break;
            }
            self.redraw(terminal)?;
            if let Some(event) = terminal.poll_event(Duration::from_millis(20))? {
                self.handle_terminal_event(terminal, event)?;
            }
            // Crossterm input is synchronous by design. Yield after each poll
            // so the caller-owned Smol executor drives model/tool work.
            smol::future::yield_now().await;
        }
        Ok(())
    }

    fn drain_events(&mut self) {
        let Some(subscription) = self.subscription.as_ref() else {
            return;
        };
        loop {
            match subscription.try_recv() {
                Ok(event) => self.state.apply_event(&event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if let Some(agent) = &self.core {
            self.state.set_snapshot(agent.snapshot());
        }
    }

    fn reap_task(&mut self) {
        let Some(receiver) = self.active_task.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Ok(())) => {
                self.active_task = None;
                self.state.status = UiStatus::Idle;
            }
            Ok(Err(CoreError::Cancelled)) => {
                self.active_task = None;
                self.state.notice("cancelled");
            }
            Ok(Err(error)) => {
                self.active_task = None;
                self.state.notice(error.to_string());
            }
            Err(TryRecvError::Disconnected) => {
                self.active_task = None;
                self.state.notice("operation task ended unexpectedly");
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn redraw(&mut self, terminal: &mut crate::terminal::TerminalGuard) -> Result<(), AppError> {
        let (width, height) = terminal.size()?;
        let (visible_lines, transcript_rows) =
            render::transcript_metrics(&self.state, width, height);
        self.state
            .set_viewport_metrics(visible_lines, transcript_rows);
        let current = render::render(&self.state, &self.registry, width, height);
        let diff = current.diff(self.previous_grid.as_ref());
        let cursor = composer_cursor(&self.state, width, height);
        terminal.draw(&diff, cursor)?;
        self.previous_grid = Some(current);
        Ok(())
    }

    fn handle_terminal_event(
        &mut self,
        terminal: &mut crate::terminal::TerminalGuard,
        event: Event,
    ) -> Result<(), AppError> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(terminal, key),
            Event::Paste(text) if self.state.picker.is_none() => {
                if let Err(error) = self.state.composer_mut().insert_str(&text) {
                    self.state.notice(error.to_string());
                }
                Ok(())
            }
            Event::Paste(text) => self.picker_insert(&text),
            Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => Ok(()),
            _ => Ok(()),
        }
    }

    fn handle_key(
        &mut self,
        terminal: &mut crate::terminal::TerminalGuard,
        key: KeyEvent,
    ) -> Result<(), AppError> {
        if self.state.picker.is_some() {
            return self.handle_picker_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') {
            let current = self.state.composer().text().to_owned();
            match crate::editor::Editor::open(terminal, &current) {
                Ok(replacement) => {
                    self.state.composer_mut().replace_from_editor(replacement);
                    self.previous_grid = None;
                }
                Err(error) => self.state.notice(error.to_string()),
            }
            return Ok(());
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.handle_control_c();
            return Ok(());
        }
        match key.code {
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if let Err(error) = self.state.composer_mut().insert(character) {
                    self.state.notice(error.to_string());
                }
            }
            KeyCode::Backspace => self.state.composer_mut().backspace(),
            KeyCode::Delete => self.state.composer_mut().delete(),
            KeyCode::Left => self.state.composer_mut().move_left(),
            KeyCode::Right => self.state.composer_mut().move_right(),
            KeyCode::Home => self.state.composer_mut().home(),
            KeyCode::End => self.state.follow_end(),
            KeyCode::PageUp => self.state.page_up(5),
            KeyCode::PageDown => self.state.page_down(5),
            KeyCode::Enter => self.submit_composer()?,
            _ => {}
        }
        Ok(())
    }

    fn handle_control_c(&mut self) {
        let Some(agent) = self.core.as_ref() else {
            return;
        };
        if !matches!(agent.snapshot().phase, AgentPhase::Idle) {
            agent.abort();
            self.state.notice("cancelling");
        } else if self.state.composer().text().is_empty() {
            self.quitting = true;
        } else {
            self.state.composer_mut().clear();
        }
    }

    fn submit_composer(&mut self) -> Result<(), AppError> {
        let input = self.state.composer_mut().take();
        if input.trim().is_empty() {
            return Ok(());
        }
        if input.starts_with('/') {
            self.dispatch_command(&input)
        } else {
            let agent = self.agent_or_setup()?;
            match agent.snapshot().phase {
                AgentPhase::Idle if !agent.has_model_provider() => {
                    self.state.notice("select a provider and model first");
                    self.open_provider_picker();
                }
                AgentPhase::Idle => self.spawn_run(agent.start_prompt(input)?),
                AgentPhase::Running(_) | AgentPhase::Cancelling(_) => {
                    agent.enqueue_steering(input)?;
                    self.state.notice("steering queued");
                }
            }
            Ok(())
        }
    }

    fn dispatch_command(&mut self, input: &str) -> Result<(), AppError> {
        let mut words = input.split_whitespace();
        match words.next().unwrap_or_default() {
            "/help" => {
                self.state.local_line(
                    "keys: Enter submit, Ctrl+C cancel/clear/quit, Ctrl+G $EDITOR, PgUp/PgDn/End scroll; commands: /provider /model /cost /compact /clear /quit",
                );
            }
            "/provider" => self.open_provider_picker(),
            "/model" => {
                if let (Some(provider), Some(model)) = (words.next(), words.next()) {
                    self.select_model(provider.to_owned(), model.to_owned())?;
                } else if let Some(provider) = self.selected_provider() {
                    self.open_model_picker(provider)?;
                } else {
                    self.open_provider_picker();
                }
            }
            "/cost" => self.show_cost(),
            "/compact" => {
                let agent = self.agent_or_setup()?;
                match agent.start_compaction() {
                    Ok(compaction) => self.spawn_compaction(compaction),
                    Err(CoreError::MissingCompactor) => self
                        .state
                        .notice("manual compaction is unavailable for this provider/model"),
                    Err(error) => self.state.notice(error.to_string()),
                }
            }
            "/clear" => {
                let agent = self.agent_or_setup()?;
                match agent.reset() {
                    Ok(()) => {
                        let snapshot = agent.snapshot();
                        self.state.clear_transcript();
                        self.state.set_snapshot(snapshot);
                        self.state.notice("cleared");
                    }
                    Err(CoreError::ActiveRun { .. }) => {
                        self.state.notice("cannot clear while the agent is active");
                    }
                    Err(error) => self.state.notice(error.to_string()),
                }
            }
            "/quit" => {
                self.quitting = true;
                if let Some(agent) = self.core.as_ref() {
                    agent.abort();
                }
            }
            command => self.state.notice(format!("unknown command {command}")),
        }
        Ok(())
    }

    fn show_cost(&mut self) {
        let Some(snapshot) = self.state.snapshot().cloned() else {
            return;
        };
        if snapshot.accounting.turns.is_empty() {
            self.state
                .local_line("cost: no provider-reported accounting yet");
            return;
        }
        for turn in &snapshot.accounting.turns {
            let model = turn
                .model
                .as_ref()
                .map(|model| format!("{}/{}", model.provider, model.model))
                .unwrap_or_else(|| "unknown model".into());
            self.state.local_line(format!(
                "cost run {} turn {} {model}: {}",
                turn.run_id.0,
                turn.turn_id.0,
                format_usage(&turn.usage)
            ));
        }
        self.state.local_line(format!(
            "cost total: {}",
            format_usage(&snapshot.accounting.aggregate)
        ));
    }

    fn open_provider_picker(&mut self) {
        if self.agent_is_active() {
            self.state.notice("provider changes require an idle agent");
            return;
        }
        self.state.picker = Some(Picker::Provider {
            filter: String::new(),
            selected: 0,
        });
    }

    fn open_model_picker(&mut self, provider: String) -> Result<(), AppError> {
        if self.agent_is_active() {
            self.state.notice("model changes require an idle agent");
            return Ok(());
        }
        if self.registry.provider(&provider).is_none() {
            return Err(AppError::Setup(format!(
                "provider {provider:?} is not compiled in"
            )));
        }
        self.state.picker = Some(Picker::Model {
            provider,
            filter: String::new(),
            selected: 0,
        });
        Ok(())
    }

    fn handle_picker_key(&mut self, key: KeyEvent) -> Result<(), AppError> {
        match key.code {
            KeyCode::Esc => self.state.picker = None,
            KeyCode::Up => self.picker_move(-1),
            KeyCode::Down => self.picker_move(1),
            KeyCode::Backspace => self.picker_backspace(),
            KeyCode::Enter => self.commit_picker()?,
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.picker_insert(&character.to_string())?
            }
            _ => {}
        }
        Ok(())
    }

    fn picker_insert(&mut self, text: &str) -> Result<(), AppError> {
        let Some(picker) = self.state.picker.as_mut() else {
            return Ok(());
        };
        match picker {
            Picker::Provider { filter, selected }
            | Picker::Model {
                filter, selected, ..
            } => {
                filter.push_str(text);
                *selected = 0;
            }
            Picker::CustomModel { input, .. } => input.push_str(text),
        }
        Ok(())
    }

    fn picker_backspace(&mut self) {
        let Some(picker) = self.state.picker.as_mut() else {
            return;
        };
        match picker {
            Picker::Provider { filter, selected }
            | Picker::Model {
                filter, selected, ..
            } => {
                filter.pop();
                *selected = 0;
            }
            Picker::CustomModel { input, .. } => {
                input.pop();
            }
        }
    }

    fn picker_move(&mut self, delta: isize) {
        let Some(picker) = self.state.picker.as_mut() else {
            return;
        };
        let length = match picker {
            Picker::Provider { filter, .. } => provider_candidates(&self.registry, filter).len(),
            Picker::Model {
                provider, filter, ..
            } => model_candidates(&self.registry, provider, filter).len(),
            Picker::CustomModel { .. } => return,
        };
        let selected = match picker {
            Picker::Provider { selected, .. } | Picker::Model { selected, .. } => selected,
            Picker::CustomModel { .. } => return,
        };
        if length != 0 {
            *selected = (*selected as isize + delta).rem_euclid(length as isize) as usize;
        }
    }

    fn commit_picker(&mut self) -> Result<(), AppError> {
        let Some(picker) = self.state.picker.clone() else {
            return Ok(());
        };
        match picker {
            Picker::Provider { filter, selected } => {
                let candidates = provider_candidates(&self.registry, &filter);
                if let Some(provider) = candidates.get(selected) {
                    if let Some(reason) = missing_credential(provider) {
                        self.state.notice(reason);
                    } else {
                        self.open_model_picker(provider.clone())?;
                    }
                }
            }
            Picker::Model {
                provider,
                filter,
                selected,
            } => {
                let candidates = model_candidates(&self.registry, &provider, &filter);
                if let Some(model) = candidates.get(selected) {
                    if model == "<custom model>" {
                        self.state.picker = Some(Picker::CustomModel {
                            provider,
                            input: String::new(),
                        });
                    } else {
                        self.select_model(provider, model.clone())?;
                    }
                }
            }
            Picker::CustomModel { provider, input } => {
                if input.trim().is_empty() {
                    self.state.notice("custom model ID cannot be empty");
                } else {
                    self.select_model(provider, input)?;
                }
            }
        }
        Ok(())
    }

    fn select_model(&mut self, provider: String, model: String) -> Result<(), AppError> {
        if self.agent_is_active() {
            self.state.notice("model changes require an idle agent");
            return Ok(());
        }
        let configured = self.configured_provider(&provider, &model)?;
        let descriptor = configured.descriptor.clone();
        self.agent_or_setup()?
            .replace_model_provider(descriptor.clone(), configured.provider)?;
        self.state.selected_model = Some(descriptor);
        self.state.picker = None;
        self.state.set_snapshot(self.agent_or_setup()?.snapshot());
        self.state.notice(format!("selected {provider}/{model}"));
        Ok(())
    }

    fn configured_provider(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<ConfiguredProvider, AppError> {
        let descriptor = self
            .registry
            .resolve_model(provider, model.to_owned())?
            .into_descriptor();
        let configuration = match provider {
            "openrouter" => {
                let key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
                    AppError::Setup("OPENROUTER_API_KEY is required for OpenRouter".into())
                })?;
                ProviderConfiguration::OpenRouter(
                    pi_agent_core::provider::openrouter::OpenRouterConfig::try_new(key, model)
                        .map_err(|error| AppError::Setup(error.to_string()))?,
                )
            }
            "command-code" => {
                let key = std::env::var("COMMANDCODE_API_KEY").map_err(|_| {
                    AppError::Setup("COMMANDCODE_API_KEY is required for Command Code".into())
                })?;
                let workspace = self
                    .workspace
                    .as_ref()
                    .ok_or_else(|| AppError::Setup("workspace is not initialized".into()))?;
                let host = pi_agent_core::provider::commandcode::CommandCodeHostContext::new(
                    workspace.to_string_lossy(),
                    utc_date(),
                    std::env::consts::OS,
                )
                .map_err(|error| AppError::Setup(error.to_string()))?;
                ProviderConfiguration::CommandCode(
                    pi_agent_core::provider::commandcode::CommandCodeConfig::new(key, model, host)
                        .map_err(|error| AppError::Setup(error.to_string()))?,
                )
            }
            _ => {
                return Err(AppError::Setup(format!(
                    "provider {provider:?} is not compiled in"
                )))
            }
        };
        self.registry
            .build(descriptor, configuration)
            .map_err(Into::into)
    }

    fn spawn_run(&mut self, run: RunHandle) {
        self.spawn_operation(async move { run.drive().await });
    }

    fn spawn_compaction(&mut self, compaction: CompactionHandle) {
        self.spawn_operation(async move { compaction.drive().await });
    }

    fn spawn_operation<F>(&mut self, operation: F)
    where
        F: std::future::Future<Output = Result<(), CoreError>> + Send + 'static,
    {
        let (sender, receiver) = sync_channel(1);
        smol::spawn(async move {
            let _ = sender.send(operation.await);
        })
        .detach();
        self.active_task = Some(receiver);
        self.state.status = UiStatus::Active;
    }

    fn agent_or_setup(&self) -> Result<&Agent, AppError> {
        self.core
            .as_ref()
            .ok_or_else(|| AppError::Setup("agent is not initialized".into()))
    }

    fn agent_is_active(&self) -> bool {
        self.core
            .as_ref()
            .is_some_and(|agent| !matches!(agent.snapshot().phase, AgentPhase::Idle))
    }

    fn selected_provider(&self) -> Option<String> {
        self.state
            .selected_model
            .as_ref()
            .map(|model| model.provider.clone())
    }
}

fn os_text(value: &OsStr, flag: &str) -> Result<String, AppError> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::Setup(format!("{flag} must be valid UTF-8")))
}

/// Build the agent shared by the interactive host and its headless tests.
///
/// Provider adapters consume the standard OpenAI-compatible context produced by the host
/// policy hook. Keeping this assembly in one function makes a headless provider probe exercise
/// the same boundary as the terminal application.
pub fn build_host_agent(
    tools: DefaultCodingTools,
) -> Result<pi_agent_core::AgentBuilder, AppError> {
    Agent::builder()
        .hooks(Arc::new(OpenAiContextHook))
        .pinned_default_coding_profile(tools)
        .map_err(|error| AppError::Setup(error.to_string()))
}

fn provider_candidates(registry: &ProviderRegistry, filter: &str) -> Vec<String> {
    let filter = filter.to_ascii_lowercase();
    registry
        .providers()
        .iter()
        .filter(|entry| {
            entry.id.to_ascii_lowercase().contains(&filter)
                || entry.display_name.to_ascii_lowercase().contains(&filter)
        })
        .map(|entry| entry.id.to_owned())
        .collect()
}

fn missing_credential(provider: &str) -> Option<String> {
    let variable = match provider {
        "openrouter" => "OPENROUTER_API_KEY",
        "command-code" => "COMMANDCODE_API_KEY",
        _ => return Some("provider is not compiled in".into()),
    };
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .is_none()
        .then(|| format!("{variable} is unavailable"))
}

fn model_candidates(registry: &ProviderRegistry, provider: &str, filter: &str) -> Vec<String> {
    let filter = filter.to_ascii_lowercase();
    let mut candidates = registry
        .provider(provider)
        .into_iter()
        .flat_map(|entry| entry.models.iter())
        .filter(|model| {
            model.id.to_ascii_lowercase().contains(&filter)
                || model.display_name.to_ascii_lowercase().contains(&filter)
        })
        .map(|model| model.id.to_owned())
        .collect::<Vec<_>>();
    if registry
        .provider(provider)
        .is_some_and(|entry| entry.allows_custom_model())
        && "custom model".contains(&filter)
    {
        candidates.push("<custom model>".into());
    }
    candidates
}

fn overlay_lines(title: &str, filter: &str, candidates: &[String], selected: usize) -> Vec<String> {
    let mut lines = vec![format!("{title} picker: {filter}")];
    if candidates.is_empty() {
        lines.push("(no matching compiled entries)".into());
    } else {
        lines.extend(candidates.iter().enumerate().map(|(index, candidate)| {
            format!("{} {candidate}", if index == selected { '>' } else { ' ' })
        }));
    }
    lines.push("Enter selects; Esc cancels".into());
    lines
}

/// Format only provider-reported values; missing fields remain absent rather than zero.
pub fn format_usage(usage: &Usage) -> String {
    let mut fields = Vec::new();
    if let Some(value) = usage.input_tokens {
        fields.push(format!("in {value}"));
    }
    if let Some(value) = usage.output_tokens {
        fields.push(format!("out {value}"));
    }
    if let Some(value) = usage.reasoning_tokens {
        fields.push(format!("reasoning {value}"));
    }
    if let Some(value) = usage.cache_read_tokens {
        fields.push(format!("cache-read {value}"));
    }
    if let Some(value) = usage.cache_write_tokens {
        fields.push(format!("cache-write {value}"));
    }
    if let Some(value) = usage.cost.as_deref() {
        fields.push(format!("cost {value}"));
    }
    if fields.is_empty() {
        "provider reported no accounting".into()
    } else {
        fields.join(", ")
    }
}

fn composer_cursor(state: &AppState, width: u16, height: u16) -> Option<(u16, u16)> {
    let layout = render::layout(width, height);
    if layout.composer.height == 0 || state.picker.is_some() || state.composer().is_multiline() {
        return None;
    }
    let cursor = state.composer().text()[..state.composer().cursor()]
        .chars()
        .count() as u16;
    Some((
        2u16.saturating_add(cursor).min(width.saturating_sub(1)),
        layout.composer.y,
    ))
}

/// Format today's UTC civil date without adding a date/time dependency.
fn utc_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

// Howard Hinnant's public-domain civil-date conversion, expressed locally to
// keep Command Code host metadata explicit without a time crate.
fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_agent_core::scheduler::{
        CancellationToken, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
    };
    use pi_agent_core::state::{Message, MessageId};
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
        state.apply_event(&AgentEvent {
            run_id: pi_agent_core::RunId(1),
            sequence: pi_agent_core::EventSequence(1),
            kind: AgentEventKind::MessageUpdate {
                message: message.clone(),
                text_delta: Some("hel".into()),
            },
        });
        state.apply_event(&AgentEvent {
            run_id: pi_agent_core::RunId(1),
            sequence: pi_agent_core::EventSequence(2),
            kind: AgentEventKind::MessageUpdate {
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
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
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
        let agent = Agent::builder().build();
        let active = agent.start_prompt("active").expect("run starts");
        let mut app = App::new(CliOptions::default());
        app.attach_agent(agent.clone());

        app.dispatch_command("/clear").expect("command is handled");

        assert!(matches!(
            app.state().status(),
            UiStatus::Notice(notice) if notice == "cannot clear while the agent is active"
        ));
        assert!(matches!(agent.snapshot().phase, AgentPhase::Running(_)));
        active.abort().expect("fixture cleanup");
    }
}

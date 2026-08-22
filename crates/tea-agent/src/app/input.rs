use crate::editor::Editor;
use crate::terminal::TerminalGuard;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tea_core::state::AgentPhase;
use tea_core::{CoreError, Usage};

use super::commands;
use super::error::AppError;
use super::runtime::App;
use super::state::{QueueDelivery, UiSurface};
use super::support::format_footer_usage;

impl App {
    pub(super) fn handle_terminal_event(
        &mut self,
        terminal: &mut TerminalGuard,
        event: Event,
    ) -> Result<(), AppError> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(terminal, key),
            Event::Paste(text)
                if self.state.picker.is_none()
                    && matches!(self.state.surface(), UiSurface::None) =>
            {
                self.state.composer_mut().insert_str_multiline(&text);
                self.refresh_command_completion();
                Ok(())
            }
            Event::Paste(text) => self.picker_insert(&text),
            Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => Ok(()),
            _ => Ok(()),
        }
    }

    fn handle_key(&mut self, terminal: &mut TerminalGuard, key: KeyEvent) -> Result<(), AppError> {
        if self.state.picker.is_some() {
            return self.handle_picker_key(key);
        }
        if self.state.surface() == UiSurface::ToolDetail
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('o')
        {
            self.state.toggle_tool_detail();
            return Ok(());
        }
        if self.state.surface() == UiSurface::ToolDetail {
            let page = usize::from(terminal.size()?.1.saturating_sub(3)).max(1);
            match key.code {
                KeyCode::PageUp | KeyCode::Up => self.state.page_surface_up(page),
                KeyCode::PageDown | KeyCode::Down => self.state.page_surface_down(page),
                _ => {}
            }
            if matches!(
                key.code,
                KeyCode::PageUp | KeyCode::Up | KeyCode::PageDown | KeyCode::Down
            ) {
                return Ok(());
            }
        }
        if !matches!(self.state.surface(), UiSurface::None) && key.code == KeyCode::Esc {
            self.state.close_surface();
            return Ok(());
        }
        if !matches!(self.state.surface(), UiSurface::None) {
            return Ok(());
        }
        if self.state.slash_completion.is_some() {
            match key.code {
                KeyCode::Esc => {
                    self.state.slash_completion = None;
                    return Ok(());
                }
                KeyCode::Up => {
                    self.state.move_slash_completion(-1);
                    return Ok(());
                }
                KeyCode::Down => {
                    self.state.move_slash_completion(1);
                    return Ok(());
                }
                KeyCode::Tab => {
                    self.complete_command();
                    return Ok(());
                }
                KeyCode::Enter => {
                    if let Some(command) = self.state.selected_slash_completion().map(str::to_owned)
                    {
                        self.state
                            .composer_mut()
                            .replace_from_editor(format!("{command} "));
                        self.state.slash_completion = None;
                        return self.submit_composer();
                    }
                }
                _ => {}
            }
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') {
            self.state.slash_completion = None;
            let current = self.state.composer().text().to_owned();
            match Editor::open(terminal, &current) {
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
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
            self.state.toggle_tool_detail();
            self.previous_grid = None;
            return Ok(());
        }
        match key.code {
            KeyCode::Tab => self.complete_command(),
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
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.state.follow_end()
            }
            KeyCode::End => self.state.composer_mut().end(),
            KeyCode::Up => {
                let width = terminal.size()?.0;
                if !self.state.composer_mut().move_visual_line_up(width) {
                    self.state.begin_history_navigation();
                    if let Some(history) = self.state.history_previous() {
                        self.state.composer_mut().replace_from_editor(history);
                    }
                }
            }
            KeyCode::Down => {
                let width = terminal.size()?.0;
                if !self.state.composer_mut().move_visual_line_down(width) {
                    if let Some(history) = self.state.history_next() {
                        self.state.composer_mut().replace_from_editor(history);
                    }
                }
            }
            KeyCode::PageUp => self.state.page_up(5),
            KeyCode::PageDown => self.state.page_down(5),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.state.composer_mut().insert_newline()
            }
            KeyCode::Enter => self.submit_composer()?,
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.state.composer_mut().move_word_left()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.state.composer_mut().move_word_right()
            }
            _ => {}
        }
        self.refresh_command_completion();
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
        self.state.slash_completion = None;
    }

    fn submit_composer(&mut self) -> Result<(), AppError> {
        let input = self.state.composer_mut().take();
        if input.trim().is_empty() {
            return Ok(());
        }
        self.state.record_history(&input);
        if input.starts_with('/') {
            self.dispatch_command(&input)
        } else {
            let agent = self.agent_or_setup()?.clone();
            match agent.snapshot().phase {
                AgentPhase::Idle if !agent.has_model_provider() => {
                    self.state.notice("select a model first");
                    self.open_model_picker();
                }
                AgentPhase::Idle => match agent.start_prompt(input.clone()) {
                    Ok(run) => {
                        self.submitted_prompt = Some(input);
                        self.spawn_run(run);
                    }
                    Err(error) => {
                        self.state.composer_mut().replace_from_editor(input);
                        self.state.notice(error.to_string());
                    }
                },
                AgentPhase::Running(_) | AgentPhase::Cancelling(_) => {
                    agent.enqueue_steering(input)?;
                    self.state.set_queue_snapshot(&agent);
                    self.state.notice("steering queued");
                }
            }
            Ok(())
        }
    }

    pub(super) fn complete_command(&mut self) {
        let input = self.state.composer().text().to_owned();
        let Some(prefix) = input.split_whitespace().next() else {
            return;
        };
        if !prefix.starts_with('/') || input.chars().any(char::is_whitespace) {
            return;
        }
        let matches = commands::matching(prefix);
        let Some(command) = self
            .state
            .selected_slash_completion()
            .or_else(|| matches.first().map(|command| command.name))
            .map(str::to_owned)
        else {
            return;
        };
        self.state
            .composer_mut()
            .replace_from_editor(format!("{command} "));
        self.state.slash_completion = None;
    }

    fn refresh_command_completion(&mut self) {
        let input = self.state.composer().text();
        let Some(prefix) = input.split_whitespace().next() else {
            self.state.slash_completion = None;
            return;
        };
        if !prefix.starts_with('/') || input.chars().any(char::is_whitespace) {
            self.state.slash_completion = None;
            return;
        }
        self.state.update_slash_completion(
            commands::matching(prefix)
                .into_iter()
                .map(|command| command.name.to_owned())
                .collect(),
        );
    }

    pub(super) fn dispatch_command(&mut self, input: &str) -> Result<(), AppError> {
        self.state.slash_completion = None;
        let mut words = input.split_whitespace();
        let command = words.next().unwrap_or_default();
        if self.agent_is_active()
            && commands::find(command).is_some_and(|spec| !spec.allowed_while_active)
        {
            self.state
                .notice(format!("{command} is unavailable while a run is active"));
            return Ok(());
        }
        match command {
            "/help" => {
                self.state
                    .set_surface_lines(UiSurface::Help, help_surface_lines());
            }
            "/model" => {
                if let (Some(provider), Some(model)) = (words.next(), words.next()) {
                    self.select_model(provider.to_owned(), model.to_owned())?;
                } else {
                    self.open_model_picker();
                }
            }
            "/cost" => {
                self.show_cost_surface();
            }
            "/steer" => self.enqueue_command_prompt(
                input.strip_prefix("/steer").unwrap_or_default(),
                QueueDelivery::Steering,
            )?,
            "/followup" => self.enqueue_command_prompt(
                input.strip_prefix("/followup").unwrap_or_default(),
                QueueDelivery::FollowUp,
            )?,
            "/session" | "/resume" => {
                if let Err(error) = self.open_session_picker() {
                    self.state.notice(error.to_string());
                }
            }
            "/new" => {
                if let Err(error) = self.new_session() {
                    self.state.notice(error.to_string());
                }
            }
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
            "/reload-extensions" => {
                if let Err(error) = self.reload_tea_extensions() {
                    self.state.notice(format!(
                        "Tea extensions were not reloaded; the previous snapshot remains active: {error}"
                    ));
                }
            }
            "/clear" => {
                let agent = self.agent_or_setup()?;
                match agent.reset() {
                    Ok(()) => {
                        let snapshot = agent.snapshot();
                        self.state.clear_transcript();
                        self.state.close_surface();
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
                    if !matches!(agent.snapshot().phase, AgentPhase::Idle) {
                        agent.abort();
                        self.state.notice("cancelling before exit");
                    }
                }
            }
            command => self.state.notice(format!("unknown command {command}")),
        }
        Ok(())
    }

    fn enqueue_command_prompt(
        &mut self,
        content: &str,
        delivery: QueueDelivery,
    ) -> Result<(), AppError> {
        let content = content.trim();
        if content.is_empty() {
            self.state.notice(match delivery {
                QueueDelivery::Steering => "usage: /steer <prompt>",
                QueueDelivery::FollowUp => "usage: /followup <prompt>",
            });
            return Ok(());
        }
        let agent = self.agent_or_setup()?.clone();
        match delivery {
            QueueDelivery::Steering => agent.enqueue_steering(content)?,
            QueueDelivery::FollowUp => agent.enqueue_follow_up(content)?,
        };
        self.state.set_queue_snapshot(&agent);
        self.state.notice(match delivery {
            QueueDelivery::Steering => "steering queued",
            QueueDelivery::FollowUp => "follow-up queued",
        });
        Ok(())
    }

    fn show_cost_surface(&mut self) {
        let Some(snapshot) = self.state.snapshot().cloned() else {
            self.state.set_surface_lines(
                UiSurface::Cost,
                vec![format!(
                    "cost total: {}",
                    format_footer_usage(&Usage::default())
                )],
            );
            return;
        };
        if snapshot.accounting.turns.is_empty() {
            self.state.set_surface_lines(
                UiSurface::Cost,
                vec![format!(
                    "cost total: {}",
                    format_footer_usage(&snapshot.accounting.aggregate)
                )],
            );
            return;
        }
        let mut lines = Vec::with_capacity(snapshot.accounting.turns.len() + 1);
        for turn in &snapshot.accounting.turns {
            let model = turn
                .model
                .as_ref()
                .map(|model| format!("{}/{}", model.provider, model.model))
                .unwrap_or_else(|| "unknown model".into());
            lines.push(format!(
                "cost run {} turn {} {model}: {}",
                turn.run_id.0,
                turn.turn_id.0,
                format_footer_usage(&turn.usage)
            ));
        }
        lines.push(format!(
            "cost total: {}",
            format_footer_usage(&snapshot.accounting.aggregate)
        ));
        self.state.set_surface_lines(UiSurface::Cost, lines);
    }
}

fn help_surface_lines() -> Vec<String> {
    const GROUPS: &[(&str, &[&str])] = &[
        ("General", &["/help", "/clear", "/quit"]),
        ("Session", &["/new", "/session", "/resume"]),
        ("Runtime", &["/model", "/cost", "/compact"]),
        ("Queue", &["/steer", "/followup"]),
        ("Extensions", &["/reload-extensions"]),
    ];

    let mut lines = vec![format!("Commands {}", commands::all().len())];
    for (heading, names) in GROUPS {
        lines.push(String::new());
        lines.push((*heading).into());
        for name in *names {
            let spec = commands::find(name).expect("help groups use registered commands");
            lines.push(format!("  {:<20} {}", spec.name, spec.help));
        }
    }
    lines
}

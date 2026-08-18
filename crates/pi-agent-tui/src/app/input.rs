use crate::editor::Editor;
use crate::terminal::TerminalGuard;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use pi_agent_core::state::AgentPhase;
use pi_agent_core::CoreError;

use super::error::AppError;
use super::runtime::App;
use super::state::QueueDelivery;
use super::support::format_usage;

impl App {
    pub(super) fn handle_terminal_event(
        &mut self,
        terminal: &mut TerminalGuard,
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

    fn handle_key(&mut self, terminal: &mut TerminalGuard, key: KeyEvent) -> Result<(), AppError> {
        if self.state.picker.is_some() {
            return self.handle_picker_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') {
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
            let agent = self.agent_or_setup()?.clone();
            match agent.snapshot().phase {
                AgentPhase::Idle if !agent.has_model_provider() => {
                    self.state.notice("select a provider and model first");
                    self.open_provider_picker();
                }
                AgentPhase::Idle => self.spawn_run(agent.start_prompt(input)?),
                AgentPhase::Running(_) | AgentPhase::Cancelling(_) => {
                    agent.enqueue_steering(input)?;
                    self.state.set_queue_snapshot(&agent);
                    self.state.notice("steering queued");
                }
            }
            Ok(())
        }
    }

    pub(super) fn dispatch_command(&mut self, input: &str) -> Result<(), AppError> {
        let mut words = input.split_whitespace();
        match words.next().unwrap_or_default() {
            "/help" => {
                self.state.local_line(
                    "keys: Enter submit, Ctrl+C cancel/clear/quit, Ctrl+G $EDITOR, PgUp/PgDn/End scroll; commands: /provider /model /cost /compact /steer <prompt> /followup <prompt> /clear /quit",
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
            "/steer" => self.enqueue_command_prompt(
                input.strip_prefix("/steer").unwrap_or_default(),
                QueueDelivery::Steering,
            )?,
            "/followup" => self.enqueue_command_prompt(
                input.strip_prefix("/followup").unwrap_or_default(),
                QueueDelivery::FollowUp,
            )?,
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
}

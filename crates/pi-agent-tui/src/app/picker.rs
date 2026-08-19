use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pi_agent_core::compaction::{
    AutomaticCompactionPolicy, ContextBudgetSource, OverflowRecovery,
};
use pi_agent_core::provider::{ConfiguredProvider, ProviderConfiguration};
use std::num::NonZeroU64;
use std::sync::Arc;

use super::error::AppError;
use super::host::{missing_credential, model_candidates, provider_candidates};
use super::runtime::App;
use super::state::Picker;
use super::support::utc_date;

impl App {
    pub(super) fn open_provider_picker(&mut self) {
        if self.agent_is_active() {
            self.state.notice("provider changes require an idle agent");
            return;
        }
        self.state.picker = Some(Picker::Provider {
            filter: String::new(),
            selected: 0,
        });
    }

    pub(super) fn open_model_picker(&mut self, provider: String) -> Result<(), AppError> {
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

    pub(super) fn handle_picker_key(&mut self, key: KeyEvent) -> Result<(), AppError> {
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

    pub(super) fn picker_insert(&mut self, text: &str) -> Result<(), AppError> {
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

    pub(super) fn select_model(&mut self, provider: String, model: String) -> Result<(), AppError> {
        if self.agent_is_active() {
            self.state.notice("model changes require an idle agent");
            return Ok(());
        }
        let configured = self.configured_provider(&provider, &model)?;
        let descriptor = configured.descriptor.clone();
        let configured_provider = configured.provider;
        self.agent_or_setup()?
            .replace_model_provider(descriptor.clone(), Arc::clone(&configured_provider))?;
        if let Some(compactor) = &self.compactor {
            compactor.configure(descriptor.clone(), Arc::clone(&configured_provider));
        }
        let policy = if self.compactor.is_some() {
            self.registry
                .provider(&provider)
                .and_then(|entry| entry.model(&model))
                .and_then(|model| model.context_window)
                .and_then(NonZeroU64::new)
                .map(automatic_compaction_policy)
                .unwrap_or_else(AutomaticCompactionPolicy::disabled)
        } else {
            AutomaticCompactionPolicy::disabled()
        };
        self.agent_or_setup()?.replace_automatic_compaction(policy)?;
        self.state.automatic_compaction_enabled = self
            .agent_or_setup()?
            .automatic_compaction()
            .enabled;
        self.state.selected_model = Some(descriptor);
        self.state.context_estimate = None;
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
                let config =
                    pi_agent_core::provider::openrouter::OpenRouterConfig::try_new(key, model)
                        .map_err(|error| AppError::Setup(error.to_string()))?;
                #[cfg(feature = "pty-harness")]
                let config = test_openrouter_config(config)?;
                ProviderConfiguration::OpenRouter(config)
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
            "local" => {
                let base_url = self
                    .options
                    .local_base_url()
                    .map(|value| super::runtime::os_text(value, "--local-base-url"))
                    .transpose()?
                    .unwrap_or_else(|| {
                        pi_agent_core::provider::local::DEFAULT_BASE_URL.to_owned()
                    });
                let config = pi_agent_core::provider::local::LocalConfig::try_new(
                    base_url,
                    model,
                )
                .map_err(|error| AppError::Setup(error.to_string()))?;
                ProviderConfiguration::Local(config)
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

    pub(super) fn selected_provider(&self) -> Option<String> {
        self.state
            .selected_model
            .as_ref()
            .map(|model| model.provider.clone())
    }
}

fn automatic_compaction_policy(context_window: NonZeroU64) -> AutomaticCompactionPolicy {
    let capacity = context_window.get();
    // Reserve room for the summary request and keep a bounded intact suffix. Large
    // OpenRouter windows use fixed practical bounds; smaller windows retain proportional room.
    AutomaticCompactionPolicy {
        enabled: true,
        context_budget: ContextBudgetSource::ContextWindow(context_window),
        reserved_tokens: (capacity / 4).min(16_384),
        recent_tokens: (capacity / 2).min(20_000),
        overflow_recovery: OverflowRecovery::CompactAndRetry,
        max_compactions_per_run: 4,
        max_overflow_retries_per_run: 1,
    }
}

#[cfg(feature = "pty-harness")]
fn test_openrouter_config(
    config: pi_agent_core::provider::openrouter::OpenRouterConfig,
) -> Result<pi_agent_core::provider::openrouter::OpenRouterConfig, AppError> {
    let Some(url) = std::env::var_os("PI_AGENT_TUI_TEST_OPENROUTER_URL") else {
        return Ok(config);
    };
    let url = url.to_str().ok_or_else(|| {
        AppError::Setup("PI_AGENT_TUI_TEST_OPENROUTER_URL must be valid UTF-8".into())
    })?;
    Ok(config.with_test_completion_url(url))
}

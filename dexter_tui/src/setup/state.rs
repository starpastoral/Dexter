use anyhow::{anyhow, Result};
use dexter_core::{Config, LlmClient, ModelRoute, ProviderAuth, ProviderConfig, ProviderKind};
use std::time::Duration;

use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq)]
pub enum SetupState {
    Welcome,
    ProviderSelection,
    ProviderConfig,
    FetchingProviderModels,
    ProviderModelSelection,
    ModelOrderSelection,
    ThemeSelection,
    Confirm,
    Saving,
    Error(String),
}

impl SetupState {
    pub fn on_model_order_enter(has_models: bool) -> Self {
        if has_models {
            SetupState::ThemeSelection
        } else {
            SetupState::Error("No active models in routing order.".to_string())
        }
    }

    pub fn on_theme_enter() -> Self {
        SetupState::Confirm
    }

    pub fn on_confirm_enter() -> Self {
        SetupState::Saving
    }
}

#[derive(Debug, Clone)]
pub struct SetupProviderEntry {
    pub kind: ProviderKind,
    pub enabled: bool,
    pub api_key: String,
    pub base_url: String,
    pub auth: ProviderAuth,
    pub available_models: Vec<String>,
    pub active_models: Vec<String>,
    pub runtime_ready: Option<bool>,
}

impl SetupProviderEntry {
    pub fn name(&self) -> &'static str {
        self.kind.display_name()
    }

    pub fn requires_api_key(&self) -> bool {
        !matches!(self.auth, ProviderAuth::None)
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.trim().is_empty()
    }

    pub fn to_provider_config(&self) -> ProviderConfig {
        let api_key = if self.api_key.trim().is_empty() {
            None
        } else {
            Some(self.api_key.trim().to_string())
        };
        ProviderConfig {
            kind: self.kind,
            name: Some(self.name().to_string()),
            api_key,
            base_url: self.base_url.trim().to_string(),
            auth: self.auth,
            enabled: self.enabled,
            models: dedup_models(self.active_models.clone()),
        }
        .normalized()
    }

    pub fn to_provider_config_for_fetch(&self) -> ProviderConfig {
        let mut cfg = self.to_provider_config();
        cfg.enabled = true;
        cfg
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouteDraft {
    pub provider_idx: usize,
    pub model: String,
}

impl ModelRouteDraft {
    pub fn key(&self) -> String {
        format!("{}::{}", self.provider_idx, self.model)
    }
}

pub struct SetupApp {
    pub state: SetupState,
    pub providers: Vec<SetupProviderEntry>,
    pub selected_provider_idx: usize,
    pub config_provider_idx: Option<usize>,
    pub guided_provider_order: Vec<usize>,
    pub guided_provider_pos: usize,
    pub provider_model_cursor: usize,
    pub model_order: Vec<ModelRouteDraft>,
    pub model_order_cursor: usize,
    pub available_themes: Vec<(&'static str, &'static str)>,
    pub selected_theme_idx: usize,
    pub config: Config,
    pub theme: Theme,
}

impl SetupApp {
    pub fn new(config: Config, show_welcome: bool) -> Self {
        let mut providers = build_provider_entries(&config);
        if !providers.iter().any(|p| p.enabled) {
            if let Some(gemini) = providers
                .iter_mut()
                .find(|p| matches!(p.kind, ProviderKind::Gemini))
            {
                gemini.enabled = true;
                if gemini.active_models.is_empty() {
                    gemini.active_models = vec!["gemini-2.5-flash-lite".to_string()];
                }
            }
        }

        let mut app = Self {
            state: if show_welcome {
                SetupState::Welcome
            } else {
                SetupState::ProviderSelection
            },
            providers,
            selected_provider_idx: 0,
            config_provider_idx: None,
            guided_provider_order: Vec::new(),
            guided_provider_pos: 0,
            provider_model_cursor: 0,
            model_order: Vec::new(),
            model_order_cursor: 0,
            available_themes: vec![
                ("auto", "🔄 Auto (Follow system appearance)"),
                ("dark", "🌑 Dark (Blue on charcoal)"),
                ("retro", "🌙 Retro (Classic amber CRT aesthetic)"),
                ("light", "☀️ Light (Clean blue/white for light terminals)"),
            ],
            selected_theme_idx: 0,
            theme: Theme::from_config(&config.theme),
            config,
        };

        if let Some(idx) = app
            .available_themes
            .iter()
            .position(|(id, _)| *id == app.config.theme)
        {
            app.selected_theme_idx = idx;
        }

        let mut seeded = false;
        if !app.config.models.executor_routes.is_empty() {
            for route in &app.config.models.executor_routes {
                if let Some((provider_idx, _)) = app
                    .providers
                    .iter()
                    .enumerate()
                    .find(|(_, p)| p.kind == route.provider)
                {
                    if route.model.trim().is_empty() {
                        continue;
                    }
                    app.model_order.push(ModelRouteDraft {
                        provider_idx,
                        model: route.model.clone(),
                    });
                    seeded = true;
                }
            }
        }

        if !seeded {
            app.update_model_order_from_active();
        }

        app
    }

    pub async fn refresh_runtime_statuses(&mut self) {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_millis(900))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };

        for provider in &mut self.providers {
            if provider.kind != ProviderKind::Ollama {
                continue;
            }
            let base = provider.base_url.trim().trim_end_matches('/');
            let root = base.strip_suffix("/v1").unwrap_or(base);
            let probe_url = format!("{}/api/tags", root);
            let ready = client
                .get(&probe_url)
                .send()
                .await
                .map(|res| res.status().is_success())
                .unwrap_or(false);
            provider.runtime_ready = Some(ready);
        }
    }

    pub fn provider_selection_len(&self) -> usize {
        self.providers.len()
    }

    pub fn current_guided_provider_idx(&self) -> Option<usize> {
        self.guided_provider_order
            .get(self.guided_provider_pos)
            .copied()
    }

    pub fn start_guided_flow(&mut self) -> Result<()> {
        let mut order: Vec<usize> = self
            .providers
            .iter()
            .enumerate()
            .filter_map(|(idx, p)| if p.enabled { Some(idx) } else { None })
            .collect();

        if order.is_empty() {
            return Err(anyhow!("Please enable at least one provider."));
        }

        if let Some(pos) = order
            .iter()
            .position(|idx| *idx == self.selected_provider_idx)
        {
            order.rotate_left(pos);
        }

        self.guided_provider_order = order;
        self.guided_provider_pos = 0;
        self.config_provider_idx = self.current_guided_provider_idx();
        self.provider_model_cursor = 0;
        self.state = SetupState::ProviderConfig;
        Ok(())
    }

    pub fn reset_guided_flow(&mut self) {
        self.guided_provider_order.clear();
        self.guided_provider_pos = 0;
        self.config_provider_idx = None;
        self.provider_model_cursor = 0;
    }

    pub fn advance_provider_config(&mut self) {
        if self.guided_provider_pos + 1 < self.guided_provider_order.len() {
            self.guided_provider_pos += 1;
            self.config_provider_idx = self.current_guided_provider_idx();
            self.state = SetupState::ProviderConfig;
        } else {
            self.guided_provider_pos = 0;
            self.config_provider_idx = self.current_guided_provider_idx();
            self.provider_model_cursor = 0;
            self.state = SetupState::FetchingProviderModels;
        }
    }

    pub fn advance_provider_models(&mut self) {
        if self.guided_provider_pos + 1 < self.guided_provider_order.len() {
            self.guided_provider_pos += 1;
            self.config_provider_idx = self.current_guided_provider_idx();
            self.provider_model_cursor = 0;
            self.state = SetupState::FetchingProviderModels;
        } else if self.ensure_at_least_one_route() {
            self.state = SetupState::ModelOrderSelection;
        } else {
            self.state = SetupState::Error(
                "No active models. Select at least one model for an enabled provider.".to_string(),
            );
        }
    }

    pub fn enabled_provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|p| p.enabled)
            .map(|p| p.name().to_string())
            .collect()
    }

    pub fn disabled_provider_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|p| !p.enabled)
            .map(|p| p.name().to_string())
            .collect()
    }

    pub fn toggle_provider_enabled(&mut self, provider_idx: usize) {
        if let Some(provider) = self.providers.get_mut(provider_idx) {
            provider.enabled = !provider.enabled;
            if provider.enabled
                && provider.active_models.is_empty()
                && !provider.available_models.is_empty()
            {
                provider
                    .active_models
                    .push(provider.available_models[0].clone());
            }
        }
        self.update_model_order_from_active();
    }

    pub fn update_model_order_from_active(&mut self) -> bool {
        let mut valid = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for (provider_idx, provider) in self.providers.iter().enumerate() {
            if !provider.enabled {
                continue;
            }
            for model in &provider.active_models {
                let model = model.trim();
                if model.is_empty() {
                    continue;
                }
                let draft = ModelRouteDraft {
                    provider_idx,
                    model: model.to_string(),
                };
                if seen.insert(draft.key()) {
                    valid.push(draft);
                }
            }
        }

        if self.model_order.is_empty() {
            self.model_order = valid;
        } else {
            let valid_keys: std::collections::HashSet<String> =
                valid.iter().map(ModelRouteDraft::key).collect();
            let mut merged = Vec::new();
            let mut merged_keys = std::collections::HashSet::new();

            for old in &self.model_order {
                let key = old.key();
                if valid_keys.contains(&key) && merged_keys.insert(key) {
                    merged.push(old.clone());
                }
            }

            for v in valid {
                let key = v.key();
                if merged_keys.insert(key) {
                    merged.push(v);
                }
            }

            self.model_order = merged;
        }

        if self.model_order.is_empty() {
            self.model_order_cursor = 0;
            return false;
        }

        if self.model_order_cursor >= self.model_order.len() {
            self.model_order_cursor = self.model_order.len().saturating_sub(1);
        }

        true
    }

    pub async fn fetch_models_for_current_provider(&mut self) -> Result<()> {
        let provider_idx = self
            .config_provider_idx
            .ok_or_else(|| anyhow!("No provider selected for model fetch"))?;

        let provider_cfg = self.providers[provider_idx].to_provider_config_for_fetch();
        let primary = self.providers[provider_idx]
            .active_models
            .first()
            .cloned()
            .or_else(|| {
                self.providers[provider_idx]
                    .available_models
                    .first()
                    .cloned()
            })
            .or_else(|| provider_cfg.models.first().cloned())
            .unwrap_or_else(|| "gemini-2.5-flash-lite".to_string());

        let client = LlmClient::with_routes(vec![provider_cfg], Vec::new(), primary, Vec::new());

        let discovered_models = client.list_models().await;
        let discovered_models = match discovered_models {
            Ok(models) => {
                if self.providers[provider_idx].kind == ProviderKind::Ollama {
                    self.providers[provider_idx].runtime_ready = Some(true);
                }
                models
            }
            Err(_) => {
                if self.providers[provider_idx].kind == ProviderKind::Ollama {
                    self.providers[provider_idx].runtime_ready = Some(false);
                }
                self.providers[provider_idx].kind.default_models()
            }
        };

        let mut available_models = dedup_models(discovered_models);
        if available_models.is_empty() {
            available_models = self.providers[provider_idx].kind.default_models();
        }

        let available_set: std::collections::HashSet<String> =
            available_models.iter().cloned().collect();
        let mut active_models = self.providers[provider_idx]
            .active_models
            .iter()
            .filter(|m| available_set.contains(*m))
            .cloned()
            .collect::<Vec<_>>();

        if active_models.is_empty() && !available_models.is_empty() {
            active_models.push(available_models[0].clone());
        }

        self.providers[provider_idx].available_models = available_models;
        self.providers[provider_idx].active_models = dedup_models(active_models);
        self.provider_model_cursor = 0;
        Ok(())
    }

    pub fn toggle_model_selection(&mut self) {
        let Some(provider_idx) = self.config_provider_idx else {
            return;
        };
        let Some(provider) = self.providers.get_mut(provider_idx) else {
            return;
        };

        let model_count = provider.available_models.len();
        if model_count == 0 {
            return;
        }

        if self.provider_model_cursor < model_count {
            let model = provider.available_models[self.provider_model_cursor].clone();
            if let Some(pos) = provider.active_models.iter().position(|m| m == &model) {
                provider.active_models.remove(pos);
            } else {
                provider.active_models.push(model);
            }
            provider.active_models = dedup_models(provider.active_models.clone());
        } else {
            let all_selected = provider.active_models.len() == model_count;
            if all_selected {
                provider.active_models.clear();
            } else {
                provider.active_models = provider.available_models.clone();
            }
            provider.active_models = dedup_models(provider.active_models.clone());
        }

        self.update_model_order_from_active();
    }

    pub fn move_model_order_up(&mut self) {
        if self.model_order_cursor > 0 && self.model_order_cursor < self.model_order.len() {
            self.model_order
                .swap(self.model_order_cursor, self.model_order_cursor - 1);
            self.model_order_cursor -= 1;
        }
    }

    pub fn move_model_order_down(&mut self) {
        if self.model_order_cursor + 1 < self.model_order.len() {
            self.model_order
                .swap(self.model_order_cursor, self.model_order_cursor + 1);
            self.model_order_cursor += 1;
        }
    }

    pub fn ensure_at_least_one_route(&mut self) -> bool {
        self.update_model_order_from_active() && !self.model_order.is_empty()
    }

    pub async fn save_config(&mut self) -> Result<()> {
        self.ensure_at_least_one_route();

        let providers_cfg: Vec<ProviderConfig> = self
            .providers
            .iter()
            .map(|p| p.to_provider_config())
            .collect();
        self.config.providers = providers_cfg.clone();

        self.config.api_keys.gemini = providers_cfg
            .iter()
            .find(|p| matches!(p.kind, ProviderKind::Gemini))
            .and_then(|p| p.api_key.clone());
        self.config.api_keys.deepseek = providers_cfg
            .iter()
            .find(|p| matches!(p.kind, ProviderKind::Deepseek))
            .and_then(|p| p.api_key.clone());
        self.config.api_keys.base_url = None;

        let routes: Vec<ModelRoute> = self
            .model_order
            .iter()
            .filter_map(|r| {
                self.providers.get(r.provider_idx).map(|p| ModelRoute {
                    provider: p.kind,
                    model: r.model.clone(),
                })
            })
            .collect();

        self.config.models.router_routes = routes.clone();
        self.config.models.executor_routes = routes.clone();

        if let Some(primary) = routes.first() {
            self.config.models.router_model = primary.model.clone();
            self.config.models.executor_model = primary.model.clone();
        }

        let fallback_models =
            dedup_models(routes.iter().skip(1).map(|r| r.model.clone()).collect());
        self.config.models.router_fallback_models = fallback_models.clone();
        self.config.models.executor_fallback_models = fallback_models;

        let theme_id = self.available_themes[self.selected_theme_idx].0;
        self.config.theme = theme_id.to_string();
        self.config.save().await?;
        Ok(())
    }
}

pub fn build_provider_entries(config: &Config) -> Vec<SetupProviderEntry> {
    let existing = config.effective_providers();
    let supported = [
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::OpenRouter,
        ProviderKind::Moonshot,
        ProviderKind::Gemini,
        ProviderKind::Deepseek,
        ProviderKind::Groq,
        ProviderKind::Baseten,
        ProviderKind::Ollama,
        ProviderKind::OpenAICompatible,
        ProviderKind::AnthropicCompatible,
    ];

    let mut entries = supported
        .iter()
        .map(|kind| {
            let mut base = ProviderConfig::builtin(*kind, None).normalized();
            let mut enabled = false;
            if let Some(existing_provider) = existing.iter().find(|p| p.kind == *kind) {
                base.api_key = existing_provider.api_key.clone();
                base.base_url = existing_provider.base_url.clone();
                base.auth = existing_provider.auth;
                base.models = dedup_models(existing_provider.models.clone());
                enabled = existing_provider.enabled;
            }

            let mut available_models = if base.models.is_empty() {
                (*kind).default_models()
            } else {
                base.models.clone()
            };
            let active_models = base.models.clone();

            if available_models.is_empty() {
                available_models = (*kind).default_models();
            }

            SetupProviderEntry {
                kind: *kind,
                enabled,
                api_key: base.api_key.unwrap_or_default(),
                base_url: base.base_url,
                auth: base.auth,
                available_models: dedup_models(available_models),
                active_models: dedup_models(active_models),
                runtime_ready: None,
            }
        })
        .collect::<Vec<_>>();

    for route in &config.models.executor_routes {
        if route.model.trim().is_empty() {
            continue;
        }
        if let Some(entry) = entries.iter_mut().find(|e| e.kind == route.provider) {
            if !entry.available_models.iter().any(|m| m == &route.model) {
                entry.available_models.push(route.model.clone());
            }
            if !entry.active_models.iter().any(|m| m == &route.model) {
                entry.active_models.push(route.model.clone());
            }
            entry.enabled = true;
        }
    }

    for entry in &mut entries {
        entry.available_models = dedup_models(entry.available_models.clone());
        entry.active_models = dedup_models(entry.active_models.clone());
        if entry.enabled && entry.active_models.is_empty() && !entry.available_models.is_empty() {
            entry.active_models.push(entry.available_models[0].clone());
        }
    }

    entries
}

pub fn dedup_models(models: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for m in models {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        if !out.iter().any(|x: &String| x == m) {
            out.push(m.to_string());
        }
    }
    out
}

pub fn model_route_display(route: &ModelRouteDraft, providers: &[SetupProviderEntry]) -> String {
    let provider_name = providers
        .get(route.provider_idx)
        .map(|p| p.name().to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    format!("{} ({})", route.model, provider_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_transition_model_order_to_theme() {
        assert_eq!(
            SetupState::on_model_order_enter(true),
            SetupState::ThemeSelection
        );
    }

    #[test]
    fn setup_transition_theme_to_confirm() {
        assert_eq!(SetupState::on_theme_enter(), SetupState::Confirm);
    }

    #[test]
    fn setup_transition_confirm_to_saving() {
        assert_eq!(SetupState::on_confirm_enter(), SetupState::Saving);
    }
}

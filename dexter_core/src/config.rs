use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub api_keys: ApiKeys,
    #[serde(default)]
    pub models: ModelPreferences,
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String {
    "auto".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct ApiKeys {
    pub gemini: Option<String>,
    pub deepseek: Option<String>,
    pub base_url: Option<String>, // Override for OpenAI-compatible endpoints
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[serde(rename = "openai")]
    OpenAI,
    Anthropic,
    #[serde(rename = "openrouter")]
    OpenRouter,
    Moonshot,
    Gemini,
    Deepseek,
    Groq,
    Baseten,
    Ollama,
    #[serde(rename = "openai_compatible", alias = "custom")]
    OpenAICompatible,
    #[serde(rename = "anthropic_compatible")]
    AnthropicCompatible,
}

impl ProviderKind {
    pub fn display_name(self) -> &'static str {
        match self {
            ProviderKind::OpenAI => "OPENAI",
            ProviderKind::Anthropic => "ANTHROPIC",
            ProviderKind::OpenRouter => "OPENROUTER",
            ProviderKind::Moonshot => "MOONSHOT",
            ProviderKind::Gemini => "GEMINI",
            ProviderKind::Deepseek => "DEEPSEEK",
            ProviderKind::Groq => "GROQ",
            ProviderKind::Baseten => "BASETEN",
            ProviderKind::Ollama => "OLLAMA",
            ProviderKind::OpenAICompatible => "OPENAI-COMPATIBLE",
            ProviderKind::AnthropicCompatible => "ANTHROPIC-COMPATIBLE",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::OpenAI => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            ProviderKind::OpenRouter => "https://openrouter.ai/api/v1",
            ProviderKind::Moonshot => "https://api.moonshot.ai/v1",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            ProviderKind::Deepseek => "https://api.deepseek.com/v1",
            ProviderKind::Groq => "https://api.groq.com/openai/v1",
            ProviderKind::Baseten => "https://inference.baseten.co/v1",
            ProviderKind::Ollama => "http://localhost:11434/v1",
            ProviderKind::OpenAICompatible => "https://api.openai.com/v1",
            ProviderKind::AnthropicCompatible => "https://api.anthropic.com/v1",
        }
    }

    pub fn default_auth(self) -> ProviderAuth {
        match self {
            ProviderKind::Baseten => ProviderAuth::ApiKey,
            ProviderKind::Ollama => ProviderAuth::None,
            ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => ProviderAuth::XApiKey,
            _ => ProviderAuth::Bearer,
        }
    }

    pub fn default_models(self) -> Vec<String> {
        match self {
            ProviderKind::OpenAI => vec![
                "gpt-5-mini".to_string(),
                "gpt-5".to_string(),
                "gpt-4.1-mini".to_string(),
            ],
            ProviderKind::Anthropic => vec![
                "claude-sonnet-4-0".to_string(),
                "claude-3-5-sonnet-latest".to_string(),
            ],
            ProviderKind::OpenRouter => vec![
                "openai/gpt-4o-mini".to_string(),
                "anthropic/claude-3.5-sonnet".to_string(),
            ],
            ProviderKind::Moonshot => {
                vec!["moonshot-v1-8k".to_string(), "moonshot-v1-32k".to_string()]
            }
            ProviderKind::Gemini => vec![
                "gemini-2.5-flash-lite".to_string(),
                "gemini-2.5-flash".to_string(),
                "gemini-2.5-pro".to_string(),
            ],
            ProviderKind::Deepseek => {
                vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()]
            }
            ProviderKind::Groq => vec![
                "llama-3.3-70b-versatile".to_string(),
                "llama3-8b-8192".to_string(),
                "mixtral-8x7b-32768".to_string(),
            ],
            ProviderKind::Baseten => vec![
                "deepseek-ai/DeepSeek-V3-0324".to_string(),
                "meta-llama/Llama-3.3-70B-Instruct".to_string(),
            ],
            ProviderKind::Ollama => vec![
                "llama3.2".to_string(),
                "qwen2.5".to_string(),
                "gemma3".to_string(),
            ],
            ProviderKind::OpenAICompatible | ProviderKind::AnthropicCompatible => Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuth {
    #[default]
    Bearer,
    ApiKey,
    XApiKey,
    None,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub auth: ProviderAuth,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub models: Vec<String>,
}

fn default_enabled() -> bool {
    true
}

impl ProviderConfig {
    pub fn builtin(kind: ProviderKind, api_key: Option<String>) -> Self {
        Self {
            kind,
            name: Some(kind.display_name().to_string()),
            api_key,
            base_url: kind.default_base_url().to_string(),
            auth: kind.default_auth(),
            enabled: true,
            models: kind.default_models(),
        }
    }

    pub fn display_name(&self) -> String {
        self.name
            .as_ref()
            .filter(|n| !n.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.kind.display_name().to_string())
    }

    pub fn normalized(mut self) -> Self {
        if self.base_url.trim().is_empty() {
            self.base_url = self.kind.default_base_url().to_string();
        }

        self.base_url = self.base_url.trim().trim_end_matches('/').to_string();

        if self.models.is_empty() {
            self.models = self.kind.default_models();
        } else {
            self.models = self
                .models
                .into_iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
        }

        self.api_key = clean_optional(self.api_key.take());
        self.name = clean_optional(self.name.take());

        if self.auth == ProviderAuth::Bearer {
            self.auth = self.kind.default_auth();
        }

        self
    }

    pub fn is_configured(&self) -> bool {
        if !self.enabled {
            return false;
        }

        match self.auth {
            ProviderAuth::None => true,
            ProviderAuth::Bearer | ProviderAuth::ApiKey | ProviderAuth::XApiKey => self
                .api_key
                .as_ref()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelPreferences {
    #[serde(default = "default_router_model")]
    pub router_model: String,
    #[serde(default = "default_executor_model")]
    pub executor_model: String,
    #[serde(default)]
    pub router_fallback_models: Vec<String>,
    #[serde(default)]
    pub executor_fallback_models: Vec<String>,
    #[serde(default)]
    pub router_routes: Vec<ModelRoute>,
    #[serde(default)]
    pub executor_routes: Vec<ModelRoute>,
}

fn default_router_model() -> String {
    "gemini-2.5-flash-lite".to_string()
}

fn default_executor_model() -> String {
    "gemini-2.5-flash-lite".to_string()
}

impl Default for ModelPreferences {
    fn default() -> Self {
        Self {
            router_model: default_router_model(),
            executor_model: default_executor_model(),
            router_fallback_models: Vec::new(),
            executor_fallback_models: Vec::new(),
            router_routes: Vec::new(),
            executor_routes: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ModelRoute {
    pub provider: ProviderKind,
    pub model: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            api_keys: ApiKeys::default(),
            models: ModelPreferences::default(),
            theme: default_theme(),
        }
    }
}

impl Config {
    pub async fn load() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .context("Could not find config directory")?
            .join("dexter");

        let config_path = config_dir.join("config.toml");

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&config_path).await?;
        let config: Config = toml::from_str(&content)?;

        Ok(config)
    }

    pub async fn save(&self) -> Result<()> {
        let config_dir = dirs::config_dir()
            .context("Could not find config directory")?
            .join("dexter");

        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).await?;
        }

        let config_path = config_dir.join("config.toml");
        let content = toml::to_string_pretty(self)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut file = fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&config_path)
                .await?;
            file.write_all(content.as_bytes()).await?;
            file.flush().await?;
            fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).await?;
        }

        #[cfg(not(unix))]
        {
            fs::write(&config_path, content).await?;
        }

        Ok(())
    }

    pub fn has_keys(&self) -> bool {
        !self.configured_providers().is_empty()
    }

    pub fn effective_providers(&self) -> Vec<ProviderConfig> {
        let providers = if self.providers.is_empty() {
            self.legacy_providers()
        } else {
            self.providers.clone()
        };

        providers
            .into_iter()
            .map(ProviderConfig::normalized)
            .collect()
    }

    pub fn configured_providers(&self) -> Vec<ProviderConfig> {
        self.effective_providers()
            .into_iter()
            .filter(|p| p.is_configured())
            .collect()
    }

    fn legacy_providers(&self) -> Vec<ProviderConfig> {
        let mut providers = Vec::new();

        if let Some(key) = clean_optional(self.api_keys.gemini.clone()) {
            providers.push(ProviderConfig::builtin(ProviderKind::Gemini, Some(key)));
        }

        if let Some(key) = clean_optional(self.api_keys.deepseek.clone()) {
            providers.push(ProviderConfig::builtin(ProviderKind::Deepseek, Some(key)));
        }

        if let Some(base_url) = clean_optional(self.api_keys.base_url.clone()) {
            let api_key = clean_optional(
                self.api_keys
                    .deepseek
                    .clone()
                    .or(self.api_keys.gemini.clone()),
            );

            providers.insert(
                0,
                ProviderConfig {
                    kind: ProviderKind::OpenAICompatible,
                    name: Some("Legacy Custom Endpoint".to_string()),
                    api_key: api_key.clone(),
                    base_url,
                    auth: if api_key.is_some() {
                        ProviderAuth::Bearer
                    } else {
                        ProviderAuth::None
                    },
                    enabled: true,
                    models: Vec::new(),
                },
            );
        }

        providers
    }
}

fn clean_optional(input: Option<String>) -> Option<String> {
    input.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

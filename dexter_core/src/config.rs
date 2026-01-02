use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub api_keys: ApiKeys,
    pub models: ModelPreferences,
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String {
    "auto".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApiKeys {
    pub gemini: Option<String>,
    pub deepseek: Option<String>,
    pub base_url: Option<String>, // Override for OpenAI-compatible endpoints
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ModelPreferences {
    pub router_model: String,
    pub executor_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_keys: ApiKeys {
                gemini: None,
                deepseek: None,
                base_url: None,
            },
            models: ModelPreferences {
                router_model: "gemini-2.5-flash-lite".to_string(), // Default safe choice
                executor_model: "gemini-2.5-flash-lite".to_string(),
            },
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

        fs::write(config_path, content).await?;
        Ok(())
    }

    pub fn has_keys(&self) -> bool {
        self.api_keys.gemini.is_some() || self.api_keys.deepseek.is_some()
    }
}

use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct LlmClient {
    http_client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
}

impl LlmClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
            base_url,
            model,
        }
    }

    pub async fn completion(&self, system_prompt: &str, user_input: &str) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            ChatMessage {
                role: "user".to_string(),
                content: user_input.to_string(),
            },
        ];

        let request_body = ChatRequest {
            model: self.model.clone(),
            messages,
            temperature: 0.1, // Deterministic output
        };

        let response = self.http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        if !response.status().is_success() {
            let error_text = response.text().await?;
            return Err(anyhow!("LLM API Error: {}", error_text));
        }

        let chat_response: ChatResponse = response.json().await?;
        
        chat_response.choices.first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| anyhow!("No content in LLM response"))
    }
}

#[async_trait::async_trait]
impl dexter_plugins::LlmBridge for LlmClient {
    async fn chat(&self, system: &str, user: &str) -> Result<String> {
        self.completion(system, user).await
    }
}

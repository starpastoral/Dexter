use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

const DEFAULT_CACHE_CAPACITY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    Normal,
    Bypass,
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    http_client: Client,
    api_key: String,
    base_url: String,
    model: String,
    cache: Arc<RwLock<HashMap<String, String>>>,
    cache_capacity: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: Option<ChatMessage>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct ModelListData {
    models: Option<Vec<Model>>, // Gemini style
    data: Option<Vec<Model>>,   // OpenAI/DeepSeek style
}

#[derive(Debug, Deserialize)]
struct Model {
    id: Option<String>,   // OpenAI style
    name: Option<String>, // Gemini style
}

impl LlmClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            http_client: Client::new(),
            api_key,
            base_url,
            model,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    pub async fn completion(&self, system_prompt: &str, user_input: &str) -> Result<String> {
        self.completion_with_policy(system_prompt, user_input, CachePolicy::Normal)
            .await
    }

    pub async fn completion_with_policy(
        &self,
        system_prompt: &str,
        user_input: &str,
        cache_policy: CachePolicy,
    ) -> Result<String> {
        let is_gemini = self.base_url.contains("generativelanguage.googleapis.com");

        // If it's Gemini, we often get better results or avoid safety refusals by
        // putting the "System" instructions at the start of the User message.
        let messages = if is_gemini {
            vec![ChatMessage {
                role: "user".to_string(),
                content: format!("{}\n\n### USER INPUT:\n{}", system_prompt, user_input),
            }]
        } else {
            vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user_input.to_string(),
                },
            ]
        };

        self.execute_completion(messages, cache_policy).await
    }

    async fn execute_completion(
        &self,
        messages: Vec<ChatMessage>,
        cache_policy: CachePolicy,
    ) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let cache_key = self.build_cache_key(&messages, 0.1)?;

        if cache_policy == CachePolicy::Normal {
            if let Some(cached) = self.cache.read().await.get(&cache_key).cloned() {
                return Ok(cached);
            }
        }

        let request_body = ChatRequest {
            model: self.model.clone(),
            messages,
            temperature: 0.1,
        };

        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            return Err(anyhow!("LLM API Error (Status {}): {}", status, text));
        }

        let chat_response: ChatResponse = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "Failed to parse LLM response: {} | Raw response: {}",
                e,
                text
            )
        })?;

        let first_choice = chat_response
            .choices
            .first()
            .ok_or_else(|| anyhow!("No choices returned from LLM"))?;

        if let Some(msg) = &first_choice.message {
            if msg.content.trim().is_empty() {
                // If we haven't already tried the single-message fallback, and we are not gemini (which already did it),
                // we could try it here. But for now, let's just report the detailed error.
                return Err(anyhow!(
                    "LLM returned empty content. Choice details: {:?}",
                    first_choice
                ));
            }
            let content = msg.content.clone();

            if cache_policy == CachePolicy::Normal {
                let mut cache = self.cache.write().await;
                if cache.len() >= self.cache_capacity {
                    if let Some(any_key) = cache.keys().next().cloned() {
                        cache.remove(&any_key);
                    }
                }
                cache.insert(cache_key, content.clone());
            }

            Ok(content)
        } else if let Some(reason) = first_choice.finish_reason.as_ref() {
            if reason.to_lowercase().contains("content_filter") {
                Err(anyhow!("Gemini content filter triggered: PROHIBITED_CONTENT. Try rephrasing your request."))
            } else {
                Err(anyhow!("LLM stopped execution. Reason: {}", reason))
            }
        } else {
            Err(anyhow!(
                "No content or reason in LLM response. Choice node: {:?}",
                first_choice
            ))
        }
    }

    fn build_cache_key(&self, messages: &[ChatMessage], temperature: f32) -> Result<String> {
        let payload = serde_json::to_string(messages)
            .map_err(|e| anyhow!("Failed to serialize messages for cache key: {}", e))?;
        Ok(format!(
            "{}|{}|{:.3}|{}",
            self.base_url, self.model, temperature, payload
        ))
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let is_gemini = self.base_url.contains("generativelanguage.googleapis.com");
        let url = if is_gemini {
            format!(
                "{}/models?key={}",
                self.base_url
                    .trim_end_matches('/')
                    .replace("/chat/completions", ""),
                self.api_key
            )
        } else {
            format!(
                "{}/models",
                self.base_url
                    .trim_end_matches('/')
                    .replace("/chat/completions", "")
            )
        };

        let mut request = self.http_client.get(&url);
        if !is_gemini {
            request = request.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            return Err(anyhow!(
                "Failed to list models (Status {}): {}",
                status,
                text
            ));
        }

        let list_data: ModelListData = serde_json::from_str(&text)
            .map_err(|e| anyhow!("Failed to parse model list: {} | Raw response: {}", e, text))?;

        let mut model_names = Vec::new();

        // Handle Gemini style
        if let Some(models) = list_data.models {
            for m in models {
                if let Some(name) = m.name {
                    // Extract ID from name like "models/gemini-pro"
                    let id = name.split('/').last().unwrap_or(&name).to_string();
                    model_names.push(id);
                }
            }
        }

        // Handle OpenAI/DeepSeek style
        if let Some(data) = list_data.data {
            for m in data {
                if let Some(id) = m.id {
                    model_names.push(id);
                }
            }
        }

        if model_names.is_empty() {
            return Err(anyhow!("No models found in provider response"));
        }

        // Sort and deduplicate
        model_names.sort();
        model_names.dedup();

        // Filter for chat-compatible names if needed, but for now return all
        Ok(model_names)
    }
}

#[async_trait::async_trait]
impl dexter_plugins::LlmBridge for LlmClient {
    async fn chat(&self, system: &str, user: &str) -> Result<String> {
        self.completion(system, user).await
    }
}

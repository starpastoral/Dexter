use crate::config::{ModelRoute, ProviderAuth, ProviderConfig, ProviderKind};
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

const DEFAULT_CACHE_CAPACITY: usize = 512;
const ROUTER_TEMPERATURE: f32 = 0.0;
const EXECUTOR_TEMPERATURE: f32 = 0.1;
const ROUTER_MAX_TOKENS: u32 = 512;
const EXECUTOR_MAX_TOKENS: u32 = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    Normal,
    Bypass,
}

#[derive(Debug, Clone)]
pub struct LlmClient {
    http_client: Client,
    targets: Vec<LlmTarget>,
    cache: Arc<RwLock<HashMap<String, String>>>,
    cache_capacity: usize,
}

#[derive(Debug, Clone)]
struct LlmTarget {
    provider_name: String,
    kind: ProviderKind,
    api_key: Option<String>,
    base_url: String,
    auth: ProviderAuth,
    model: String,
}

#[derive(Debug, Clone, Copy)]
struct CompletionParams {
    temperature: f32,
    max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
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

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    system: String,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Option<Vec<AnthropicContentBlock>>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelListData {
    models: Option<Vec<Model>>, // Gemini and Ollama style
    data: Option<Vec<Model>>,   // OpenAI style
}

#[derive(Debug, Deserialize)]
struct Model {
    id: Option<String>,   // OpenAI style
    name: Option<String>, // Gemini/Ollama style
}

impl LlmClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        let kind = infer_provider_kind(&base_url);
        let api_key = clean_optional(api_key);
        let auth = match kind {
            ProviderKind::Baseten => {
                if api_key.is_some() {
                    ProviderAuth::ApiKey
                } else {
                    ProviderAuth::None
                }
            }
            ProviderKind::Ollama => ProviderAuth::None,
            ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => {
                if api_key.is_some() {
                    ProviderAuth::XApiKey
                } else {
                    ProviderAuth::None
                }
            }
            _ => {
                if api_key.is_some() {
                    ProviderAuth::Bearer
                } else {
                    ProviderAuth::None
                }
            }
        };

        Self {
            http_client: Client::new(),
            targets: vec![LlmTarget {
                provider_name: kind.display_name().to_string(),
                kind,
                api_key,
                base_url: base_url.trim().trim_end_matches('/').to_string(),
                auth,
                model: model.trim().to_string(),
            }],
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    pub fn with_fallbacks(
        providers: Vec<ProviderConfig>,
        primary_model: String,
        fallback_models: Vec<String>,
    ) -> Self {
        Self::with_routes(providers, Vec::new(), primary_model, fallback_models)
    }

    pub fn with_routes(
        providers: Vec<ProviderConfig>,
        routes: Vec<ModelRoute>,
        primary_model: String,
        fallback_models: Vec<String>,
    ) -> Self {
        let provider_defs: Vec<ProviderConfig> = providers
            .into_iter()
            .map(ProviderConfig::normalized)
            .filter(|p| p.is_configured())
            .collect();

        let mut targets = Vec::new();
        let mut seen = HashSet::new();

        // First priority: explicit provider+model route order.
        if !routes.is_empty() {
            for route in routes {
                let model = route.model.trim();
                if model.is_empty() {
                    continue;
                }
                if let Some(provider) = provider_defs.iter().find(|p| p.kind == route.provider) {
                    let t = target_from_provider(provider, model.to_string());
                    if seen.insert(target_key(&t)) {
                        targets.push(t);
                    }
                }
            }
        }

        // If explicit routes are missing/empty, fallback to legacy model flow.
        if targets.is_empty() {
            targets = build_targets_from_legacy_models(
                &provider_defs,
                primary_model,
                fallback_models,
                &mut seen,
            );
        } else {
            // Add provider-local model candidates behind explicit routes.
            for p in &provider_defs {
                for m in &p.models {
                    let clean_model = m.trim();
                    if clean_model.is_empty() {
                        continue;
                    }
                    let t = target_from_provider(p, clean_model.to_string());
                    if seen.insert(target_key(&t)) {
                        targets.push(t);
                    }
                }
            }
        }

        // Last resort for runtime resilience.
        if targets.is_empty() {
            let fallback_provider = ProviderConfig::builtin(ProviderKind::Ollama, None);
            targets.push(target_from_provider(
                &fallback_provider,
                "llama3.2".to_string(),
            ));
        }

        Self {
            http_client: Client::new(),
            targets,
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }
}

fn build_targets_from_legacy_models(
    provider_defs: &[ProviderConfig],
    primary_model: String,
    fallback_models: Vec<String>,
    seen: &mut HashSet<String>,
) -> Vec<LlmTarget> {
    let mut targets = Vec::new();
    let mut global_models = Vec::new();
    if let Some(m) = clean_optional(primary_model) {
        global_models.push(m);
    }
    for m in fallback_models {
        if let Some(clean) = clean_optional(m) {
            if !global_models.iter().any(|x| x == &clean) {
                global_models.push(clean);
            }
        }
    }

    // First pass: global model preference across providers.
    for model in &global_models {
        for p in provider_defs {
            let t = target_from_provider(p, model.clone());
            if seen.insert(target_key(&t)) {
                targets.push(t);
            }
        }
    }

    // Second pass: provider-specific model lists as extra fallbacks.
    for p in provider_defs {
        for m in &p.models {
            let clean_model = m.trim();
            if clean_model.is_empty() {
                continue;
            }
            if global_models.iter().any(|gm| gm == clean_model) {
                continue;
            }
            let t = target_from_provider(p, clean_model.to_string());
            if seen.insert(target_key(&t)) {
                targets.push(t);
            }
        }
    }

    targets
}

impl LlmClient {
    pub async fn completion(&self, system_prompt: &str, user_input: &str) -> Result<String> {
        self.completion_with_policy_and_params(
            system_prompt,
            user_input,
            CachePolicy::Normal,
            router_completion_params(),
        )
        .await
    }

    pub async fn completion_with_policy(
        &self,
        system_prompt: &str,
        user_input: &str,
        cache_policy: CachePolicy,
    ) -> Result<String> {
        self.completion_with_policy_and_params(
            system_prompt,
            user_input,
            cache_policy,
            executor_completion_params(),
        )
        .await
    }

    async fn completion_with_policy_and_params(
        &self,
        system_prompt: &str,
        user_input: &str,
        cache_policy: CachePolicy,
        params: CompletionParams,
    ) -> Result<String> {
        let mut errors = Vec::new();
        for target in &self.targets {
            match self
                .execute_completion_for_target(
                    target,
                    system_prompt,
                    user_input,
                    cache_policy,
                    params,
                )
                .await
            {
                Ok(content) => return Ok(content),
                Err(e) => errors.push(format!(
                    "- [{} | {}] {}",
                    target.provider_name, target.model, e
                )),
            }
        }

        Err(anyhow!(
            "All configured providers/models failed:\n{}",
            errors.join("\n")
        ))
    }

    async fn execute_completion_for_target(
        &self,
        target: &LlmTarget,
        system_prompt: &str,
        user_input: &str,
        cache_policy: CachePolicy,
        params: CompletionParams,
    ) -> Result<String> {
        if is_anthropic_target(target) {
            self.execute_anthropic_completion_for_target(
                target,
                system_prompt,
                user_input,
                cache_policy,
                params,
            )
            .await
        } else {
            self.execute_openai_completion_for_target(
                target,
                system_prompt,
                user_input,
                cache_policy,
                params,
            )
            .await
        }
    }

    async fn execute_openai_completion_for_target(
        &self,
        target: &LlmTarget,
        system_prompt: &str,
        user_input: &str,
        cache_policy: CachePolicy,
        params: CompletionParams,
    ) -> Result<String> {
        let url = format!("{}/chat/completions", target.base_url.trim_end_matches('/'));
        let messages = build_openai_messages(target, system_prompt, user_input);
        let cache_key =
            self.build_cache_key(target, &messages, params.temperature, params.max_tokens)?;

        if cache_policy == CachePolicy::Normal {
            if let Some(cached) = self.cache.read().await.get(&cache_key).cloned() {
                return Ok(cached);
            }
        }

        let mut request_body = ChatRequest {
            model: target.model.clone(),
            messages,
            temperature: params.temperature,
            max_tokens: params.max_tokens,
        };

        let mut request = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json");
        request = apply_auth_header(request, target)?;
        let response = request.json(&request_body).send().await?;

        let mut status = response.status();
        let mut text = response.text().await?;

        if !status.is_success()
            && request_body.max_tokens.is_some()
            && likely_rejects_max_tokens(status.as_u16(), &text)
        {
            request_body.max_tokens = None;
            let mut retry = self
                .http_client
                .post(&url)
                .header("Content-Type", "application/json");
            retry = apply_auth_header(retry, target)?;
            let retry_response = retry.json(&request_body).send().await?;
            status = retry_response.status();
            text = retry_response.text().await?;
        }

        if !status.is_success() {
            let lower = text.to_lowercase();
            let hint = if status.as_u16() == 429
                || lower.contains("rate limit")
                || lower.contains("quota")
            {
                " (quota/rate-limit, trying fallback)"
            } else if lower.contains("content_filter")
                || lower.contains("safety")
                || lower.contains("blocked")
            {
                " (content policy block, trying fallback)"
            } else {
                ""
            };
            return Err(anyhow!(
                "LLM API Error (Status {}): {}{}",
                status,
                truncate_error(&text),
                hint
            ));
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
                Err(anyhow!(
                    "content filter triggered by current provider/model; trying fallback"
                ))
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

    async fn execute_anthropic_completion_for_target(
        &self,
        target: &LlmTarget,
        system_prompt: &str,
        user_input: &str,
        cache_policy: CachePolicy,
        params: CompletionParams,
    ) -> Result<String> {
        let url = format!("{}/messages", target.base_url.trim_end_matches('/'));
        let max_tokens = params.max_tokens.unwrap_or(EXECUTOR_MAX_TOKENS);
        let cache_key = self.build_anthropic_cache_key(
            target,
            system_prompt,
            user_input,
            params.temperature,
            max_tokens,
        )?;

        if cache_policy == CachePolicy::Normal {
            if let Some(cached) = self.cache.read().await.get(&cache_key).cloned() {
                return Ok(cached);
            }
        }

        let request_body = AnthropicRequest {
            model: target.model.clone(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: user_input.to_string(),
            }],
            system: system_prompt.to_string(),
            temperature: params.temperature,
            max_tokens,
        };

        let mut request = self
            .http_client
            .post(&url)
            .header("Content-Type", "application/json");
        request = apply_auth_header(request, target)?;
        let response = request.json(&request_body).send().await?;

        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            let lower = text.to_lowercase();
            let hint = if status.as_u16() == 429
                || lower.contains("rate limit")
                || lower.contains("quota")
            {
                " (quota/rate-limit, trying fallback)"
            } else if lower.contains("content_filter")
                || lower.contains("safety")
                || lower.contains("blocked")
            {
                " (content policy block, trying fallback)"
            } else {
                ""
            };
            return Err(anyhow!(
                "LLM API Error (Status {}): {}{}",
                status,
                truncate_error(&text),
                hint
            ));
        }

        let parsed: AnthropicResponse = serde_json::from_str(&text).map_err(|e| {
            anyhow!(
                "Failed to parse LLM response: {} | Raw response: {}",
                e,
                text
            )
        })?;

        let mut chunks = Vec::new();
        if let Some(content) = parsed.content {
            for block in content {
                if block.kind.as_deref() == Some("text") {
                    if let Some(text) = block.text {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            chunks.push(trimmed.to_string());
                        }
                    }
                }
            }
        }

        if chunks.is_empty() {
            if let Some(reason) = parsed.stop_reason {
                if reason.to_lowercase().contains("safety")
                    || reason.to_lowercase().contains("content_filter")
                {
                    return Err(anyhow!(
                        "content filter triggered by current provider/model; trying fallback"
                    ));
                }
                return Err(anyhow!("LLM stopped execution. Reason: {}", reason));
            }
            return Err(anyhow!("No text content in Anthropic response"));
        }

        let content = chunks.join("\n\n");

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
    }

    fn build_cache_key(
        &self,
        target: &LlmTarget,
        messages: &[ChatMessage],
        temperature: f32,
        max_tokens: Option<u32>,
    ) -> Result<String> {
        let payload = serde_json::json!({
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        let payload = serde_json::to_string(&payload)
            .map_err(|e| anyhow!("Failed to serialize messages for cache key: {}", e))?;
        Ok(format!(
            "{}|{}|{}|{}",
            target.provider_name, target.base_url, target.model, payload
        ))
    }

    fn build_anthropic_cache_key(
        &self,
        target: &LlmTarget,
        system_prompt: &str,
        user_input: &str,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<String> {
        let payload = serde_json::json!({
            "system": system_prompt,
            "user": user_input,
            "temperature": temperature,
            "max_tokens": max_tokens,
        });
        Ok(format!(
            "{}|{}|{}|anthropic|{}",
            target.provider_name, target.base_url, target.model, payload
        ))
    }

    pub async fn list_models(&self) -> Result<Vec<String>> {
        let mut all = Vec::new();
        let mut errors = Vec::new();
        let mut visited = HashSet::new();

        for target in &self.targets {
            let key = format!(
                "{}|{}|{:?}|{}",
                target.provider_name,
                target.base_url,
                target.auth,
                target.api_key.clone().unwrap_or_default()
            );
            if !visited.insert(key) {
                continue;
            }

            match self.fetch_models_for_target(target).await {
                Ok(models) => all.extend(models),
                Err(e) => {
                    errors.push(format!(
                        "{} @ {}: {}",
                        target.provider_name, target.base_url, e
                    ));
                }
            }
        }

        all.sort();
        all.dedup();

        if all.is_empty() {
            return Err(anyhow!(
                "Failed to list models from configured providers:\n{}",
                errors.join("\n")
            ));
        }

        Ok(all)
    }

    async fn fetch_models_for_target(&self, target: &LlmTarget) -> Result<Vec<String>> {
        // Gemini exposes model discovery on its non-openai endpoint.
        if is_gemini_target(target) {
            let key = target
                .api_key
                .clone()
                .ok_or_else(|| anyhow!("Gemini model listing requires API key"))?;
            let mut base = target.base_url.trim_end_matches('/').to_string();
            if base.ends_with("/openai") {
                base = base.trim_end_matches("/openai").to_string();
            }
            let url = format!("{}/models?key={}", base, key);
            let response = self.http_client.get(&url).send().await?;
            let status = response.status();
            let text = response.text().await?;
            if !status.is_success() {
                return Err(anyhow!(
                    "status {} from {}: {}",
                    status,
                    url,
                    truncate_error(&text)
                ));
            }
            return parse_model_list(&text);
        }

        // OpenAI-compatible path.
        let url = format!("{}/models", target.base_url.trim_end_matches('/'));
        let mut request = self.http_client.get(&url);
        request = apply_auth_header(request, target)?;
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if status.is_success() {
            return parse_model_list(&text);
        }

        // Ollama fallback to native endpoint.
        if target.kind == ProviderKind::Ollama {
            let base = target.base_url.trim_end_matches('/');
            let root = base.strip_suffix("/v1").unwrap_or(base);
            let fallback_url = format!("{}/api/models", root);
            let response = self.http_client.get(&fallback_url).send().await?;
            let fallback_status = response.status();
            let fallback_text = response.text().await?;
            if fallback_status.is_success() {
                return parse_model_list(&fallback_text);
            }
            return Err(anyhow!(
                "status {} from {}, then status {} from {}",
                status,
                url,
                fallback_status,
                fallback_url
            ));
        }

        Err(anyhow!(
            "status {} from {}: {}",
            status,
            url,
            truncate_error(&text)
        ))
    }
}

fn parse_model_list(text: &str) -> Result<Vec<String>> {
    let list_data: ModelListData = serde_json::from_str(text)
        .map_err(|e| anyhow!("Failed to parse model list: {} | Raw response: {}", e, text))?;

    let mut model_names = Vec::new();

    if let Some(models) = list_data.models {
        for m in models {
            if let Some(name) = m.name {
                let id = name.strip_prefix("models/").unwrap_or(&name).to_string();
                model_names.push(id);
            }
        }
    }

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

    model_names.sort();
    model_names.dedup();
    Ok(model_names)
}

fn router_completion_params() -> CompletionParams {
    CompletionParams {
        temperature: ROUTER_TEMPERATURE,
        max_tokens: Some(ROUTER_MAX_TOKENS),
    }
}

fn executor_completion_params() -> CompletionParams {
    CompletionParams {
        temperature: EXECUTOR_TEMPERATURE,
        max_tokens: Some(EXECUTOR_MAX_TOKENS),
    }
}

fn likely_rejects_max_tokens(status_code: u16, body: &str) -> bool {
    if status_code != 400 {
        return false;
    }
    let lower = body.to_lowercase();
    lower.contains("max_tokens")
        || lower.contains("max completion tokens")
        || lower.contains("unknown field")
        || lower.contains("unrecognized field")
}

fn build_openai_messages(
    target: &LlmTarget,
    system_prompt: &str,
    user_input: &str,
) -> Vec<ChatMessage> {
    if is_gemini_target(target) {
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
    }
}

fn is_gemini_target(target: &LlmTarget) -> bool {
    target.kind == ProviderKind::Gemini
        || target
            .base_url
            .contains("generativelanguage.googleapis.com")
}

fn is_anthropic_target(target: &LlmTarget) -> bool {
    matches!(
        target.kind,
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible
    ) || target.base_url.contains("anthropic.com")
}

fn apply_auth_header(
    request: reqwest::RequestBuilder,
    target: &LlmTarget,
) -> Result<reqwest::RequestBuilder> {
    match target.auth {
        ProviderAuth::None => Ok(request),
        ProviderAuth::Bearer => {
            let key = target
                .api_key
                .as_ref()
                .ok_or_else(|| anyhow!("Missing API key for provider {}", target.provider_name))?;
            Ok(request.header("Authorization", format!("Bearer {}", key)))
        }
        ProviderAuth::ApiKey => {
            let key = target
                .api_key
                .as_ref()
                .ok_or_else(|| anyhow!("Missing API key for provider {}", target.provider_name))?;
            Ok(request.header("Authorization", format!("Api-Key {}", key)))
        }
        ProviderAuth::XApiKey => {
            let key = target
                .api_key
                .as_ref()
                .ok_or_else(|| anyhow!("Missing API key for provider {}", target.provider_name))?;
            Ok(request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"))
        }
    }
}

fn infer_provider_kind(base_url: &str) -> ProviderKind {
    let lower = base_url.to_lowercase();
    if lower.contains("api.openai.com") {
        ProviderKind::OpenAI
    } else if lower.contains("api.anthropic.com") {
        ProviderKind::Anthropic
    } else if lower.contains("openrouter.ai") {
        ProviderKind::OpenRouter
    } else if lower.contains("moonshot.ai") {
        ProviderKind::Moonshot
    } else if lower.contains("generativelanguage.googleapis.com")
        || lower.contains("googleapis.com")
    {
        ProviderKind::Gemini
    } else if lower.contains("deepseek.com") {
        ProviderKind::Deepseek
    } else if lower.contains("groq.com") {
        ProviderKind::Groq
    } else if lower.contains("baseten.co") {
        ProviderKind::Baseten
    } else if lower.contains("localhost:11434") || lower.contains("127.0.0.1:11434") {
        ProviderKind::Ollama
    } else if lower.contains("anthropic") {
        ProviderKind::AnthropicCompatible
    } else {
        ProviderKind::OpenAICompatible
    }
}

fn target_from_provider(provider: &ProviderConfig, model: String) -> LlmTarget {
    LlmTarget {
        provider_name: provider.display_name(),
        kind: provider.kind,
        api_key: provider.api_key.clone(),
        base_url: provider.base_url.trim().trim_end_matches('/').to_string(),
        auth: provider.auth,
        model,
    }
}

fn target_key(target: &LlmTarget) -> String {
    format!(
        "{}|{}|{:?}|{}|{}",
        target.provider_name,
        target.base_url,
        target.auth,
        target.api_key.clone().unwrap_or_default(),
        target.model
    )
}

fn truncate_error(text: &str) -> String {
    const MAX: usize = 320;
    if text.len() > MAX {
        format!("{}...", &text[..MAX])
    } else {
        text.to_string()
    }
}

fn clean_optional(input: String) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[async_trait::async_trait]
impl dexter_plugins::LlmBridge for LlmClient {
    async fn chat(&self, system: &str, user: &str) -> Result<String> {
        self.completion_with_policy(system, user, CachePolicy::Normal)
            .await
    }
}

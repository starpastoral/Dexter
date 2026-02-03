use crate::context::FileContext;
use crate::llm::LlmClient;
use anyhow::{anyhow, Result};
use dexter_plugins::Plugin;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct RouterResponse {
    plugin_name: String,
    confidence: f32,
    reasoning: String,
}

pub struct Router {
    llm_client: LlmClient,
}

impl Router {
    pub fn new(llm_client: LlmClient) -> Self {
        Self { llm_client }
    }

    pub async fn route(
        &self,
        user_input: &str,
        context: &FileContext,
        plugins: &[std::sync::Arc<dyn Plugin>],
    ) -> Result<String> {
        let plugin_list: Vec<String> = plugins
            .iter()
            .map(|p| format!("- {}: {}", p.name(), p.get_doc_for_router()))
            .collect();

        let context_str = if let Some(summary) = &context.summary {
            summary.clone()
        } else {
            context.files.join(", ")
        };

        let system_prompt = format!(
            r#"You are the Router Agent for Dexter.
Your job is to map User Intent to the best available Plugin.

### USER INTENT:
{}

### Available Plugins:
{}

### Context:
{}

Output Format: JSON
{{
  "plugin_name": "exact_name_from_list",
  "confidence": 0.0_to_1.0,
  "reasoning": "why this plugin"
}}
"#,
            user_input,
            plugin_list.join("\n"),
            context_str
        );

        let response = self
            .llm_client
            .completion(
                &system_prompt,
                "Which plugin should be used for this intent?",
            )
            .await?;

        // Parse JSON from response (naive parsing, ensuring json block extraction might be needed in prod)
        // For now assuming LLM follows instruction purely or we use a strict parser later.
        // We might need to strip markdown code blocks ```json ... ```
        let clean_json = response
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let router_resp: RouterResponse = serde_json::from_str(clean_json)
            .map_err(|e| anyhow!("Failed to parse Router JSON: {}. Response: {}", e, response))?;

        if router_resp.confidence < 0.7 {
            return Err(anyhow!(
                "Low confidence ({:.2}): {}",
                router_resp.confidence,
                router_resp.reasoning
            ));
        }

        Ok(router_resp.plugin_name)
    }
}

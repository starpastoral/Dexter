use crate::llm::LlmClient;
use crate::context::FileContext;
use dexter_plugins::Plugin;
use anyhow::{Result, anyhow};
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

    pub async fn route(&self, user_input: &str, context: &FileContext, plugins: &[Box<dyn Plugin>]) -> Result<String> {
        let plugin_list: Vec<String> = plugins.iter()
            .map(|p| format!("- {}: {}", p.name(), p.get_doc_for_router()))
            .collect();

        let context_str = if let Some(summary) = &context.summary {
            summary.clone()
        } else {
            context.files.join(", ")
        };

        let system_prompt = format!(
            r#"You are the Router Agent for Dexter, a CLI tool copilot.
Your job is to map User Intent to the best available Plugin.

Available Plugins:
{}

Current Working Directory Context:
{}

Output Format: JSON
{{
  "plugin_name": "exact_name_from_list", // or "system" if undefined
  "confidence": 0.0_to_1.0,
  "reasoning": "short explanation"
}}
"#,
            plugin_list.join("\n"),
            context_str
        );

        let response = self.llm_client.completion(&system_prompt, user_input).await?;
        
        // Parse JSON from response (naive parsing, ensuring json block extraction might be needed in prod)
        // For now assuming LLM follows instruction purely or we use a strict parser later.
        // We might need to strip markdown code blocks ```json ... ```
        let clean_json = response.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
        
        let router_resp: RouterResponse = serde_json::from_str(clean_json)
            .map_err(|e| anyhow!("Failed to parse Router JSON: {}. Response: {}", e, response))?;

        if router_resp.confidence < 0.7 {
            return Err(anyhow!("Low confidence ({:.2}): {}", router_resp.confidence, router_resp.reasoning));
        }

        Ok(router_resp.plugin_name)
    }
}

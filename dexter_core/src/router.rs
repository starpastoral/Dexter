use crate::context::FileContext;
use crate::llm::LlmClient;
use anyhow::{anyhow, Result};
use dexter_plugins::Plugin;
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub enum ClarifySource {
    Rule,
    LLM,
}

#[derive(Debug, Clone)]
pub struct ClarifyOption {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub resolved_intent: String,
}

#[derive(Debug, Clone)]
pub enum RouteOutcome {
    Selected {
        plugin: String,
        confidence: f32,
        reasoning: String,
    },
    Unsupported {
        reason: String,
    },
    Clarify {
        question: String,
        options: Vec<ClarifyOption>,
        source: ClarifySource,
    },
}

#[derive(Debug, Deserialize)]
struct RouterResponse {
    plugin_name: Option<String>,
    confidence: Option<f32>,
    reasoning: Option<String>,
    clarify: Option<RouterClarify>,
}

#[derive(Debug, Deserialize)]
struct RouterClarify {
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Vec<RouterClarifyOption>,
}

#[derive(Debug, Deserialize)]
struct RouterClarifyOption {
    id: Option<String>,
    label: String,
    detail: String,
    resolved_intent: String,
}

pub struct Router {
    llm_client: LlmClient,
}

impl Router {
    pub fn new(llm_client: LlmClient) -> Self {
        Self { llm_client }
    }

    pub fn llm_client(&self) -> &LlmClient {
        &self.llm_client
    }

    pub async fn route(
        &self,
        user_input: &str,
        context: &FileContext,
        plugins: &[std::sync::Arc<dyn Plugin>],
    ) -> Result<RouteOutcome> {
        if let Some(outcome) = rule_precheck(user_input) {
            return Ok(outcome);
        }

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
  "plugin_name": "exact_name_from_list or 'none'",
  "confidence": 0.0_to_1.0,
  "reasoning": "why this plugin",
  "clarify": {{
    "question": "only if the intent is ambiguous or needs user choice",
    "options": [
      {{
        "id": "stable_id",
        "label": "short button label",
        "detail": "short explanation",
        "resolved_intent": "a single clarified instruction"
      }}
    ]
  }}
}}

Rules:
- If no plugin fits, set plugin_name to "none" and confidence to 0.0.
- Only include "clarify" when multiple plausible interpretations exist.
- If you include "clarify", set plugin_name to "none".
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

        if let Some(clarify) = router_resp.clarify {
            if let Some(outcome) = validate_llm_clarify(clarify) {
                return Ok(outcome);
            }
        }

        let plugin_name = router_resp
            .plugin_name
            .unwrap_or_else(|| "none".to_string());
        let confidence = router_resp.confidence.unwrap_or(0.0);
        let reasoning = router_resp.reasoning.unwrap_or_default();

        let plugin_set: HashSet<String> = plugins.iter().map(|p| p.name().to_string()).collect();

        if plugin_name == "none" || confidence < 0.7 || !plugin_set.contains(&plugin_name) {
            return Ok(RouteOutcome::Unsupported {
                reason: if reasoning.is_empty() {
                    "No suitable plugin found for this request.".to_string()
                } else {
                    reasoning
                },
            });
        }

        Ok(RouteOutcome::Selected {
            plugin: plugin_name,
            confidence,
            reasoning,
        })
    }
}

fn rule_precheck(user_input: &str) -> Option<RouteOutcome> {
    let lower = user_input.to_lowercase();

    let exts = extract_extensions_in_order(&lower);
    let (has_media, has_doc) = classify_exts(&exts);

    if has_media && has_doc {
        return Some(RouteOutcome::Unsupported {
            reason:
                "Audio/video formats can’t be converted into document formats (e.g., mp3 → html)."
                    .to_string(),
        });
    }

    let rename_keywords = [
        "rename",
        "renaming",
        "change extension",
        "rename to",
        "rename as",
        "改名",
        "重命名",
    ];
    let convert_keywords = ["convert", "conversion", "transcode", "转成", "转换"];
    let has_rename = contains_any(&lower, &rename_keywords);
    let has_convert = contains_any(&lower, &convert_keywords) || lower.contains("->");

    if has_rename && exts.len() >= 2 && (has_convert || has_media) {
        let (src, dst) = pick_src_dst(&exts);
        let (src_label, dst_label) = match (src, dst) {
            (Some(a), Some(b)) => (a, b),
            _ => ("source", "target"),
        };
        let question =
            "Do you want to rename the file extension or convert the actual media format?"
                .to_string();
        let options = vec![
            ClarifyOption {
                id: "rename_only".to_string(),
                label: "Rename only".to_string(),
                detail: "Only change the filename extension (no media conversion).".to_string(),
                resolved_intent: format!(
                    "rename file extension from {} to {} (rename only, no conversion)",
                    src_label, dst_label
                ),
            },
            ClarifyOption {
                id: "convert_format".to_string(),
                label: "Convert format".to_string(),
                detail: "Transcode the media to the new format.".to_string(),
                resolved_intent: format!(
                    "convert media format from {} to {}",
                    src_label, dst_label
                ),
            },
        ];
        return Some(RouteOutcome::Clarify {
            question,
            options,
            source: ClarifySource::Rule,
        });
    }

    None
}

fn validate_llm_clarify(clarify: RouterClarify) -> Option<RouteOutcome> {
    let question = clarify.question?.trim().to_string();
    if question.is_empty() || question.len() > 120 {
        return None;
    }

    if clarify.options.len() < 2 || clarify.options.len() > 4 {
        return None;
    }

    let mut options: Vec<ClarifyOption> = Vec::new();
    for (i, opt) in clarify.options.into_iter().enumerate() {
        if opt.label.trim().is_empty()
            || opt.detail.trim().is_empty()
            || opt.resolved_intent.trim().is_empty()
        {
            return None;
        }
        if opt.label.len() > 24 || opt.detail.len() > 80 {
            return None;
        }
        if has_forbidden_chars(&opt.resolved_intent) {
            return None;
        }
        let id = opt.id.unwrap_or_else(|| format!("option_{}", i + 1));
        options.push(ClarifyOption {
            id,
            label: opt.label,
            detail: opt.detail,
            resolved_intent: opt.resolved_intent,
        });
    }

    Some(RouteOutcome::Clarify {
        question,
        options,
        source: ClarifySource::LLM,
    })
}

fn has_forbidden_chars(value: &str) -> bool {
    let banned = ["&&", "||", ";", "|", "`", "$("];
    banned.iter().any(|t| value.contains(t))
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| haystack.contains(n))
}

fn extract_extensions_in_order(input: &str) -> Vec<String> {
    let known_exts = known_extensions();
    let re = regex::Regex::new(r"(?i)[a-z0-9]{1,5}").unwrap();
    let mut out = Vec::new();
    for m in re.find_iter(input) {
        let token = m.as_str().to_lowercase();
        if known_exts.contains(token.as_str()) {
            if out.last().map(|s: &String| s == &token).unwrap_or(false) {
                continue;
            }
            out.push(token);
        }
    }
    out
}

fn classify_exts(exts: &[String]) -> (bool, bool) {
    let media = media_extensions();
    let docs = doc_extensions();
    let mut has_media = false;
    let mut has_doc = false;
    for e in exts {
        if media.contains(e.as_str()) {
            has_media = true;
        }
        if docs.contains(e.as_str()) {
            has_doc = true;
        }
    }
    (has_media, has_doc)
}

fn pick_src_dst(exts: &[String]) -> (Option<&str>, Option<&str>) {
    if exts.len() >= 2 {
        (Some(exts[0].as_str()), Some(exts[1].as_str()))
    } else if exts.len() == 1 {
        (Some(exts[0].as_str()), None)
    } else {
        (None, None)
    }
}

fn known_extensions() -> HashSet<&'static str> {
    let mut set = HashSet::new();
    for e in media_extensions() {
        set.insert(e);
    }
    for e in doc_extensions() {
        set.insert(e);
    }
    for e in image_extensions() {
        set.insert(e);
    }
    set
}

fn media_extensions() -> HashSet<&'static str> {
    [
        "mp3", "wav", "flac", "aac", "m4a", "ogg", "mp4", "mov", "mkv", "avi", "webm",
    ]
    .into_iter()
    .collect()
}

fn doc_extensions() -> HashSet<&'static str> {
    ["html", "htm", "pdf", "docx", "md", "txt", "rtf"]
        .into_iter()
        .collect()
}

fn image_extensions() -> HashSet<&'static str> {
    ["png", "jpg", "jpeg", "gif", "webp"].into_iter().collect()
}

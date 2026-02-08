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
        } else if context.files.is_empty() {
            "(no visible files in current directory)".to_string()
        } else {
            context
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| format!("{}. {}", i + 1, f))
                .collect::<Vec<_>>()
                .join("\n")
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
- Every clarify option must be a single operation only.
- Do NOT propose multi-step or chained operations inside one clarify option.
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

        let router_resp: RouterResponse = parse_router_response(&response)?;

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

fn parse_router_response(response: &str) -> Result<RouterResponse> {
    let clean_json = response
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    if let Ok(parsed) = serde_json::from_str::<RouterResponse>(clean_json) {
        return Ok(parsed);
    }

    if let Some(candidate) = extract_first_json_object(response) {
        if let Ok(parsed) = serde_json::from_str::<RouterResponse>(&candidate) {
            return Ok(parsed);
        }
    }

    Err(anyhow!(
        "Failed to parse Router JSON from model response. Raw response snippet: {}",
        truncate_router_error(response)
    ))
}

fn extract_first_json_object(input: &str) -> Option<String> {
    let mut start = None;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        return Some(input[s..=idx].to_string());
                    }
                }
            }
            _ => {}
        }
    }

    None
}

fn truncate_router_error(text: &str) -> String {
    const MAX: usize = 320;
    if text.len() > MAX {
        format!("{}...", &text[..MAX])
    } else {
        text.to_string()
    }
}

fn rule_precheck(user_input: &str) -> Option<RouteOutcome> {
    let lower = user_input.to_lowercase();
    let operation_intents = detect_operation_intents(&lower);

    // Guardrail: if user asks for chained mixed workflows, force single-operation clarify.
    if operation_intents.len() >= 2 && has_step_connector(&lower) {
        let options: Vec<ClarifyOption> = operation_intents
            .iter()
            .take(4)
            .map(|intent| clarify_option_for_intent(*intent))
            .collect();

        if options.len() >= 2 {
            return Some(RouteOutcome::Clarify {
                question:
                    "This request mixes multiple operations. Which single operation should Dexter run first?"
                        .to_string(),
                options,
                source: ClarifySource::Rule,
            });
        }
    }

    let exts = extract_extensions_in_order(&lower);
    let (has_media, has_doc) = classify_exts(&exts);

    if has_media && has_doc {
        return Some(RouteOutcome::Unsupported {
            reason:
                "Audio/video formats can’t be converted into document formats (e.g., mp3 → html)."
                    .to_string(),
        });
    }

    let has_rename = contains_any(&lower, RENAME_KEYWORDS);
    let has_convert = contains_any(&lower, CONVERT_KEYWORDS) || lower.contains("->");

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
        if looks_mixed_operation_intent(&opt.resolved_intent) {
            continue;
        }
        let id = opt.id.unwrap_or_else(|| format!("option_{}", i + 1));
        options.push(ClarifyOption {
            id,
            label: opt.label,
            detail: opt.detail,
            resolved_intent: opt.resolved_intent,
        });
    }

    if options.len() < 2 {
        return None;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OperationIntent {
    Rename,
    Ocr,
    Compress,
    Convert,
    ExtractAudio,
    Dedupe,
}

const RENAME_KEYWORDS: &[&str] = &[
    "rename",
    "renaming",
    "change extension",
    "rename to",
    "rename as",
    "改名",
    "重命名",
    "后缀",
];
const OCR_KEYWORDS: &[&str] = &[
    "ocr",
    "文字识别",
    "识别文字",
    "扫描文字",
    "扫描识别",
    "可搜索",
];
const COMPRESS_KEYWORDS: &[&str] = &["compress", "compression", "压缩", "减小体积"];
const CONVERT_KEYWORDS: &[&str] = &["convert", "conversion", "transcode", "转成", "转换"];
const EXTRACT_AUDIO_KEYWORDS: &[&str] = &[
    "extract audio",
    "audio extract",
    "提取音频",
    "抽取音频",
    "音频提取",
];
const DEDUPE_KEYWORDS: &[&str] = &[
    "duplicate",
    "duplicates",
    "dedupe",
    "jdupes",
    "去重",
    "重复文件",
];
const OPERATION_INTENT_KEYWORDS: &[(OperationIntent, &[&str])] = &[
    (OperationIntent::Rename, RENAME_KEYWORDS),
    (OperationIntent::Ocr, OCR_KEYWORDS),
    (OperationIntent::Compress, COMPRESS_KEYWORDS),
    (OperationIntent::Convert, CONVERT_KEYWORDS),
    (OperationIntent::ExtractAudio, EXTRACT_AUDIO_KEYWORDS),
    (OperationIntent::Dedupe, DEDUPE_KEYWORDS),
];

fn detect_operation_intents(lower: &str) -> Vec<OperationIntent> {
    let mut intents = Vec::new();

    for (intent, keywords) in OPERATION_INTENT_KEYWORDS {
        if contains_any(lower, keywords) {
            intents.push(*intent);
        }
    }

    intents
}

fn has_step_connector(lower: &str) -> bool {
    contains_any(
        lower,
        &[
            " then ",
            " and then ",
            " first ",
            " next ",
            " finally ",
            " followed by ",
            "后再",
            "然后",
            "接着",
            "最后",
            "并且",
            "并 ",
            "再",
            "先",
        ],
    )
}

fn clarify_option_for_intent(intent: OperationIntent) -> ClarifyOption {
    match intent {
        OperationIntent::Rename => ClarifyOption {
            id: "rename_only".to_string(),
            label: "Rename files".to_string(),
            detail: "Only rename filenames/patterns.".to_string(),
            resolved_intent:
                "Rename files only (no OCR, conversion, compression, or duplicate scan)."
                    .to_string(),
        },
        OperationIntent::Ocr => ClarifyOption {
            id: "ocr_only".to_string(),
            label: "OCR PDFs".to_string(),
            detail: "Only run OCR on PDF files.".to_string(),
            resolved_intent:
                "Run OCR on PDF files only (no compression, renaming, or other steps).".to_string(),
        },
        OperationIntent::Compress => ClarifyOption {
            id: "compress_only".to_string(),
            label: "Compress files".to_string(),
            detail: "Only compress files/media.".to_string(),
            resolved_intent:
                "Compress files only (no OCR, renaming, format conversion, or duplicate scan)."
                    .to_string(),
        },
        OperationIntent::Convert => ClarifyOption {
            id: "convert_only".to_string(),
            label: "Convert format".to_string(),
            detail: "Only convert file format.".to_string(),
            resolved_intent:
                "Convert file format only (no renaming, OCR, compression, or duplicate scan)."
                    .to_string(),
        },
        OperationIntent::ExtractAudio => ClarifyOption {
            id: "extract_audio".to_string(),
            label: "Extract audio".to_string(),
            detail: "Only extract audio from media.".to_string(),
            resolved_intent:
                "Extract audio only (no renaming, OCR, compression, or duplicate scan).".to_string(),
        },
        OperationIntent::Dedupe => ClarifyOption {
            id: "dedupe_only".to_string(),
            label: "Check duplicates".to_string(),
            detail: "Only scan duplicate files.".to_string(),
            resolved_intent:
                "Scan duplicate files only (no renaming, OCR, compression, or conversion)."
                    .to_string(),
        },
    }
}

fn looks_mixed_operation_intent(value: &str) -> bool {
    let lower = value.to_lowercase();
    if contains_any(
        &lower,
        &[
            "separate step",
            "separate operation",
            "break down",
            "one step at a time",
            "分步",
            "逐步",
            "分别",
        ],
    ) {
        return false;
    }

    detect_operation_intents(&lower).len() >= 2 && has_step_connector(&lower)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_router_response_accepts_markdown_wrapped_json() {
        let raw = r#"```json
{"plugin_name":"ffmpeg","confidence":0.92,"reasoning":"media task"}
```"#;
        let parsed = parse_router_response(raw).expect("should parse");
        assert_eq!(parsed.plugin_name.as_deref(), Some("ffmpeg"));
    }

    #[test]
    fn parse_router_response_extracts_json_from_noise() {
        let raw = r#"I think this is correct:
{"plugin_name":"f2","confidence":0.91,"reasoning":"rename task"}
Thanks!"#;
        let parsed = parse_router_response(raw).expect("should parse");
        assert_eq!(parsed.plugin_name.as_deref(), Some("f2"));
    }

    #[test]
    fn truncate_router_error_limits_output_size() {
        let long = "x".repeat(500);
        let truncated = truncate_router_error(&long);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= 323);
    }

    #[test]
    fn rule_precheck_mixed_workflow_yields_single_operation_clarify() {
        let input = "把 PDF 做 OCR 后再压缩，并把结果重命名加日期后缀";
        let outcome = rule_precheck(input).expect("should trigger clarify");
        match outcome {
            RouteOutcome::Clarify {
                source, options, ..
            } => {
                assert!(matches!(source, ClarifySource::Rule));
                assert!(options.len() >= 2);
                for opt in options {
                    assert!(!looks_mixed_operation_intent(&opt.resolved_intent));
                }
            }
            _ => panic!("expected clarify outcome"),
        }
    }

    #[test]
    fn validate_llm_clarify_filters_mixed_options() {
        let clarify = RouterClarify {
            question: Some("Which should I do first?".to_string()),
            options: vec![
                RouterClarifyOption {
                    id: Some("mixed".to_string()),
                    label: "OCR then compress".to_string(),
                    detail: "Do OCR and compression together".to_string(),
                    resolved_intent: "First do OCR then compress and then rename the file."
                        .to_string(),
                },
                RouterClarifyOption {
                    id: Some("ocr".to_string()),
                    label: "OCR only".to_string(),
                    detail: "Only OCR".to_string(),
                    resolved_intent: "Run OCR on PDF files only.".to_string(),
                },
                RouterClarifyOption {
                    id: Some("rename".to_string()),
                    label: "Rename only".to_string(),
                    detail: "Only rename".to_string(),
                    resolved_intent: "Rename files only.".to_string(),
                },
            ],
        };

        let out = validate_llm_clarify(clarify).expect("should keep non-mixed options");
        match out {
            RouteOutcome::Clarify { options, .. } => {
                assert_eq!(options.len(), 2);
                assert!(options.iter().all(|o| o.id != "mixed"));
            }
            _ => panic!("expected clarify"),
        }
    }
}

use regex::Regex;
use std::sync::OnceLock;

/// Redacts common secret patterns from logs/history while preserving
/// surrounding command structure for debugging.
pub fn redact_sensitive_text(input: &str) -> String {
    static COOKIES_RE: OnceLock<Regex> = OnceLock::new();
    static AUTH_BEARER_RE: OnceLock<Regex> = OnceLock::new();
    static AUTH_APIKEY_RE: OnceLock<Regex> = OnceLock::new();
    static QUERY_TOKEN_RE: OnceLock<Regex> = OnceLock::new();
    static KEY_LIKE_RE: OnceLock<Regex> = OnceLock::new();

    let cookies_re = COOKIES_RE
        .get_or_init(|| Regex::new(r#"(?i)(--cookies(?:=|\s+))("[^"]*"|'[^']*'|\S+)"#).unwrap());
    let auth_bearer_re = AUTH_BEARER_RE.get_or_init(|| {
        Regex::new(r#"(?i)(authorization\s*:\s*bearer\s+)([A-Za-z0-9._~+/=-]+)"#).unwrap()
    });
    let auth_apikey_re = AUTH_APIKEY_RE
        .get_or_init(|| Regex::new(r#"(?i)(x-api-key\s*:\s*)([A-Za-z0-9._~+/=-]+)"#).unwrap());
    let query_token_re = QUERY_TOKEN_RE.get_or_init(|| {
        Regex::new(r#"(?i)([?&](?:token|access_token|api_key|apikey|key)=)([^&\s"']+)"#).unwrap()
    });
    let key_like_re =
        KEY_LIKE_RE.get_or_init(|| Regex::new(r#"(?i)\b(?:sk|rk)-[A-Za-z0-9_-]{12,}\b"#).unwrap());

    let step1 = cookies_re.replace_all(input, "$1[REDACTED]").to_string();
    let step2 = auth_bearer_re
        .replace_all(&step1, "$1[REDACTED]")
        .to_string();
    let step3 = auth_apikey_re
        .replace_all(&step2, "$1[REDACTED]")
        .to_string();
    let step4 = query_token_re
        .replace_all(&step3, "$1[REDACTED]")
        .to_string();
    key_like_re.replace_all(&step4, "[REDACTED]").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_masks_common_secrets() {
        let raw = r#"Authorization: Bearer abc123token
x-api-key: supersecret
https://a.com/path?token=abc&x=1
yt-dlp --cookies cookies.txt "https://a.com"
sk-live-1234567890abcdef"#;

        let masked = redact_sensitive_text(raw);
        assert!(!masked.contains("abc123token"));
        assert!(!masked.contains("supersecret"));
        assert!(!masked.contains("cookies.txt"));
        assert!(!masked.contains("sk-live-1234567890abcdef"));
        assert!(masked.contains("Authorization: Bearer [REDACTED]"));
        assert!(masked.contains("x-api-key: [REDACTED]"));
        assert!(masked.contains("token=[REDACTED]"));
    }
}

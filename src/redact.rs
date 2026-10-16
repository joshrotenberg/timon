//! Secrets taken out of what timon writes to disk: API keys, tokens, private keys in prompts,
//! answers and provider output become `[redacted:<kind>]`.
//!
//! Copied from bottega-core `src/redact.rs` (bottega commit c3d4c83) and kept as is apart from
//! this comment.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::{Captures, Regex};
use serde_json::Value;

struct Rule {
    kind: &'static str,
    re: Regex,
    /// Capture groups kept in front of the redaction, such as `Bearer ` or `NAME=`.
    keep: usize,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |kind, pattern: &str, keep| Rule {
            kind,
            re: Regex::new(pattern).expect("redaction pattern"),
            keep,
        };
        // Order matters: the more specific shapes run first, so an Anthropic key is not
        // reported as an OpenAI one.
        vec![
            rule(
                "private_key",
                r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)",
                0,
            ),
            rule("anthropic", r"sk-ant-[A-Za-z0-9_-]{20,}", 0),
            rule("openai", r"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{20,}", 0),
            rule(
                "github",
                r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,})",
                0,
            ),
            rule("aws", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", 0),
            rule("slack", r"\bxox[abposr]-[A-Za-z0-9-]{10,}", 0),
            rule("google", r"\bAIza[0-9A-Za-z_-]{35}", 0),
            rule(
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
                0,
            ),
            rule(
                "bearer",
                r"(?i)(\bbearer\s+)([A-Za-z0-9._~+/=-]{16,})",
                1,
            ),
            rule(
                "assignment",
                // The name may be quoted, and the quotes escaped, as in JSON tool arguments
                // and JSON inside a string; unquoted names alone let `"password": "..."`
                // through (#23). The value stops at a backslash for the escaped form.
                r#"(?i)(\b[A-Z0-9_.-]*(?:SECRET|TOKEN|PASSWORD|PASSWD|API[_-]?KEY|ACCESS[_-]?KEY|PRIVATE[_-]?KEY)[A-Z0-9_.-]*(?:\\?["'])?\s*[=:]\s*(?:\\?["'])?)([^\s"'`,;\\]{6,})"#,
                1,
            ),
        ]
    })
}

/// The text with every secret replaced by `[redacted:<kind>]`. Borrowed when nothing
/// matched, which is the common case.
pub fn redact_str(text: &str) -> Cow<'_, str> {
    let mut out: Cow<str> = Cow::Borrowed(text);
    for rule in rules() {
        if !rule.re.is_match(&out) {
            continue;
        }
        let replaced = rule
            .re
            .replace_all(&out, |c: &Captures| {
                let whole = c.get(0).map(|m| m.as_str()).unwrap_or("");
                let value = c.get(rule.keep + 1).map(|m| m.as_str()).unwrap_or(whole);
                // `max_tokens: 4000` and `TOKEN_COUNT=123456` name a token but are not one,
                // and a value an earlier rule already replaced keeps its more specific kind.
                if rule.kind == "assignment"
                    && (value.chars().all(|ch| ch.is_ascii_digit())
                        || value.starts_with("[redacted:"))
                {
                    return whole.to_string();
                }
                let prefix: String = (1..=rule.keep)
                    .filter_map(|i| c.get(i).map(|m| m.as_str()))
                    .collect();
                format!("{prefix}[redacted:{}]", rule.kind)
            })
            .into_owned();
        out = Cow::Owned(replaced);
    }
    out
}

/// A key that names a secret: one ending in a secret word, optionally plural (`DB_PASSWORD`,
/// `apiKey`, `client_secret`, `tokens`). A key with a word after it describes a secret
/// rather than holding one (`token_type`, `secret_name`, `private_key_path`, `tokenizer`),
/// and blanking those broke schemas for no gain (found in review).
fn names_a_secret(key: &str) -> bool {
    static NAME: OnceLock<Regex> = OnceLock::new();
    NAME.get_or_init(|| {
        Regex::new(
            r"(?i)(?:secret|token|password|passwd|api[_-]?key|access[_-]?key|private[_-]?key)s?$",
        )
        .expect("secret name pattern")
    })
    .is_match(key)
}

/// Whether a string under a secret-named key is redacted whole: the assignment rule's
/// exceptions, so a count, a short word, a sentence, or an earlier redaction is kept.
fn looks_like_a_value(s: &str) -> bool {
    s.chars().count() >= 6
        && !s.chars().all(|c| c.is_ascii_digit())
        && !s.chars().any(char::is_whitespace)
        && !s.starts_with("[redacted:")
}

/// Redacts every string in a JSON value in place, and every object key.
///
/// JSON keeps a name and its value in separate strings, so the assignment rule, which needs
/// `NAME=value` in one string, never saw `{"DB_PASSWORD": "hunter2hunter2"}`. Every string
/// under a key that names a secret, including inside arrays and objects below it, is
/// therefore redacted whole, with the rule's exceptions (see `looks_like_a_value`). Keys are
/// redacted too, since JSON a worker wrote can use a token as a key (found in review of
/// tosi's result redaction). timon's own field names match no rule.
pub fn redact_value(v: &mut Value) {
    redact_within(v, false);
}

fn redact_within(v: &mut Value, under_secret: bool) {
    match v {
        Value::String(s) if under_secret && looks_like_a_value(s) => {
            *s = "[redacted:assignment]".to_string();
        }
        Value::String(s) => {
            if let Cow::Owned(r) = redact_str(s) {
                *s = r;
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| redact_within(item, under_secret)),
        Value::Object(map) => {
            for (key, mut value) in std::mem::take(map) {
                redact_within(&mut value, under_secret || names_a_secret(&key));
                let mut key = redact_str(&key).into_owned();
                // Two keys can redact to the same text; neither value may be lost.
                if map.contains_key(&key) {
                    let base = key.clone();
                    key = (2..)
                        .map(|n| format!("{base} ({n})"))
                        .find(|k| !map.contains_key(k))
                        .expect("an unused key");
                }
                map.insert(key, value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &str) -> String {
        redact_str(s).into_owned()
    }

    #[test]
    fn each_kind_of_secret_is_replaced_and_named() {
        let cases = [
            (
                "key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
                "key [redacted:anthropic]",
            ),
            ("sk-proj-abcdefghijklmnopqrstuvwx1234", "[redacted:openai]"),
            (
                "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
                "[redacted:github]",
            ),
            (
                "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyzABCDEFGH",
                "[redacted:github]",
            ),
            ("AKIAIOSFODNN7EXAMPLE", "[redacted:aws]"),
            ("xoxb-1234567890-abcdefghij", "[redacted:slack]"),
            (
                "AIzaSyA1234567890abcdefghijklmnopqrstuv",
                "[redacted:google]",
            ),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N",
                "[redacted:jwt]",
            ),
            (
                "Authorization: Bearer abcdef0123456789xyz",
                "Authorization: Bearer [redacted:bearer]",
            ),
            (
                "export DB_PASSWORD=hunter2hunter2",
                "export DB_PASSWORD=[redacted:assignment]",
            ),
            (
                "api_key: \"s3cr3t-value-here\"",
                "api_key: \"[redacted:assignment]\"",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(r(input), want, "{input}");
        }
    }

    #[test]
    fn quoted_and_escaped_json_fields_are_redacted() {
        let cases = [
            (
                r#"{"password":"synthetic-passphrase"}"#,
                r#"{"password":"[redacted:assignment]"}"#,
            ),
            (
                r#"{"api_key": "synthetic-generic-key"}"#,
                r#"{"api_key": "[redacted:assignment]"}"#,
            ),
            (
                r#"'token': 'synthetic-access-token'"#,
                r#"'token': '[redacted:assignment]'"#,
            ),
            (
                r#"{\"token\":\"synthetic-access-token\",\"n\":1}"#,
                r#"{\"token\":\"[redacted:assignment]\",\"n\":1}"#,
            ),
        ];
        for (input, want) in cases {
            assert_eq!(r(input), want, "{input}");
        }
        for fine in [
            r#""max_tokens": 4000"#,
            r#""input_tokens":12"#,
            r#""token_count": "123456789""#,
        ] {
            assert_eq!(r(fine), fine);
        }
    }

    #[test]
    fn a_private_key_block_is_replaced_whole_even_when_cut_short() {
        let pem = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nafter";
        assert_eq!(r(pem), "before\n[redacted:private_key]\nafter");
        let cut = "-----BEGIN RSA PRIVATE KEY-----\nMIIEow ...[truncated]";
        assert_eq!(r(cut), "[redacted:private_key]");
    }

    #[test]
    fn ids_hashes_paths_and_token_counts_are_left_alone() {
        for s in [
            "27d1f434-b559-4583-8026-7c2ba30ac060",
            "commit 3214f33a9c0e1b2d4f5a6b7c8d9e0f1a2b3c4d5e",
            "/Users/me/Code/github.com/joshrotenberg/trovador/src/redact.rs",
            "max_tokens: 4000",
            "TOKEN_COUNT=123456",
            "\"input_tokens\":12345678",
            "the password field is required",
            "sk-short",
        ] {
            assert_eq!(r(s), s);
        }
    }

    #[test]
    fn a_value_is_redacted_by_its_key_and_keys_are_redacted_too() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";
        let mut v = serde_json::json!({
            "env": {"DB_PASSWORD": "hunter2hunter2", "STRIPE_SECRET": "rk_live_abc123",
                    "api_key": "short", "max_tokens": 4000, "input_tokens": "123456",
                    "clientSecret": "abc123def456", "password_policy": "min 12 chars"},
            "tokens": ["hunter2hunter2", {"value": "abc123def456"}],
            "described": {"token_type": "Bearer", "secret_type": "aws_access_key",
                          "private_key_path": "/home/u/.ssh/id_ed25519",
                          "tokenizer": "cl100k_base", "secret_name": "prod/db",
                          "note": "the password is in the vault"},
            "records": [{"text": "OPENAI_API_KEY=sk-abcdefghijklmnopqrstuvwxyz"}],
            "found": {key: "config.toml", "sk-ant-api03-zyxwvutsrqponmlkjihgfedcba9876": "other.toml"},
            "n": 3
        });
        redact_value(&mut v);
        assert_eq!(v["env"]["DB_PASSWORD"], "[redacted:assignment]");
        assert_eq!(v["env"]["STRIPE_SECRET"], "[redacted:assignment]");
        assert_eq!(v["env"]["api_key"], "short", "too short to be a secret");
        assert_eq!(v["env"]["max_tokens"], 4000);
        assert_eq!(v["env"]["input_tokens"], "123456", "all digits");
        assert_eq!(v["env"]["clientSecret"], "[redacted:assignment]");
        assert_eq!(v["env"]["password_policy"], "min 12 chars");
        assert_eq!(v["tokens"][0], "[redacted:assignment]");
        assert_eq!(v["tokens"][1]["value"], "[redacted:assignment]");
        let described = serde_json::json!({"token_type": "Bearer", "secret_type": "aws_access_key",
            "private_key_path": "/home/u/.ssh/id_ed25519", "tokenizer": "cl100k_base",
            "secret_name": "prod/db", "note": "the password is in the vault"});
        assert_eq!(
            v["described"], described,
            "keys that describe a secret keep their values"
        );
        assert_eq!(v["records"][0]["text"], "OPENAI_API_KEY=[redacted:openai]");
        assert_eq!(v["n"], 3);
        let found = v["found"].as_object().unwrap();
        assert!(
            !serde_json::to_string(found).unwrap().contains("sk-ant"),
            "{found:?}"
        );
        let mut values: Vec<&str> = found.values().map(|x| x.as_str().unwrap()).collect();
        values.sort();
        assert_eq!(
            values,
            ["config.toml", "other.toml"],
            "colliding keys keep both values"
        );
    }

    #[test]
    fn trovadors_own_field_names_are_left_alone() {
        let mut v = serde_json::json!({"session_id": "a1", "next_offset": 3,
            "usage": {"cache_creation_input_tokens": 24897, "output_tokens": 470}});
        let before = v.clone();
        redact_value(&mut v);
        assert_eq!(v, before);
    }
}

//! Secret scrubber (ADR-0012 §5) — a regex-based masker that replaces host
//! passwords, tokens, and private keys with `[REDACTED]` before transcript
//! content is served to an external [Coordinator](../../CONTEXT.md).
//!
//! Raw terminal output frequently echoes secrets: an `export AWS_SECRET_ACCESS_KEY=…`
//! the agent ran, a `Bearer …` header in a `curl`, a pasted private key. The
//! Coordinator read API (`GET /nodes/{id}/log` and the `last_assistant_message`
//! in `GET /nodes`) hands that raw material to a remote agent, so a leaked
//! secret would travel off-host. [`SecretScrubber`] is applied at that boundary
//! (in `coordinator::enrichment`) so every coordinator-facing transcript path
//! is masked in exactly one place.
//!
//! ## Two matching strategies
//! 1. **Context-free token formats** (`ghp_…`, `AKIA…`, `Bearer …`, PEM private
//!    key blocks): these are unambiguous shapes, so they are masked wherever
//!    they appear, even mid-sentence.
//! 2. **Key/value secrets** (`PASSWORD=…`, `"api_token": "…"`, `--secret=…`):
//!    masked only when the *key* carries a secret-word, so a benign value isn't
//!    redacted just because it sits after an `=`.
//!
//! Scrubbing is deliberately a *masking* pass, never a parse: it works on the
//! structured transcript fields (turn text, tool-call inputs, last assistant
//! message) and never touches the JSON envelope's own keys, so it cannot
//! corrupt the `{"status":…}` shape the Coordinator relies on.

use once_cell::sync::Lazy;
use regex::{Captures, Regex};

/// The masked replacement. ASCII (no fancy glyphs) so it survives every
/// transport and terminal the transcript might be rendered in.
const MASK: &str = "[REDACTED]";

/// A PEM-encoded private key block (RSA/EC/OPENSSH/…), masked whole. `(?s)` lets
/// `.` span the newline-delimited base64 body; the lazy `.*?` stops at the first
/// `END` marker so two adjacent keys don't merge into one match.
static PRIVATE_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)-----BEGIN[A-Z ]*PRIVATE KEY-----.*?-----END[A-Z ]*PRIVATE KEY-----")
        .expect("private-key regex is valid")
});

/// The secret-word alternation shared by the text key/value rule and the
/// JSON-key rule, so "what counts as a secret-named field" has one definition.
///
/// Deliberately omits a bare `auth`: it would redact the innocuous `author`
/// field GitHub issues carry. It also omits `authorization`: in *text*, an
/// `Authorization: Bearer <token>` value is two words, so a single-value-word
/// rule would mask only the scheme and *leave the token exposed* — the dedicated
/// `Bearer …`/`Basic …` token rules own that header. `auth_token` stays (a
/// genuine `auth_token=…` secret).
const SECRET_WORDS: &str = concat!(
    r"password|passwd|secret|token|api[_\-]?key|access[_\-]?key|",
    r"client[_\-]?secret|credentials?|auth[_\-]?token|private[_\-]?key"
);

/// `key<sep>value` where the key carries a secret-word. The separator allows an
/// optional closing quote before `:`/`=` so JSON (`"password": "x"`) and shell
/// (`PASSWORD=x`) both match; the value runs until the next quote, comma, or
/// whitespace so a trailing `"` or list separator is preserved. Only the value
/// is masked — the key and punctuation are reconstructed in [`scrub_str`].
static KEY_VALUE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(&format!(
        concat!(
            r"(?i)",
            // key — any run of identifier chars containing one secret-word
            r"(?P<key>[\w.\-]*(?:{words})[\w.\-]*)",
            // separator — optional closing quote, then : or =, with surrounding space
            r#"(?P<sep>["']?\s*[:=]\s*)"#,
            // optional opening quote of the value, then the value itself
            r#"(?P<q>["']?)(?P<val>[^\s"',]+)"#,
        ),
        words = SECRET_WORDS
    ))
    .expect("key-value secret regex is valid")
});

/// Matches a JSON object *key* that names a secret. Used by
/// [`SecretScrubber::scrub_json`] to mask a value wholesale when its key says
/// it's a credential — catching structured secrets (`{"password": "swordfish"}`)
/// that carry no token-shaped marker in the value itself. Intentionally a
/// *substring* match (so `accessToken`/`apiKey` camelCase keys are caught):
/// over-masking a benign `tokens_used` is the safe direction for a secret
/// scrubber; missing a real credential key is not.
static SECRET_KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(&format!("(?i)(?:{SECRET_WORDS})")).expect("secret-key regex is valid"));

/// An HTTP auth-scheme credential — `Bearer`/`Basic`/`token`/`OAuth` followed by
/// the credential. Masks the credential while keeping the scheme word (in its
/// original case) so the header stays readable. `token`/`OAuth` cover GitHub's
/// own legacy `Authorization: token <PAT>` and `OAuth <key>` headers, which the
/// key/value rule can't (its value capture is a single word, and `authorization`
/// is deliberately excluded there). The trailing char class includes `+/=` so a
/// Base64 `Basic` credential is fully consumed.
static AUTH_SCHEME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?P<scheme>Bearer|Basic|token|OAuth)\s+[A-Za-z0-9._\-+/]{8,}={0,2}")
        .expect("auth-scheme regex is valid")
});

/// High-confidence, context-free token shapes. Each is distinctive enough that a
/// match is almost certainly a real credential, so they are masked wherever they
/// appear. Auth-scheme headers (`Bearer …`/`Basic …`/`token …`) are handled by
/// [`AUTH_SCHEME_RE`] instead, which preserves the scheme word.
static TOKEN_RES: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    let rules: &[(&str, &str)] = &[
        // GitHub personal-access / OAuth / app tokens (ghp_, gho_, ghu_, ghs_, ghr_).
        (r"\bgh[opsur]_[A-Za-z0-9]{20,}\b", MASK),
        // GitHub fine-grained PAT.
        (r"\bgithub_pat_[A-Za-z0-9_]{20,}\b", MASK),
        // AWS access key id.
        (r"\bAKIA[0-9A-Z]{16}\b", MASK),
        // Google API key.
        (r"\bAIza[0-9A-Za-z_\-]{35}\b", MASK),
        // Slack token.
        (r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b", MASK),
        // OpenAI-style secret key.
        (r"\bsk-[A-Za-z0-9]{20,}\b", MASK),
    ];
    rules
        .iter()
        .map(|(pat, repl)| (Regex::new(pat).expect("token regex is valid"), *repl))
        .collect()
});

/// A regex-based masker for secrets in raw terminal transcript text. Stateless —
/// all methods are associated functions over lazily-compiled static regexes, so
/// there is nothing to construct and the patterns compile exactly once.
pub struct SecretScrubber;

impl SecretScrubber {
    /// Mask every secret in `input`, returning the cleaned string. Runs the
    /// private-key, key/value, and token-format passes in turn; the passes are
    /// ordered so a `token=ghp_…` is masked once by the key/value rule rather
    /// than twice. Idempotent: re-scrubbing already-masked text is a no-op.
    pub fn scrub(input: &str) -> String {
        scrub_str(input)
    }

    /// Recursively mask secrets inside a JSON value (used to scrub a tool call's
    /// raw `input` tree). Two rules, both preserving the JSON shape (object keys
    /// are never altered):
    /// - a string value whose *key* names a secret (`{"password": "x"}`) is
    ///   masked wholesale, even when the value carries no token-shaped marker;
    /// - every other string leaf is content-scrubbed (token shapes, `k=v`
    ///   secrets, private keys) via [`scrub`](Self::scrub).
    pub fn scrub_json(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(s) => {
                let cleaned = scrub_str(s);
                if &cleaned != s {
                    *s = cleaned;
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    Self::scrub_json(item);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    // Under a secret-named key, every string leaf is masked whole
                    // — the key already says it's a credential, and a secret can
                    // hide in an array/object value (`{"api_keys": ["k1","k2"]}`)
                    // that carries no token shape of its own.
                    if SECRET_KEY_RE.is_match(key) {
                        mask_all_strings(v);
                        continue;
                    }
                    Self::scrub_json(v);
                }
            }
            _ => {}
        }
    }
}

/// Replace every string leaf in a JSON subtree with [`MASK`], preserving shape.
/// Used when a key already identifies the whole subtree as a credential, so its
/// values must not survive even if they carry no token marker.
fn mask_all_strings(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = MASK.to_string(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(mask_all_strings),
        serde_json::Value::Object(map) => map.values_mut().for_each(mask_all_strings),
        _ => {}
    }
}

/// The masking pipeline, factored out so both public entry points share it.
fn scrub_str(input: &str) -> String {
    // 1. Whole PEM blocks first, before the token rules can nibble at their
    //    base64 body.
    let step1 = PRIVATE_KEY_RE.replace_all(input, "[REDACTED PRIVATE KEY]");
    // 2. key=value secrets — mask only the value, keep key + punctuation.
    let step2 = KEY_VALUE_RE.replace_all(&step1, |caps: &Captures| {
        format!("{}{}{}{}", &caps["key"], &caps["sep"], &caps["q"], MASK)
    });
    // 3. Auth-scheme credentials — mask the credential, keep the scheme word.
    let step3 = AUTH_SCHEME_RE.replace_all(&step2, |caps: &Captures| {
        format!("{} {}", &caps["scheme"], MASK)
    });
    // 4. Context-free token shapes anywhere they appear.
    let mut out = step3.into_owned();
    for (re, repl) in TOKEN_RES.iter() {
        out = re.replace_all(&out, *repl).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_shell_password_assignment() {
        assert_eq!(
            SecretScrubber::scrub("export DB_PASSWORD=hunter2 && run"),
            "export DB_PASSWORD=[REDACTED] && run"
        );
    }

    #[test]
    fn masks_json_quoted_secret_keeping_quotes_and_shape() {
        // The value is masked but the surrounding quotes and the JSON envelope
        // keys stay intact — a Coordinator must still get parseable JSON.
        assert_eq!(
            SecretScrubber::scrub(r#"{"api_token": "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"}"#),
            r#"{"api_token": "[REDACTED]"}"#
        );
    }

    #[test]
    fn masks_aws_secret_access_key_by_key_word() {
        assert_eq!(
            SecretScrubber::scrub("AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEX"),
            "AWS_SECRET_ACCESS_KEY=[REDACTED]"
        );
    }

    #[test]
    fn masks_github_token_mid_sentence_context_free() {
        // No key=value context here — the distinctive ghp_ shape is enough.
        let scrubbed = SecretScrubber::scrub("I ran it with ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345 set.");
        assert_eq!(scrubbed, "I ran it with [REDACTED] set.");
    }

    #[test]
    fn masks_aws_access_key_id_anywhere() {
        let scrubbed = SecretScrubber::scrub("key id AKIAIOSFODNN7EXAMPLE here");
        assert_eq!(scrubbed, "key id [REDACTED] here");
    }

    #[test]
    fn masks_bearer_token_but_keeps_scheme() {
        assert_eq!(
            SecretScrubber::scrub("Authorization: Bearer abc123def456ghi789"),
            "Authorization: Bearer [REDACTED]"
        );
    }

    #[test]
    fn masks_legacy_token_and_oauth_auth_schemes() {
        // GitHub's own legacy header is `Authorization: token <PAT>`; a classic
        // 40-hex PAT matches no context-free token shape, so only the scheme
        // rule catches it. `OAuth <key>` is covered the same way.
        assert_eq!(
            SecretScrubber::scrub(
                "curl -H 'Authorization: token 1234567890abcdef1234567890abcdef12345678'"
            ),
            "curl -H 'Authorization: token [REDACTED]'"
        );
        assert_eq!(
            SecretScrubber::scrub("Authorization: OAuth deadbeefdeadbeef"),
            "Authorization: OAuth [REDACTED]"
        );
    }

    #[test]
    fn auth_scheme_masking_is_idempotent() {
        let once = SecretScrubber::scrub("Authorization: token deadbeefdeadbeef");
        assert_eq!(once, "Authorization: token [REDACTED]");
        assert_eq!(SecretScrubber::scrub(&once), once);
    }

    #[test]
    fn masks_pem_private_key_block_whole() {
        let pem = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\nabc/def+ghi=\n-----END RSA PRIVATE KEY-----\nafter";
        let scrubbed = SecretScrubber::scrub(pem);
        assert_eq!(scrubbed, "before\n[REDACTED PRIVATE KEY]\nafter");
        assert!(!scrubbed.contains("MIIEpAIBAAKCAQEA"));
    }

    #[test]
    fn leaves_benign_text_untouched() {
        let benign = "Running cargo test on branch feat/x — 42 passed, last_activity_at=2026.";
        assert_eq!(SecretScrubber::scrub(benign), benign);
    }

    #[test]
    fn does_not_redact_author_field() {
        // Regression guard: a bare `auth` secret-word would clobber the issue
        // `author` field the collaborator gate itself reads. It must survive.
        let text = r#"{"author": "octocat", "number": 7}"#;
        assert_eq!(SecretScrubber::scrub(text), text);
    }

    #[test]
    fn is_idempotent() {
        let once = SecretScrubber::scrub("password=swordfish");
        assert_eq!(once, "password=[REDACTED]");
        assert_eq!(SecretScrubber::scrub(&once), once);
    }

    #[test]
    fn scrub_json_masks_string_values_not_keys() {
        let mut v = serde_json::json!({
            "command": "curl -H 'Authorization: Bearer abc123def456ghi789'",
            "password": "swordfish-1234",
            "nested": ["plain", "GITHUB_TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"],
            "count": 3
        });
        SecretScrubber::scrub_json(&mut v);
        assert_eq!(
            v["command"].as_str().unwrap(),
            "curl -H 'Authorization: Bearer [REDACTED]'"
        );
        // The object key "password" is structural and survives; its value is masked.
        assert!(v.as_object().unwrap().contains_key("password"));
        assert_eq!(v["password"].as_str().unwrap(), "[REDACTED]");
        assert_eq!(v["nested"][0].as_str().unwrap(), "plain");
        assert_eq!(v["nested"][1].as_str().unwrap(), "GITHUB_TOKEN=[REDACTED]");
        // Non-string leaves are untouched.
        assert_eq!(v["count"].as_i64().unwrap(), 3);
    }

    #[test]
    fn scrub_json_masks_secret_named_value_without_token_marker() {
        // A plain value under a secret-named key has no token shape of its own,
        // so only the key-aware rule can catch it.
        let mut v = serde_json::json!({"db_password": "swordfish-1234"});
        SecretScrubber::scrub_json(&mut v);
        assert_eq!(v["db_password"].as_str().unwrap(), "[REDACTED]");
    }

    #[test]
    fn scrub_json_masks_array_and_object_under_secret_key() {
        // A secret can hide in a non-string value under a secret-named key; the
        // whole subtree's string leaves must be masked, not just direct strings.
        let mut v = serde_json::json!({
            "api_keys": ["plain-secret-no-shape", "another"],
            "credentials": {"username": "bob", "password": "swordfish"}
        });
        SecretScrubber::scrub_json(&mut v);
        assert_eq!(v["api_keys"][0].as_str().unwrap(), "[REDACTED]");
        assert_eq!(v["api_keys"][1].as_str().unwrap(), "[REDACTED]");
        assert_eq!(v["credentials"]["username"].as_str().unwrap(), "[REDACTED]");
        assert_eq!(v["credentials"]["password"].as_str().unwrap(), "[REDACTED]");
    }

    #[test]
    fn scrub_json_does_not_mask_benign_named_value() {
        // `author` is not a secret key — its value must survive untouched.
        let mut v = serde_json::json!({"author": "octocat", "title": "Fix bug"});
        SecretScrubber::scrub_json(&mut v);
        assert_eq!(v["author"].as_str().unwrap(), "octocat");
        assert_eq!(v["title"].as_str().unwrap(), "Fix bug");
    }
}

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
//! The key/value step is split in two passes run in order: a quoted-value
//! rule masks whole-quoted values like `APP_SECRET="correct horse battery
//! staple"` (issue #1220), and the original single-word rule then handles
//! unquoted values and any residue from the quoted pass.
//!
//! Scrubbing is deliberately a *masking* pass, never a parse: it works on the
//! structured transcript fields (turn text, tool-call inputs, last assistant
//! message) and never touches the JSON envelope's own keys, so it cannot
//! corrupt the `{"status":…}` shape the Coordinator relies on.
//!
//! [`SecretScrubber::scrub_for_persistence`] is the log-file boundary. A log
//! line is free text that may also contain a JSON object, and the text
//! key/value rule leaks the tail of an array under a secret-named key. That
//! entry point masks each embedded object or array first, then runs the same
//! text pipeline, so `buildmesh.log` and a support copy of it are not a second
//! copy of a credential.

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
///
/// `ticket` is included because pairing tickets and WebSocket handshake
/// tickets are credentials (`ticket=<hex>`, `{"ticket":"…"}`). A bare `pair`
/// is not: it is a substring of `repair`. The `#pair=` fragment has its own
/// rule below.
const SECRET_WORDS: &str = concat!(
    r"password|passwd|secret|token|api[_\-]?key|access[_\-]?key|",
    r"client[_\-]?secret|credentials?|auth[_\-]?token|private[_\-]?key|ticket"
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

/// `key<sep>"<value>"` (or `'…'`) for secret-word keys. Two branches — one
/// for double-quoted, one for single-quoted — because the Rust `regex` crate
/// is DFA-based and doesn't support backreferences, so we can't say "closing
/// quote must match the opening one" via a single capture group. The two
/// branches share the key/sep prefix and each captures its own value group
/// (`dq`/`sq`); the replacement callback picks whichever matched. The
/// alternation is wrapped in `(?:…)` so the `|` is scoped to the quote/value
/// pair and doesn't split the whole pattern (which would leave the second
/// branch without a key requirement). This catches a passphrase like
/// `"correct horse battery staple"` whole instead of leaking everything
/// after the first space. Runs **before** [`KEY_VALUE_RE`] in [`scrub_str`] —
/// otherwise the single-word rule would mask only the first word of the
/// passphrase and leave the trailing quote behind, after which this pass
/// would have nothing to extend. Issue #1220.
static QUOTED_VALUE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(&format!(
        concat!(
            r"(?i)",
            r"(?P<key>[\w.\-]*(?:{words})[\w.\-]*)",
            // separator — optional closing quote, then : or =, with surrounding space
            r#"(?P<sep>["']?\s*[:=]\s*)"#,
            // Alternation scoped to the quote/value pair: `"…"` OR `'…'`.
            // `[^"]*` and `[^']*` don't span newlines, which is the common
            // case for quoted secret values (PEM blocks have their own
            // whole-block rule).
            r#"(?:""#,
            r#"(?P<dq>[^"]*)"|'(?P<sq>[^']*)')"#,
        ),
        words = SECRET_WORDS
    ))
    .expect("quoted-value regex is valid")
});

/// Matches a JSON object *key* that names a secret. Used by
/// [`SecretScrubber::scrub_json`] to mask a value wholesale when its key says
/// it's a credential — catching structured secrets (`{"password": "swordfish"}`)
/// that carry no token-shaped marker in the value itself. Intentionally a
/// *substring* match (so `accessToken`/`apiKey` camelCase keys are caught):
/// over-masking a benign `tokens_used` is the safe direction for a secret
/// scrubber; missing a real credential key is not.
static SECRET_KEY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(&format!("(?i)(?:{SECRET_WORDS})")).expect("secret-key regex is valid")
});

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

/// A pairing invitation in a URL fragment (`#pair=<ticket>`). The fragment key
/// is `pair`, which the key/value rule must not treat as a secret word (it is
/// inside `repair`). The value runs to the next whitespace, `#`, or `&`.
static PAIRING_FRAGMENT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)#pair=[^\s&#]+").expect("pairing fragment regex is valid"));

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
        // OpenAI-style secret key. Dashes are part of the Anthropic
        // (`sk-ant-api03-…`) and OpenAI Platform (`sk-proj-…`, `sk-admin-…`)
        // families; the trailing `\b` ensures the dash separator before any
        // following word is *not* swallowed (issue #1220).
        (r"\bsk-[A-Za-z0-9_-]{20,}\b", MASK),
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
    /// private-key, quoted-value, key/value, auth-scheme, pairing-fragment, and
    /// token-format passes in turn; the passes are ordered so a `token=ghp_…`
    /// is masked once by the key/value rule rather than twice, and a quoted
    /// passphrase is masked whole by the quoted-value rule rather than leaking
    /// the words after the first space (issue #1220). Idempotent: re-scrubbing
    /// already-masked text is a no-op.
    pub fn scrub(input: &str) -> String {
        scrub_str(input)
    }

    /// Mask a log line before it is written to `buildmesh.log`.
    ///
    /// Embedded JSON objects and arrays are parsed and passed through
    /// [`scrub_json`](Self::scrub_json), so a secret-named key masks its whole
    /// value — including a plain hex root or device token, and an array of
    /// credentials the text rule would only nibble. The text pipeline masks
    /// the surrounding prose (PEM blocks, authorization headers, pairing
    /// fragments, token shapes) and is not run over the JSON again: a second
    /// pass would treat the already-masked array as a key/value and tear the
    /// brackets. A span that is not JSON, and a JSON value with nothing to
    /// mask, is kept byte-for-byte.
    pub fn scrub_for_persistence(input: &str) -> String {
        scrub_log_line(input)
    }

    /// Recursively mask secrets inside a JSON value (used to scrub a tool call's
    /// raw `input` tree). Two rules, both preserving the JSON shape (object keys
    /// are never altered):
    /// - a string value whose *key* names a secret (`{"password": "x"}`) is
    ///   masked wholesale, even when the value carries no token-shaped marker;
    /// - every other string leaf is content-scrubbed (token shapes, `k=v`
    ///   secrets, private keys) via [`scrub`](Self::scrub).
    pub fn scrub_json(value: &mut serde_json::Value) {
        scrub_json_at(value, 0);
    }
}

/// How many times a string leaf may itself contain JSON. A hostile line can
/// nest `{"a":"{\"a\":…}"}` without bound; past this depth the leaf is masked
/// as text only.
const MAX_EMBED_DEPTH: u32 = 8;

/// Mask a log line: JSON values through [`scrub_json_at`], and the prose
/// between them through [`scrub_str`].
///
/// A `{` or `[` inside a quoted span, or one that is not valid JSON, stays in
/// the prose, so `password="correct {} horse"` is still one quoted value. A JSON value that
/// is itself the value of a secret-named key (`token=["…"]`) is masked whole.
/// A string leaf that contains JSON is scanned again, up to [`MAX_EMBED_DEPTH`].
fn scrub_log_line(input: &str) -> String {
    scrub_log_line_at(input, 0)
}

fn scrub_log_line_at(input: &str, depth: u32) -> String {
    if depth > MAX_EMBED_DEPTH {
        return scrub_str(input);
    }
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let mut search = i;
        let mut parsed = None;
        while let Some(start) = next_json_start(input, search) {
            if let Some((end, value)) = parse_json_value(input, start) {
                parsed = Some((start, end, value));
                break;
            }
            // A brace that is not JSON stays in the prose span. Splitting
            // there would hide the tail of `password="correct {horse}"` from
            // the quoted-value rule.
            search = start + 1;
        }
        let Some((start, end, mut value)) = parsed else {
            out.push_str(&scrub_str(&input[i..]));
            break;
        };
        let prose = &input[i..start];
        if let Some(key_at) = secret_key_prefix(prose) {
            let original = value.clone();
            mask_all_strings(&mut value);
            out.push_str(&scrub_str(&prose[..key_at]));
            out.push_str(&prose[key_at..]);
            if value == original {
                out.push_str(&input[start..end]);
            } else {
                out.push_str(&value.to_string());
            }
        } else {
            let original = value.clone();
            scrub_json_at(&mut value, depth);
            out.push_str(&scrub_str(prose));
            if value == original {
                out.push_str(&input[start..end]);
            } else {
                out.push_str(&value.to_string());
            }
        }
        i = end;
    }
    out
}

/// Next `{` or `[` that is not inside a quoted span.
///
/// A brace inside `password="correct {} horse"` is part of the secret, not a
/// JSON value. Double quotes always open a span. A single quote opens one
/// only after `=` or `:`, so an apostrophe in `it's` does not swallow a later
/// object.
fn next_json_start(input: &str, from: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut i = from;
    let mut quote: Option<u8> = None;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if escape {
                escape = false;
            } else if c == b'\\' && q == b'"' {
                escape = true;
            } else if c == q {
                quote = None;
            }
        } else if c == b'"' {
            quote = Some(b'"');
        } else if c == b'\'' && opens_single_quote(bytes, i) {
            // Skip a passphrase (`password='correct {} horse'`). A quoted
            // span whose body is itself a JSON object or array stays visible,
            // or `data='{"api_keys":["…"]}'` would never be parsed.
            if let Some(end) = matching_single_quote(bytes, i) {
                let body = &input[i + 1..end];
                // A secret-named key owns the whole quoted value
                // (`token='["<hex>"]'`). Leaving the JSON visible would hide
                // that key from the quoted-value rule. A non-secret key
                // (`data='{"api_keys":[…]}'`) still has to be parsed.
                let owned_by_secret = secret_key_prefix(&input[from..i]).is_some();
                if owned_by_secret || !json_container(body) {
                    i = end;
                    continue;
                }
            }
        } else if c == b'{' || c == b'[' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn matching_single_quote(bytes: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn json_container(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .is_some_and(|value| value.is_object() || value.is_array())
}

fn opens_single_quote(bytes: &[u8], at: usize) -> bool {
    let mut j = at;
    while j > 0 {
        j -= 1;
        match bytes[j] {
            b' ' | b'\t' | b'\n' | b'\r' => continue,
            b'=' | b':' => return true,
            _ => return false,
        }
    }
    true
}

fn parse_json_value(input: &str, start: usize) -> Option<(usize, serde_json::Value)> {
    let end = json_value_end(input, start)?;
    let value: serde_json::Value = serde_json::from_str(&input[start..end]).ok()?;
    if value.is_array() || value.is_object() {
        Some((end, value))
    } else {
        None
    }
}

/// Byte offset of a secret-named key whose separator (`:` or `=`) is the last
/// non-space before `prose` ends, which is where a JSON value is about to
/// start. The key's value is then the whole JSON value, not a leaf inside it.
fn secret_key_prefix(prose: &str) -> Option<usize> {
    let trimmed_end = prose.trim_end().len();
    if trimmed_end == 0 {
        return None;
    }
    let head = &prose[..trimmed_end];
    let last = *head.as_bytes().last()?;
    if last != b':' && last != b'=' {
        return None;
    }
    let mut key_end = head[..head.len() - 1].trim_end().len();
    if key_end == 0 {
        return None;
    }
    let quote = head.as_bytes()[key_end - 1];
    if quote == b'"' || quote == b'\'' {
        key_end -= 1;
    }
    if key_end == 0 {
        return None;
    }
    let key_region = &head[..key_end];
    let rel = match key_region
        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'))
    {
        Some(at) => at + key_region[at..].chars().next()?.len_utf8(),
        None => 0,
    };
    let key = &key_region[rel..];
    if key.is_empty() || !SECRET_KEY_RE.is_match(key) {
        return None;
    }
    Some(rel)
}

fn scrub_json_at(value: &mut serde_json::Value, depth: u32) {
    match value {
        serde_json::Value::String(s) => {
            let cleaned = if depth >= MAX_EMBED_DEPTH {
                scrub_str(s)
            } else {
                scrub_log_line_at(s, depth + 1)
            };
            if &cleaned != s {
                *s = cleaned;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                scrub_json_at(item, depth);
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
                scrub_json_at(v, depth);
            }
        }
        _ => {}
    }
}

/// Byte index just past the JSON value that opens at `start`, or `None` when
/// the brackets never close. `start` points at `{` or `[`.
fn json_value_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_string = false;
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i + 1);
                    }
                    if depth < 0 {
                        return None;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
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
    // 2. Quoted multi-word secrets — runs BEFORE the single-word key=value
    //    rule so a passphrase like `"correct horse battery staple"` is masked
    //    whole. If the single-word rule ran first it would only mask the first
    //    word, and by the time this pass saw the input the closing quote
    //    would be detached from its opening one, leaving the rest exposed
    //    (issue #1220). The callback picks which branch matched (`dq` vs
    //    `sq`) so the replacement uses the right quote character on both
    //    sides of the mask.
    let step2 = QUOTED_VALUE_RE.replace_all(&step1, |caps: &Captures| {
        let quote = if caps.name("dq").is_some() { '"' } else { '\'' };
        format!("{}{}{}{}{}", &caps["key"], &caps["sep"], quote, MASK, quote)
    });
    // 3. Unquoted (or single-quoted) key=value secrets — mask only the value,
    //    keep key + punctuation.
    let step3 = KEY_VALUE_RE.replace_all(&step2, |caps: &Captures| {
        format!("{}{}{}{}", &caps["key"], &caps["sep"], &caps["q"], MASK)
    });
    // 4. Auth-scheme credentials — mask the credential, keep the scheme word.
    let step4 = AUTH_SCHEME_RE.replace_all(&step3, |caps: &Captures| {
        format!("{} {}", &caps["scheme"], MASK)
    });
    // 5. Pairing invitations in URL fragments. Idempotent: `#pair=[REDACTED]`
    //    matches the same value pattern and is replaced with itself.
    let step5 = PAIRING_FRAGMENT_RE.replace_all(&step4, "#pair=[REDACTED]");
    // 6. Context-free token shapes anywhere they appear.
    let mut out = step5.into_owned();
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
        let scrubbed =
            SecretScrubber::scrub("I ran it with ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345 set.");
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

    // --- #1220: multi-word quoted secrets --------------------------------
    //
    // A passphrase or multi-word secret under a secret-named key. The single-word
    // value rule used `[^\s"',]+` so only the first word was masked and the rest
    // of the secret leaked to the Coordinator.

    #[test]
    fn masks_quoted_multi_word_secret_value_in_shell_form() {
        // Bug #1220: the regex stopped at whitespace, so `correct horse battery
        // staple` exposed everything after the first word.
        assert_eq!(
            SecretScrubber::scrub(r#"APP_SECRET="correct horse battery staple""#),
            r#"APP_SECRET="[REDACTED]""#
        );
    }

    #[test]
    fn masks_quoted_multi_word_secret_value_in_json_form() {
        // Same bug, JSON shape — a passphrase under a secret-named key.
        assert_eq!(
            SecretScrubber::scrub(r#"{"password": "correct horse battery staple"}"#),
            r#"{"password": "[REDACTED]"}"#
        );
    }

    #[test]
    fn masks_quoted_multi_word_secret_value_with_single_quotes() {
        // The tempered-greedy regex must close on the *matching* quote.
        assert_eq!(
            SecretScrubber::scrub("API_KEY='multi word secret value'"),
            "API_KEY='[REDACTED]'"
        );
    }

    #[test]
    fn masks_quoted_multi_word_secret_value_with_spaces_around_equals() {
        assert_eq!(
            SecretScrubber::scrub(r#"DB_PASSWORD  =  "phrase with spaces""#),
            r#"DB_PASSWORD  =  "[REDACTED]""#
        );
    }

    #[test]
    fn masks_pairing_fragment_and_ticket_assignment() {
        let raw = "open https://127.0.0.1/#pair=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa then ticket=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let once = SecretScrubber::scrub(raw);
        assert_eq!(
            once,
            "open https://127.0.0.1/#pair=[REDACTED] then ticket=[REDACTED]"
        );
        assert_eq!(SecretScrubber::scrub(&once), once);
    }

    #[test]
    fn scrub_for_persistence_masks_nested_credential_array() {
        // The text key/value rule stops at the first quote, so
        // `{"api_keys":["plain-secret-no-shape"]}` would keep the secret.
        // Persistence parsing has to mask the whole array.
        let raw = r#"failed {"api_keys":["plain-secret-no-shape"],"author":"octocat"}"#;
        let scrubbed = SecretScrubber::scrub_for_persistence(raw);
        assert!(!scrubbed.contains("plain-secret-no-shape"), "{scrubbed}");
        assert!(
            scrubbed.contains(r#"["[REDACTED]"]"#),
            "the array must stay intact around the mask: {scrubbed}"
        );
        assert!(scrubbed.contains("octocat"), "{scrubbed}");
    }

    #[test]
    fn scrub_for_persistence_leaves_benign_prose_unchanged() {
        // `repair` contains `pair` and `author` contains `auth`. Neither is a
        // credential key. A log line with no secret must stay byte-identical
        // so the writer does not rewrite ordinary diagnostics.
        let raw = "repair=yes author=octocat session=42";
        assert_eq!(SecretScrubber::scrub_for_persistence(raw), raw);
    }

    #[test]
    fn scrub_for_persistence_masks_json_embedded_in_a_string() {
        let raw = r#"{"message":"{\"api_keys\":[\"plain-secret-no-shape\"]}","author":"octocat"}"#;
        let scrubbed = SecretScrubber::scrub_for_persistence(raw);
        assert!(!scrubbed.contains("plain-secret-no-shape"), "{scrubbed}");
        assert!(scrubbed.contains("octocat"), "{scrubbed}");
    }

    #[test]
    fn scrub_for_persistence_masks_a_json_value_owned_by_a_secret_key() {
        let array = r#"token=["0123456789abcdef0123456789abcdef"]"#;
        let object = r#"token={"raw":"fedcba9876543210fedcba9876543210"}"#;
        for raw in [array, object] {
            let scrubbed = SecretScrubber::scrub_for_persistence(raw);
            assert!(
                !scrubbed.contains("0123456789abcdef0123456789abcdef"),
                "{scrubbed}"
            );
            assert!(
                !scrubbed.contains("fedcba9876543210fedcba9876543210"),
                "{scrubbed}"
            );
            assert!(scrubbed.contains("token="), "{scrubbed}");
            assert!(scrubbed.contains("[REDACTED]"), "{scrubbed}");
        }
    }

    #[test]
    fn scrub_for_persistence_masks_a_quoted_secret_that_contains_json() {
        // `{}` is valid JSON. Pulling it out of a quoted secret splits the
        // value and leaves the tail. The quote has to hide that brace from
        // the JSON scan, including when the assignment sits inside a string.
        let top = r#"password="correct {} horse" session=42"#;
        assert_eq!(
            SecretScrubber::scrub_for_persistence(top),
            SecretScrubber::scrub(top),
            "{top}"
        );
        assert!(!SecretScrubber::scrub_for_persistence(top).contains("horse"));
        let nested = r#"{"message":"password=\"correct {} horse\"","author":"octocat"}"#;
        let scrubbed = SecretScrubber::scrub_for_persistence(nested);
        assert!(!scrubbed.contains("horse"), "{scrubbed}");
        assert!(scrubbed.contains("octocat"), "{scrubbed}");
    }

    #[test]
    fn scrub_for_persistence_still_parses_single_quoted_json() {
        // A single quote after `=` hides a passphrase that contains `{}`.
        // It must not hide a JSON object wrapped in those same quotes, or the
        // text rule nibbles the array and keeps the credential.
        let wrapped = r#"data='{"api_keys":["plain-secret-no-shape"]}'"#;
        let scrubbed = SecretScrubber::scrub_for_persistence(wrapped);
        assert!(!scrubbed.contains("plain-secret-no-shape"), "{scrubbed}");
        let prose = "password='correct {} horse' session=42";
        assert_eq!(
            SecretScrubber::scrub_for_persistence(prose),
            SecretScrubber::scrub(prose)
        );
        assert!(!SecretScrubber::scrub_for_persistence(prose).contains("horse"));
        let apostrophe = r#"it's {"api_keys":["plain-secret-no-shape"]}"#;
        assert!(
            !SecretScrubber::scrub_for_persistence(apostrophe).contains("plain-secret-no-shape"),
            "{apostrophe}"
        );
        // The secret name sits outside the JSON. Parsing the body would drop
        // that name and keep the credential `scrub` removes.
        let owned = "token='[\"0123456789abcdef0123456789abcdef\"]'";
        assert_eq!(
            SecretScrubber::scrub_for_persistence(owned),
            SecretScrubber::scrub(owned),
            "{owned}"
        );
        assert!(!SecretScrubber::scrub_for_persistence(owned).contains("0123456789abcdef"));
        let named = r#"password='{"user":"alice"}'"#;
        assert!(
            !SecretScrubber::scrub_for_persistence(named).contains("alice"),
            "{named}"
        );
    }

    #[test]
    fn scrub_for_persistence_matches_scrub_when_a_brace_is_not_json() {
        // A `{` inside a quoted secret is not a JSON value. Splitting the line
        // there lets the tail of the secret through. Persistence must mask the
        // same span the text pipeline does.
        for raw in [
            r#"password="correct {horse}" session=42"#,
            "token=abc{def session=42",
        ] {
            assert_eq!(
                SecretScrubber::scrub_for_persistence(raw),
                SecretScrubber::scrub(raw),
                "{raw}"
            );
        }
    }

    #[test]
    fn quoted_multi_word_masking_is_idempotent() {
        // Re-scrubbing already-masked output must be a no-op.
        let once = SecretScrubber::scrub(r#"PASSWORD="hello world""#);
        assert_eq!(once, r#"PASSWORD="[REDACTED]""#);
        assert_eq!(SecretScrubber::scrub(&once), once);
    }

    // --- #1220: dashed `sk-` keys ----------------------------------------
    //
    // The `sk-` rule excluded dashes, so the Anthropic (`sk-ant-api03-…`) and
    // OpenAI Platform (`sk-proj-…` / `sk-admin-…`) families never matched.

    #[test]
    fn masks_anthropic_sk_ant_api03_key_with_dashes() {
        // Real Anthropic key shape — the rule must catch the full token, dashes
        // included.
        let raw = "sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB";
        assert_eq!(SecretScrubber::scrub(raw), MASK);
    }

    #[test]
    fn masks_openai_sk_proj_key_with_dashes() {
        let raw = "sk-proj-abcdefghijklmnopqrstuv1234567890ABCD";
        assert_eq!(SecretScrubber::scrub(raw), MASK);
    }

    #[test]
    fn masks_openai_sk_admin_key_with_dashes() {
        let raw = "sk-admin-abcdefghijklmnopqrstuv1234567890ABCD";
        assert_eq!(SecretScrubber::scrub(raw), MASK);
    }

    #[test]
    fn dashed_sk_key_does_not_swallow_trailing_word() {
        // Greedy `[A-Za-z0-9_-]{20,}` must stop at the last word char so a
        // dash separator between the key and the next token doesn't get eaten.
        let scrubbed = SecretScrubber::scrub(
            "export sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB other",
        );
        assert_eq!(scrubbed, "export [REDACTED] other");
    }

    #[test]
    fn dashed_sk_key_masking_is_idempotent() {
        let raw = "sk-ant-api03-DUMMYKEYEXAMPLEabcdefghijklmnop1234567890AB";
        let once = SecretScrubber::scrub(raw);
        assert_eq!(once, MASK);
        assert_eq!(SecretScrubber::scrub(&once), once);
    }
}

//! Local reversible redaction for prompts, tool calls, and tool output.
//!
//! The scrubber replaces sensitive values with opaque session tokens. The
//! mapping stays in memory; a caller must explicitly ask to restore a token for
//! one scope. It is deliberately not a hash: hashes cannot be reversed, and
//! unsalted hashes of phone numbers or email addresses are dictionary-attackable.

use std::{collections::BTreeMap, fmt};

use rand::Rng;
use regex::Regex;

/// What a token represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Email,
    Phone,
    ApiKey,
    Card,
    PrivateKey,
    Secret,
    Credential,
    Generic,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Phone => "phone",
            Self::ApiKey => "api_key",
            Self::Card => "card",
            Self::PrivateKey => "private_key",
            Self::Secret => "secret",
            Self::Credential => "credential",
            Self::Generic => "generic",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
struct Entry {
    kind: Kind,
    value: String,
    scopes: Vec<String>,
}

/// Session-scoped reversible redaction.
pub struct Scrubber {
    key: [u8; 32],
    entries: BTreeMap<String, Entry>,
}

impl Scrubber {
    /// Create a new session. Tokens are stable only inside this instance.
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        rand::rng().fill(&mut key);
        Self {
            key,
            entries: BTreeMap::new(),
        }
    }

    /// Redact detected PII and secrets for display/human read-back.
    pub fn scrub(&mut self, text: &str) -> String {
        self.scrub_with_scopes(text, &["display"])
    }

    /// Redact using an explicit set of restore scopes.
    pub fn scrub_with_scopes(&mut self, text: &str, scopes: &[&str]) -> String {
        let mut text = self.scrub_private_keys(text, scopes);
        text = self.scrub_regex(&text, &API_KEY_RE, Kind::ApiKey, scopes, |_| true);
        text = self.scrub_regex(&text, &EMAIL_RE, Kind::Email, scopes, |_| true);
        text = self.scrub_cards(&text, scopes);
        text = self.scrub_phones(&text, scopes);
        self.scrub_assignments(text, scopes)
    }

    /// Insert a known value and return its token.
    pub fn tokenize(&mut self, kind: Kind, value: &str, scopes: &[&str]) -> String {
        if let Some((token, _)) = self.existing(kind, value) {
            return token.to_string();
        }
        for width in [8usize, 12, 16, 24, 32] {
            let token = self.make_token(kind, value, width);
            match self.entries.get(&token) {
                Some(entry) if entry.value != value || entry.kind != kind => continue,
                _ => {
                    self.entries.insert(
                        token.clone(),
                        Entry {
                            kind,
                            value: value.to_string(),
                            scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
                        },
                    );
                    return token;
                }
            }
        }
        // A 32-hex-character keyed digest collision is not realistic; keep the
        // API total rather than silently overwriting another secret.
        unreachable!("scrub token collision at full width")
    }

    /// Restore tokens that explicitly allow `scope`.
    ///
    /// Unauthorized tokens are left untouched. This is intentional: a model
    /// cannot force expansion by emitting a guessed token.
    pub fn restore(&self, text: &str, scope: &str) -> String {
        TOKEN_RE
            .replace_all(text, |captures: &regex::Captures<'_>| {
                let token = &captures[0];
                let Some(entry) = self.entries.get(token) else {
                    return token.to_string();
                };
                if entry
                    .scopes
                    .iter()
                    .any(|allowed| allowed == "*" || allowed == scope)
                {
                    entry.value.clone()
                } else {
                    token.to_string()
                }
            })
            .into_owned()
    }

    /// Restore every token whose scope matches any of the supplied scopes.
    pub fn restore_scopes(&self, text: &str, scopes: &[&str]) -> String {
        TOKEN_RE
            .replace_all(text, |captures: &regex::Captures<'_>| {
                let token = &captures[0];
                let Some(entry) = self.entries.get(token) else {
                    return token.to_string();
                };
                if entry
                    .scopes
                    .iter()
                    .any(|allowed| allowed == "*" || scopes.iter().any(|scope| allowed == scope))
                {
                    entry.value.clone()
                } else {
                    token.to_string()
                }
            })
            .into_owned()
    }

    /// Human-only local reveal. Not exposed through the model gateway.
    pub fn value(&self, token: &str) -> Option<&str> {
        self.entries.get(token).map(|entry| entry.value.as_str())
    }

    pub fn tokens(&self) -> impl Iterator<Item = (&str, Kind)> {
        self.entries
            .iter()
            .map(|(token, entry)| (token.as_str(), entry.kind))
    }

    fn existing(&self, kind: Kind, value: &str) -> Option<(&str, &Entry)> {
        self.entries
            .iter()
            .find(|(_, entry)| entry.kind == kind && entry.value == value)
            .map(|(token, entry)| (token.as_str(), entry))
    }

    fn make_token(&self, kind: Kind, value: &str, width: usize) -> String {
        let mut hasher = blake3::Hasher::new_keyed(&self.key);
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(value.as_bytes());
        let digest = hasher.finalize().to_hex();
        let id = &digest.as_str()[..width];
        format!("⟦mush:{}:{}⟧", kind.as_str(), id)
    }

    fn scrub_private_keys(&mut self, text: &str, scopes: &[&str]) -> String {
        self.scrub_regex(text, &PRIVATE_KEY_RE, Kind::PrivateKey, scopes, |_| true)
    }

    fn scrub_regex(
        &mut self,
        text: &str,
        regex: &Regex,
        kind: Kind,
        scopes: &[&str],
        accept: impl Fn(&str) -> bool,
    ) -> String {
        regex
            .replace_all(text, |captures: &regex::Captures<'_>| {
                let value = &captures[0];
                if !accept(value) || value.starts_with('⟦') {
                    return value.to_string();
                }
                self.tokenize(kind, value, scopes)
            })
            .into_owned()
    }

    fn scrub_cards(&mut self, text: &str, scopes: &[&str]) -> String {
        self.scrub_regex(text, &CARD_RE, Kind::Card, scopes, |value| {
            let digits: String = value.chars().filter(char::is_ascii_digit).collect();
            (13..=19).contains(&digits.len()) && luhn(&digits)
        })
    }

    fn scrub_phones(&mut self, text: &str, scopes: &[&str]) -> String {
        self.scrub_regex(text, &PHONE_RE, Kind::Phone, scopes, |value| {
            let digits = value.chars().filter(char::is_ascii_digit).count();
            (7..=15).contains(&digits) && !looks_like_date(value)
        })
    }

    fn scrub_assignments(&mut self, text: String, scopes: &[&str]) -> String {
        ASSIGNMENT_RE
            .replace_all(&text, |captures: &regex::Captures<'_>| {
                let Some(value) = captures.get(2) else {
                    return captures[0].to_string();
                };
                let value = value.as_str();
                if value.starts_with('⟦') {
                    return captures[0].to_string();
                }
                let token = self.tokenize(Kind::Secret, value, scopes);
                format!("{}={}", &captures[1], token)
            })
            .into_owned()
    }
}

impl Default for Scrubber {
    fn default() -> Self {
        Self::new()
    }
}

fn luhn(digits: &str) -> bool {
    let mut sum = 0u32;
    let parity = digits.len() % 2;
    for (index, ch) in digits.chars().enumerate() {
        let Some(mut value) = ch.to_digit(10) else {
            return false;
        };
        if index % 2 == parity {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
    }
    sum.is_multiple_of(10)
}

fn looks_like_date(value: &str) -> bool {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    let separators = value.chars().filter(|ch| !ch.is_ascii_digit()).count();
    digits.len() <= 8 && separators <= 2 && (value.contains('/') || value.contains('-'))
}

static EMAIL_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").expect("email regex")
});
static PHONE_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\+?[0-9][0-9()\-.\s]{6,}[0-9]").expect("phone regex"));
static CARD_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"\b[0-9][0-9 \-]{11,}[0-9]\b").expect("card regex"));
static API_KEY_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?:
          sk-[A-Za-z0-9_-]{16,}
        | gh[pousr]_[A-Za-z0-9_]{16,}
        | github_pat_[A-Za-z0-9_]{20,}
        | xox[baprs]-[A-Za-z0-9-]{10,}
        | AKIA[0-9A-Z]{16}
        | AIza[0-9A-Za-z_-]{20,}
        | npm_[A-Za-z0-9]{20,}
        | sk_live_[A-Za-z0-9]{16,}
        )",
    )
    .expect("api key regex")
});
static PRIVATE_KEY_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
        .expect("private key regex")
});
static ASSIGNMENT_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_]*(?:token|key|secret|password|pass|pwd)[A-Za-z0-9_]*)\s*=\s*([^\s"'`]{6,})"#,
    )
    .expect("assignment regex")
});
static TOKEN_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"⟦mush:[a-z_]+:[0-9a-f]+⟧").expect("token regex"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_and_phone_are_reversible_with_display_scope() {
        let mut scrubber = Scrubber::new();
        let input = "Ada: ada@example.com, +1 (415) 555-0199";
        let scrubbed = scrubber.scrub(input);
        assert!(!scrubbed.contains("ada@example.com"));
        assert!(!scrubbed.contains("555-0199"));
        assert!(scrubbed.contains("⟦mush:email:"));
        assert!(scrubbed.contains("⟦mush:phone:"));
        assert_eq!(scrubber.restore(&scrubbed, "display"), input);
        assert_eq!(scrubber.restore(&scrubbed, "bash"), scrubbed);
    }

    #[test]
    fn same_value_gets_the_same_token() {
        let mut scrubber = Scrubber::new();
        let first = scrubber.tokenize(Kind::Email, "a@example.com", &["display"]);
        let second = scrubber.tokenize(Kind::Email, "a@example.com", &["display"]);
        assert_eq!(first, second);
        assert_eq!(scrubber.value(&first), Some("a@example.com"));
    }

    #[test]
    fn known_api_keys_and_assignments_are_scrubbed() {
        let mut scrubber = Scrubber::new();
        let input = "key=ghp_123456789012345678901234567890\nTOKEN=supersecretvalue";
        let scrubbed = scrubber.scrub(input);
        assert!(!scrubbed.contains("ghp_123456789012345678901234567890"));
        assert!(!scrubbed.contains("supersecretvalue"));
        assert!(scrubbed.contains("⟦mush:api_key:"));
        assert!(scrubbed.contains("⟦mush:secret:"));
        assert_eq!(scrubber.restore(&scrubbed, "display"), input);
    }

    #[test]
    fn only_matching_scopes_restore() {
        let mut scrubber = Scrubber::new();
        let token = scrubber.tokenize(
            Kind::Credential,
            "geode://github_token",
            &["http:api.github.com"],
        );
        assert_eq!(
            scrubber.restore(&format!("Authorization: Bearer {token}"), "bash"),
            format!("Authorization: Bearer {token}")
        );
        assert_eq!(
            scrubber.restore(
                &format!("Authorization: Bearer {token}"),
                "http:api.github.com"
            ),
            "Authorization: Bearer geode://github_token"
        );
    }

    #[test]
    fn luhn_gate_avoids_arbitrary_long_digits() {
        let mut scrubber = Scrubber::new();
        let valid = scrubber.scrub("card 4242 4242 4242 4242");
        assert!(valid.contains("⟦mush:card:"));
        let invalid = scrubber.scrub("number 1234 5678 9012 3456");
        assert!(!invalid.contains("⟦mush:card:"));
    }

    #[test]
    fn private_key_blocks_are_scrubbed() {
        let mut scrubber = Scrubber::new();
        let input =
            "-----BEGIN OPENSSH PRIVATE KEY-----\nsecret\n-----END OPENSSH PRIVATE KEY-----";
        let scrubbed = scrubber.scrub(input);
        assert!(scrubbed.contains("⟦mush:private_key:"));
        assert_eq!(scrubber.restore(&scrubbed, "display"), input);
    }
}

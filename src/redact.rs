//! Secret masker (frozen interface, see the build contract). Every hit is
//! replaced ENTIRELY by a `[masked:<class>:<len>:<fp>]` marker where class is
//! token | entropy | assignment | credential, len is the original hit length,
//! and fp is a short prefix of a sha digest of the hit — nothing reversible and
//! no plaintext prefix survives.
//!
//! Pure string transform: no IO, deterministic, idempotent — marker bodies use
//! separators that sit outside every hit alphabet, so a second pass is a no-op.
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::Serialize;
use sha2::{Digest, Sha256};

const FP_LEN: usize = 8;
const RUN_MIN: usize = 40;
const DISTINCT_MIN: usize = 16;
const GROUPS_MIN: usize = 3;

/// Dense token alphabet shared by the entropy and assignment value classes.
const DENSE: &str = "[A-Za-z0-9+/=_-]";

// Class (c), assignment shape: an UPPER_CASE name, an equals sign, then a long
// token value. The whole assignment is the hit.
static ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\b[A-Z][A-Z0-9_]{{2,}}={DENSE}{{16,}}")).expect("assignment pattern")
});

// Class (a), provider-token shape: a short lowercase prefix of two to seven
// letters, a dash, then a twenty-plus run of token characters.
static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[a-z]{2,7}-[A-Za-z0-9_-]{20,}").expect("token pattern"));

// Class (b), candidate dense runs of forty-plus characters; the per-hit
// diversity check below keeps prose and plain hex digests out.
static ENTROPY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{DENSE}{{40,}}")).expect("entropy pattern"));

// Class (d), credential-carrying syntax. The three classes above recognise a
// value that LOOKS like a secret; these recognise syntax that is only ever used
// to hand a secret to something, so a short, lowercase, dictionary-word
// password — the shape that actually leaked an operating-system password into
// this Lake — is masked on its context instead of its entropy.
//
// Only the value is replaced, never the surrounding command: `echo … | sudo -S`
// or `send …` is evidence an analyst needs, and the shape itself carries nothing
// private. Every value alternative is therefore captured as `val` or `bare`.
//
// Quotes in transcript text are usually backslash-escaped one or more times,
// because a tool call arrives as JSON nested inside JSON, so every quote here is
// a run of backslashes followed by a quote. A value stops at the first quote,
// backslash, or newline, which is also what keeps `send \"secret\\r\"` from
// swallowing the trailing `\r`.
const QUOTE: &str = r#"(?:\\*["'])"#;
const VALUE: &str = r#"[^"'\\\n]{1,256}"#;

// A password piped into a password-reading command: `echo "<secret>" | sudo -S`,
// including `printf`, `echo -n`, combined sudo flags (`-kS`), and `--stdin`.
static CRED_STDIN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:echo|printf)[ \t]+(?:-[A-Za-z]+[ \t]+)?{QUOTE}(?P<val>{VALUE}){QUOTE}[ \t]*\|[ \t]*sudo(?:[ \t]+-[A-Za-z]+)*(?:[ \t]+-[A-Za-z]*S\b|[ \t]+--stdin\b)"
    ))
    .expect("stdin credential pattern")
});

// An expect script answering a password prompt. The prompt word anchors the
// rule and the lazy window takes the FIRST `send` after it, so the interactive
// command sends that follow (`send "mkdir -p ~/.ssh…"`) stay readable. `send:
// sending "…"` is expect's own diagnostic echo of the same secret.
static CRED_EXPECT_SEND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?is)(?:password|passwd|passphrase).{{0,80}}?\bsend(?:ing)?\b[^"'\n]{{0,24}}?{QUOTE}(?P<val>{VALUE})"#
    ))
    .expect("expect-send credential pattern")
});

// A flag value is either fully quoted or fully bare. Making the opening quote
// part of the quoted alternative is what stops a bare value from running to
// whatever quote happens to appear later in the command.
const FLAG_VALUE: &str = r#"(?:(?:\\*["'])(?P<val>[^"'\\\n]{1,256})(?:\\*["'])|(?P<bare>[^-\s\\"'|;&][^\s\\"'|;&]{0,255}))"#;

// `--password <value>`, `--password=<value>`, and the long spellings around it.
static CRED_LONG_FLAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"--(?:password|passwd|pass|pw)(?:[ \t]*=[ \t]*|[ \t]+){FLAG_VALUE}"
    ))
    .expect("long password flag pattern")
});

// Short `-p` is only a password flag for the tools that define it that way;
// `mkdir -p`, `sudo -p`, and `docker -p` are not credentials, so the tool name
// is part of the pattern rather than a guess about the value. The flag must also
// start its own argument, or the `-p` inside `--password` would match here after
// the long-flag rule had already masked that value.
static CRED_SHORT_FLAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"\b(?:sshpass|mysql|mysqldump|mysqladmin|psql|mongosh|mongo|redis-cli|smbclient|vncviewer|mosquitto_pub|mosquitto_sub)\b[^\n;|]{{0,120}}?[ \t]-p[ \t]?{FLAG_VALUE}"
    ))
    .expect("short password flag pattern")
});

// A JSON or object key that names a secret, holding a quoted value: this is how
// Weles form fills, Skarbiec payloads, and API request bodies appear in tool
// calls (`"login_password": "<secret>"`).
static CRED_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)[A-Za-z0-9_.\[\]-]*(?:password|passwd|passphrase|secret|api[_-]?key|access[_-]?token){QUOTE}?[ \t]*[:=][ \t]*{QUOTE}(?P<val>{VALUE}){QUOTE}"
    ))
    .expect("credential key pattern")
});

// A browser form-fill descriptor, where the secret sits in a neighbouring value
// field and the key itself is innocent: `{"type":"password","val":"<secret>"}`.
static CRED_FORM_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)type{QUOTE}?[ \t]*:[ \t]*{QUOTE}password{QUOTE}[ \t]*,[ \t]*{QUOTE}val(?:ue)?{QUOTE}?[ \t]*:[ \t]*{QUOTE}(?P<val>{VALUE}){QUOTE}"
    ))
    .expect("form field credential pattern")
});

/// Value capture names, in the order `sub_value` prefers them.
const VALUE_GROUPS: [&str; 2] = ["val", "bare"];

/// Per-class hit counts reported by stream commits and recovery replay.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MaskCounts {
    pub token: u64,
    pub entropy: u64,
    pub assignment: u64,
    pub credential: u64,
}

#[derive(Debug, Default)]
pub struct Masker {
    counts: MaskCounts,
}

fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex = format!("{digest:x}");
    hex[..FP_LEN].to_string()
}

fn marker(class: &str, value: &str) -> String {
    format!(
        "[masked:{class}:{}:{}]",
        value.chars().count(),
        fingerprint(value)
    )
}

/// High-entropy filter: long enough, many distinct characters, and drawing on
/// several character groups at once. Lowercase prose (one group) and plain hex
/// digests (two groups, sixteen distinct at most) both fail this on purpose.
fn dense_enough(run: &str) -> bool {
    if run.chars().count() < RUN_MIN {
        return false;
    }
    let mut distinct: Vec<char> = run.chars().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < DISTINCT_MIN {
        return false;
    }
    let has_lower = run.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = run.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = run.chars().any(|c| c.is_ascii_digit());
    let has_symbol = run
        .chars()
        .any(|c| matches!(c, '+' | '/' | '=' | '_' | '-'));
    let groups = [has_lower, has_upper, has_digit, has_symbol]
        .into_iter()
        .filter(|hit| *hit)
        .count();
    groups >= GROUPS_MIN
}

/// A credential shape only hides something when its value is a literal. A shell
/// variable, a command substitution, an empty value, or a marker from an earlier
/// pass either carries no secret or already carries none, and masking those would
/// delete evidence while protecting nothing. Rejecting markers here is also what
/// makes the credential rules idempotent.
fn credential_literal(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('$')
        && !value.contains("$(")
        && !value.contains("${")
        && !value.contains('`')
        && !value.contains("[masked:")
}

impl Masker {
    pub fn new() -> Self {
        Self::default()
    }

    fn sub(&mut self, text: &str, re: &Regex, class: &str, guarded: bool) -> String {
        let mut hits = 0u64;
        let out = re.replace_all(text, |caps: &Captures<'_>| {
            let hit = &caps[0];
            if guarded && !dense_enough(hit) {
                return hit.to_string();
            }
            hits += 1;
            marker(class, hit)
        });
        match class {
            "token" => self.counts.token += hits,
            "entropy" => self.counts.entropy += hits,
            _ => self.counts.assignment += hits,
        }
        out.into_owned()
    }

    /// Replaces only the captured value of a credential shape, leaving the
    /// command or key that carried it intact.
    fn sub_value(&mut self, text: &str, re: &Regex) -> String {
        let mut hits = 0u64;
        let out = re.replace_all(text, |caps: &Captures<'_>| {
            let whole = caps.get(0).expect("whole match");
            let value = VALUE_GROUPS
                .iter()
                .filter_map(|name| caps.name(name))
                .next();
            let Some(value) = value.filter(|value| credential_literal(value.as_str())) else {
                return whole.as_str().to_string();
            };
            hits += 1;
            let head = value.start() - whole.start();
            let tail = value.end() - whole.start();
            format!(
                "{}{}{}",
                &whole.as_str()[..head],
                marker("credential", value.as_str()),
                &whole.as_str()[tail..]
            )
        });
        self.counts.credential += hits;
        out.into_owned()
    }

    /// Order matters. Credential syntax runs first, because its context proves a
    /// value is a secret even when the value itself looks ordinary, and because
    /// it must see the original quoting before another class rewrites part of it.
    /// Then whole assignments, provider-shaped tokens, and leftover dense runs,
    /// so each remaining secret is attributed to its richest class.
    pub fn mask(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let out = self.sub_value(text, &CRED_STDIN_RE);
        let out = self.sub_value(&out, &CRED_EXPECT_SEND_RE);
        let out = self.sub_value(&out, &CRED_FORM_FIELD_RE);
        let out = self.sub_value(&out, &CRED_KEY_RE);
        let out = self.sub_value(&out, &CRED_LONG_FLAG_RE);
        let out = self.sub_value(&out, &CRED_SHORT_FLAG_RE);
        let out = self.sub(&out, &ASSIGN_RE, "assignment", false);
        let out = self.sub(&out, &TOKEN_RE, "token", false);
        self.sub(&out, &ENTROPY_RE, "entropy", true)
    }

    pub fn counts(&self) -> MaskCounts {
        self.counts
    }
}

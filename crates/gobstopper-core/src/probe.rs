//! Probe-based quality scoring for compaction strategies.
//!
//! A *probe* is a short verbatim string lifted from the pre-compaction
//! transcript — a path, a command, a decision phrase, an error
//! signature, a file name, or a long identifier. Scoring checks which
//! probes still appear verbatim in the rewritten text: a cheap,
//! deterministic proxy for "did the compaction destroy what the session
//! still needs". Pure text in, score out — no I/O, no provider parsing,
//! no model calls.
//!
//! Callers own the policy: which raw transcript lines count as live
//! context (the normalized [`crate::model::Transcript`] carries no
//! payload text, so extraction runs on the raw store text and callers
//! keep or drop probes by `line_index`), and where the protected tail
//! begins is an argument to [`score_probes`].

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Hard cap on extracted probes so one noisy transcript cannot dominate
/// the score or eval runtime.
pub const MAX_PROBES: usize = 64;
/// Hard cap on a single probe's length in bytes.
pub const MAX_PROBE_LEN: usize = 64;
/// Hard cap on missed-probe strings echoed in [`ProbeScore`] for
/// debugging — bounded because they are transcript content.
pub const MAX_MISSED: usize = 8;

/// Per-kind caps applied before the global cap, indexed by
/// [`ProbeKind`] discriminant, so a transcript heavy in one category
/// (a build log full of paths) cannot starve the rest.
const KIND_CAP: [usize; 6] = [24, 16, 12, 16, 16, 16];

/// At most this many probes of one kind from a single line — a
/// directory listing should not become twenty near-identical probes.
const PER_LINE_MAX: usize = 4;

/// Minimum length for an identifier probe; shorter words are prose.
const MIN_IDENT_LEN: usize = 10;

/// Command words that open a command probe. A probe is the word plus
/// its argument run, e.g. `cargo test --workspace`.
const COMMANDS: &[&str] = &[
    "cargo", "rustc", "rustup", "git", "bun", "npm", "npx", "yarn", "pnpm", "node", "deno",
    "python", "python3", "pip", "uv", "pytest", "go", "make", "docker", "kubectl", "brew", "curl",
];

/// Lowercased phrases that open a decision probe.
const DECISION_KEYS: &[&str] = &[
    "decided",
    "decide",
    "decision",
    "will use",
    "chose",
    "chosen",
    "going with",
];

/// Case-sensitive error-signature openers.
const ERROR_KEYS: &[&str] = &["error[", "error: ", "FAILED", "panicked"];

/// Extensions a bare `name.ext` token must carry to count as a
/// file-name probe. Path probes allow any extension; this list only
/// bounds the noisier basename case.
const FILE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "rb", "java", "kt", "c", "h", "cc",
    "cpp", "hpp", "cs", "swift", "toml", "json", "jsonl", "yaml", "yml", "md", "txt", "sh", "sql",
    "html", "css", "xml", "lock", "cfg", "ini",
];

/// What kind of verbatim string a probe carries. [`ProbeScore::by_kind`]
/// reports recall in this declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeKind {
    /// `/...`, `~/...`, `./...`, or `relative/dir/name.ext` token.
    Path,
    /// A shell command word plus its argument run (`cargo test`).
    Command,
    /// A keyword phrase (`decided`, `will use`, `chose`) plus context.
    Decision,
    /// `error[...]`, `error: ...`, `FAILED`, or `panicked` window.
    ErrorSignature,
    /// `name.ext` basename token.
    FileName,
    /// Long `snake_case`/`camelCase` word that is not a bare JSON token.
    Identifier,
}

impl ProbeKind {
    const ALL: [ProbeKind; 6] = [
        ProbeKind::Path,
        ProbeKind::Command,
        ProbeKind::Decision,
        ProbeKind::ErrorSignature,
        ProbeKind::FileName,
        ProbeKind::Identifier,
    ];
}

/// One extracted probe. In-memory only — the serialized score carries
/// counts, ratios, and a bounded missed-sample, never the probe list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub kind: ProbeKind,
    /// The verbatim string checked for presence in rewritten text.
    /// At most [`MAX_PROBE_LEN`] bytes.
    pub text: String,
    /// Zero-based line of the raw transcript text it was lifted from.
    pub line_index: usize,
}

/// Recall tally for one probe kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindTally {
    pub kind: ProbeKind,
    pub total: usize,
    pub recalled: usize,
}

/// Quality score of a rewritten transcript against the probe set
/// extracted from the source: counts and ratios plus a bounded sample
/// of missed probe strings (≤ [`MAX_MISSED`] entries, each already ≤
/// [`MAX_PROBE_LEN`] bytes). Comparable across strategies evaluated on
/// the same source because they share one probe set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeScore {
    /// Probes extracted from the source transcript.
    pub probes_total: usize,
    /// Probes still present verbatim in the rewritten text.
    pub probes_recalled: usize,
    /// `probes_recalled / probes_total`; 1.0 when there were no probes.
    pub recall: f64,
    /// Probes whose line falls in the protected tail.
    pub tail_probes_total: usize,
    /// Tail probes still present verbatim.
    pub tail_probes_recalled: usize,
    /// Every tail probe survived — the recent context the strategy
    /// promises to keep is verbatim-intact. Vacuously true when the
    /// tail carried no probes.
    pub tail_intact: bool,
    /// Bounded sample of missed probe strings, in transcript order.
    pub missed_probes: Vec<String>,
    /// Per-kind recall, one entry per kind in declaration order.
    pub by_kind: Vec<KindTally>,
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_path_char(b: u8) -> bool {
    is_word(b) || matches!(b, b'-' | b'.' | b'/' | b'~' | b'+' | b'@')
}

fn is_fname_char(b: u8) -> bool {
    is_word(b) || matches!(b, b'-' | b'.')
}

fn is_cmd_arg(b: u8) -> bool {
    is_word(b)
        || matches!(
            b,
            b'-' | b'.' | b'/' | b'~' | b'+' | b'=' | b':' | b'%' | b'@'
        )
}

/// Clamp `end` back to a UTF-8 boundary so window probes never split a
/// multibyte character.
fn clamp_boundary(s: &str, mut end: usize) -> usize {
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Verbatim window of at most `max_len` bytes starting at `start`,
/// trimmed of trailing whitespace and sentence dots.
fn window(line: &str, start: usize, max_len: usize) -> &str {
    let end = clamp_boundary(line, (start + max_len).min(line.len()));
    line[start..end].trim_end_matches([' ', '.'])
}

/// Truncate a probe candidate to [`MAX_PROBE_LEN`] on a char boundary.
fn truncate_at(text: &str, max_len: usize) -> &str {
    if text.len() <= max_len {
        text
    } else {
        &text[..clamp_boundary(text, max_len)]
    }
}

/// Per-line, per-kind collection state shared by the line scanners.
struct Sink<'a> {
    seen: &'a mut HashSet<String>,
    out: &'a mut Vec<Probe>,
    cap: usize,
    line_index: usize,
    per_line: usize,
}

impl Sink<'_> {
    fn full(&self) -> bool {
        self.out.len() >= self.cap || self.per_line >= PER_LINE_MAX
    }

    fn push(&mut self, kind: ProbeKind, text: &str) {
        let text = truncate_at(text, MAX_PROBE_LEN);
        if self.full() || text.len() < 3 || !self.seen.insert(text.to_string()) {
            return;
        }
        self.out.push(Probe {
            kind,
            text: text.to_string(),
            line_index: self.line_index,
        });
        self.per_line += 1;
    }
}

/// `name.ext` where ext is alphanumeric and 1–8 chars — restricted to
/// `known` when given, any extension otherwise.
fn has_ext(base: &str, known: Option<&[&str]>) -> bool {
    let Some(dot) = base.rfind('.') else {
        return false;
    };
    let (stem, ext) = (&base[..dot], &base[dot + 1..]);
    if stem.is_empty()
        || !stem
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
        || ext.is_empty()
        || ext.len() > 8
        || !ext.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return false;
    }
    match known {
        Some(list) => list.iter().any(|e| ext.eq_ignore_ascii_case(e)),
        None => true,
    }
}

/// A path token is absolute/anchored (`/`, `~/`, `./`) or a relative
/// `dir/name.ext` shape.
fn is_path_token(tok: &str) -> bool {
    if tok.len() < 3 || !tok.contains('/') {
        return false;
    }
    if tok.starts_with('/') || tok.starts_with("~/") || tok.starts_with("./") {
        return true;
    }
    let base = tok.rsplit('/').next().unwrap_or("");
    has_ext(base, None)
}

fn scan_paths(line: &str, sink: &mut Sink) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && !sink.full() {
        if is_path_char(b[i]) {
            let start = i;
            while i < b.len() && is_path_char(b[i]) {
                i += 1;
            }
            let tok = line[start..i].trim_end_matches('.');
            if is_path_token(tok) {
                sink.push(ProbeKind::Path, tok);
            }
        } else {
            i += 1;
        }
    }
}

fn scan_filenames(line: &str, sink: &mut Sink) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && !sink.full() {
        if is_fname_char(b[i]) {
            let start = i;
            while i < b.len() && is_fname_char(b[i]) {
                i += 1;
            }
            let tok = line[start..i].trim_end_matches('.');
            if has_ext(tok, Some(FILE_EXTS)) {
                sink.push(ProbeKind::FileName, tok);
            }
        } else {
            i += 1;
        }
    }
}

fn scan_commands(line: &str, sink: &mut Sink) {
    let b = line.as_bytes();
    for cmd in COMMANDS {
        for (start, _) in line.match_indices(cmd) {
            if sink.full() {
                return;
            }
            // Not a word fragment or path component (`mycargo`, `/git`).
            if start > 0 && is_path_char(b[start - 1]) {
                continue;
            }
            // Require at least one space then an argument run.
            if b.get(start + cmd.len()) != Some(&b' ') {
                continue;
            }
            let mut j = start + cmd.len();
            while j < b.len() && (b[j] == b' ' || is_cmd_arg(b[j])) {
                j += 1;
            }
            let end = clamp_boundary(line, j.min(start + MAX_PROBE_LEN));
            let text = line[start..end].trim_end_matches([' ', '.']);
            if text[cmd.len()..].trim().len() >= 2 {
                sink.push(ProbeKind::Command, text);
            }
        }
    }
}

fn scan_decisions(line: &str, sink: &mut Sink) {
    // `to_ascii_lowercase` folds only ASCII, so byte offsets in the
    // lowered copy still line up with `line`.
    let lowered = line.to_ascii_lowercase();
    for key in DECISION_KEYS {
        for (start, _) in lowered.match_indices(key) {
            if sink.full() {
                return;
            }
            sink.push(ProbeKind::Decision, window(line, start, MAX_PROBE_LEN));
        }
    }
}

fn scan_errors(line: &str, sink: &mut Sink) {
    for key in ERROR_KEYS {
        for (start, _) in line.match_indices(key) {
            if sink.full() {
                return;
            }
            sink.push(
                ProbeKind::ErrorSignature,
                window(line, start, MAX_PROBE_LEN),
            );
        }
    }
}

/// `"token"` — quoted single words in JSONL are provider enum values
/// and keys (`"response_item"`, `"tool_use_id":`), not session content
/// worth probing.
fn is_quoted_token(b: &[u8], start: usize, end: usize) -> bool {
    start > 0 && b[start - 1] == b'"' && end < b.len() && b[end] == b'"'
}

fn scan_identifiers(line: &str, sink: &mut Sink) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && !sink.full() {
        if is_word(b[i]) {
            let start = i;
            while i < b.len() && is_word(b[i]) {
                i += 1;
            }
            let tok = &line[start..i];
            let identifierish = tok.contains('_')
                || (tok.bytes().any(|b| b.is_ascii_uppercase())
                    && tok.bytes().any(|b| b.is_ascii_lowercase()));
            if tok.len() >= MIN_IDENT_LEN
                && !b[start].is_ascii_digit()
                && identifierish
                && !is_quoted_token(b, start, i)
            {
                sink.push(ProbeKind::Identifier, tok);
            }
        } else {
            i += 1;
        }
    }
}

/// Extract the bounded, deterministic probe set for `text` — same input
/// always yields the same probes in the same order (transcript order,
/// kind-tagged). Callers filter by `line_index` to restrict extraction
/// to lines that carry live context.
pub fn extract_probes(text: &str) -> Vec<Probe> {
    let scanners: [fn(&str, &mut Sink); 6] = [
        scan_paths,
        scan_commands,
        scan_decisions,
        scan_errors,
        scan_filenames,
        scan_identifiers,
    ];
    let mut seen = HashSet::new();
    let mut buckets: [Vec<Probe>; 6] = Default::default();
    for (line_index, line) in text.lines().enumerate() {
        for (k, scan) in scanners.iter().enumerate() {
            let mut sink = Sink {
                seen: &mut seen,
                out: &mut buckets[k],
                cap: KIND_CAP[k],
                line_index,
                per_line: 0,
            };
            scan(line, &mut sink);
        }
    }

    // Round-robin across kinds so the global cap leaves a mixed set,
    // then back to transcript order for readable missed samples.
    let mut probes = Vec::new();
    let mut cursors = [0usize; 6];
    while probes.len() < MAX_PROBES {
        let mut progressed = false;
        for (k, bucket) in buckets.iter().enumerate() {
            if cursors[k] < bucket.len() {
                probes.push(bucket[cursors[k]].clone());
                cursors[k] += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    probes.sort_by_key(|p| (p.line_index, p.kind as usize));
    probes
}

/// Score `post_text` against `probes`: verbatim recall overall and over
/// the protected tail (probes with `line_index >= tail_start_line`).
/// Pass `usize::MAX` for `tail_start_line` when nothing is protected.
pub fn score_probes(probes: &[Probe], post_text: &str, tail_start_line: usize) -> ProbeScore {
    let mut recalled = 0usize;
    let mut tail_total = 0usize;
    let mut tail_recalled = 0usize;
    let mut missed_probes = Vec::new();
    let mut kind_total = [0usize; 6];
    let mut kind_recalled = [0usize; 6];

    for p in probes {
        let k = p.kind as usize;
        kind_total[k] += 1;
        let in_tail = p.line_index >= tail_start_line;
        if in_tail {
            tail_total += 1;
        }
        if post_text.contains(p.text.as_str()) {
            recalled += 1;
            kind_recalled[k] += 1;
            if in_tail {
                tail_recalled += 1;
            }
        } else if missed_probes.len() < MAX_MISSED {
            missed_probes.push(p.text.clone());
        }
    }

    let probes_total = probes.len();
    ProbeScore {
        probes_total,
        probes_recalled: recalled,
        recall: if probes_total == 0 {
            1.0
        } else {
            recalled as f64 / probes_total as f64
        },
        tail_probes_total: tail_total,
        tail_probes_recalled: tail_recalled,
        tail_intact: tail_recalled == tail_total,
        missed_probes,
        by_kind: ProbeKind::ALL
            .iter()
            .enumerate()
            .map(|(i, kind)| KindTally {
                kind: *kind,
                total: kind_total[i],
                recalled: kind_recalled[i],
            })
            .collect(),
    }
}

/// Extract probes from `pre_text` and score `post_text` in one call.
/// `tail_start_line` is the first line of the protected tail.
pub fn score_text(pre_text: &str, post_text: &str, tail_start_line: usize) -> ProbeScore {
    score_probes(&extract_probes(pre_text), post_text, tail_start_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(probes: &[Probe], kind: ProbeKind, needle: &str) -> bool {
        probes
            .iter()
            .any(|p| p.kind == kind && p.text.contains(needle))
    }

    #[test]
    fn extraction_is_deterministic_and_typed() {
        let text = concat!(
            "user: fix /Users/bg/project/src/main.rs then run cargo test --workspace\n",
            "assistant: ran git status; decided to use the sawtooth strategy\n",
            "tool: error[E0308]: mismatched types in crates/core/src/probe.rs FAILED\n",
            "tool: read config file gobstopper.toml for eval_transcript settings\n",
        );
        let a = extract_probes(text);
        let b = extract_probes(text);
        assert_eq!(a, b, "extraction must be deterministic");
        assert!(has(&a, ProbeKind::Path, "/Users/bg/project/src/main.rs"));
        assert!(has(&a, ProbeKind::Command, "cargo test --workspace"));
        assert!(has(&a, ProbeKind::Command, "git status"));
        assert!(has(&a, ProbeKind::Decision, "decided to use the sawtooth"));
        assert!(has(&a, ProbeKind::ErrorSignature, "error[E0308]"));
        assert!(has(&a, ProbeKind::Path, "crates/core/src/probe.rs"));
        assert!(has(&a, ProbeKind::FileName, "gobstopper.toml"));
        assert!(has(&a, ProbeKind::Identifier, "eval_transcript"));
    }

    #[test]
    fn probes_are_bounded() {
        // 200 distinct paths spread over 50 lines: kind caps bind.
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("/dir{:03}/file{i}.rs ", i % 20));
            if i % 4 == 3 {
                text.push('\n');
            }
        }
        let probes = extract_probes(&text);
        assert!(probes.len() <= MAX_PROBES, "{}", probes.len());
        assert!(probes.iter().all(|p| p.text.len() <= MAX_PROBE_LEN));
        // Dedupe: same text on two lines yields one probe.
        let dupes = extract_probes("x /a/b/c.rs\ny /a/b/c.rs\n");
        assert_eq!(dupes.iter().filter(|p| p.text == "/a/b/c.rs").count(), 1);
    }

    #[test]
    fn scoring_counts_verbatim_recall_and_tail() {
        let text = "kept /a/b/kept.rs here\nlost /a/b/lost.rs there\n";
        let probes = extract_probes(text);
        // Line 1 is the protected tail; the rewrite keeps line 0 only.
        let score = score_probes(&probes, "kept /a/b/kept.rs here\n[elided]\n", 1);
        assert_eq!(score.probes_total, 4); // two paths + two basenames
        assert_eq!(score.probes_recalled, 2);
        assert_eq!(score.recall, 0.5);
        assert_eq!(score.tail_probes_total, 2);
        assert_eq!(score.tail_probes_recalled, 0);
        assert!(!score.tail_intact);
        assert_eq!(score.missed_probes.len(), 2);
        assert!(score.by_kind.iter().all(|k| k.total == 2 || k.total == 0));
    }

    #[test]
    fn elided_region_loses_probes_but_tail_survives() {
        let text = concat!(
            "tool out: error[E0308] at /old/stale.rs FAILED\n",
            "assistant: decided to use sawtooth for /old/x.rs\n",
            "tool out: cargo build --release on /new/tail.rs\n",
        );
        let probes = extract_probes(text);
        let post = concat!(
            "tool out: [output elided by gobstopper: 512 bytes]\n",
            "assistant: decided to use sawtooth for /old/x.rs\n",
            "tool out: cargo build --release on /new/tail.rs\n",
        );
        let score = score_probes(&probes, post, 2);
        assert!(score.probes_recalled < score.probes_total);
        assert!(score.recall < 1.0 && score.recall > 0.0);
        assert!(score.tail_intact, "{:?}", score.missed_probes);
        assert_eq!(score.tail_probes_recalled, score.tail_probes_total);
        assert!(score
            .missed_probes
            .iter()
            .any(|m| m.contains("error[E0308]")));
    }

    #[test]
    fn digest_mentions_recall_probes() {
        let text = "tool out: wrote /project/src/eval.rs with eval_transcript\n";
        let post = concat!(
            "tool out: [output elided by gobstopper: 88 bytes]\n",
            "user: [gobstopper state card]\nfile: /project/src/eval.rs\n",
        );
        let score = score_text(text, post, usize::MAX);
        // Path and basename survive via the digest; the identifier died.
        assert!(post.contains("/project/src/eval.rs"));
        assert!(score
            .missed_probes
            .iter()
            .any(|m| m.contains("eval_transcript")));
        assert!(score.recall < 1.0 && score.recall > 0.0);
        assert_eq!(score.tail_probes_total, 0);
        assert!(score.tail_intact); // vacuous: no tail probes
    }

    #[test]
    fn empty_text_scores_perfect() {
        let score = score_text("", "", 0);
        assert_eq!(score.probes_total, 0);
        assert_eq!(score.recall, 1.0);
        assert!(score.tail_intact);
        assert!(score.missed_probes.is_empty());
    }

    #[test]
    fn missed_sample_is_capped() {
        let mut text = String::new();
        for i in 0..40 {
            text.push_str(&format!("/elided/dir/item{i}.rs\n"));
        }
        let score = score_text(&text, "", 0);
        assert!(score.missed_probes.len() <= MAX_MISSED);
        assert_eq!(score.probes_recalled, 0);
    }
}

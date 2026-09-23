//! Probe-based quality scoring for compaction strategies.
//!
//! A *probe* is a short verbatim string lifted from the pre-compaction
//! transcript — a path, a command, a decision phrase, an error
//! signature, a file name, or a long identifier. Scoring checks which
//! probes still appear verbatim in the rewritten text: a cheap,
//! measurement of textual retention. It does not establish whether an
//! instruction is obeyed, a fact is true, or a continuation task succeeds. Pure text in, score out — no I/O, no provider parsing,
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

/// Interpretation of a probe score. A model judgment is an unqualified
/// estimate, not independently established semantic equivalence or task success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreBasis {
    #[default]
    Literal,
    ModelJudgment,
    LiteralAndModelJudgment,
}

/// Wilson 95% interval for the recorded binary tally. This descriptive
/// binomial interval does not correct heuristic selection or correlated probes.
pub fn wilson_interval(successes: u64, total: u64) -> Option<[f64; 2]> {
    if total == 0 || successes > total {
        return None;
    }
    let n = total as f64;
    let p = successes as f64 / n;
    let z2 = 3.841458820694124;
    let center = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let margin = 1.959963984540054 * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt() / (1.0 + z2 / n);
    Some([(center - margin).max(0.0), (center + margin).min(1.0)])
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
    /// `probes_recalled / probes_total`; legacy numeric slot is zero when
    /// unavailable. Consumers must check `recall_available`.
    pub recall: f64,
    #[serde(default)]
    pub basis: ScoreBasis,
    #[serde(default)]
    pub probes_requested: usize,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub recall_available: bool,
    #[serde(default)]
    pub recall_wilson95: Option<[f64; 2]>,
    /// Probes whose line falls in the protected tail.
    pub tail_probes_total: usize,
    /// Tail probes still present verbatim.
    pub tail_probes_recalled: usize,
    /// All requested tail probes were evaluated and survived, and at least
    /// one tail probe exists. This says nothing about unprobed tail content.
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
            if probes.len() == MAX_PROBES {
                break;
            }
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
    score_observations(probes, tail_start_line, ScoreBasis::Literal, |_, probe| {
        Some(post_text.contains(&probe.text))
    })
}

fn score_observations(
    probes: &[Probe],
    tail_start_line: usize,
    basis: ScoreBasis,
    observe: impl Fn(usize, &Probe) -> Option<bool>,
) -> ProbeScore {
    let mut recalled = 0;
    let mut total = 0;
    let mut tail_total = 0;
    let mut tail_recalled = 0;
    let mut tail_complete = true;
    let mut missed_probes = Vec::new();
    let mut kind_total = [0; 6];
    let mut kind_recalled = [0; 6];
    for (index, probe) in probes.iter().take(MAX_PROBES).enumerate() {
        let in_tail = probe.line_index >= tail_start_line;
        let observed = (!probe.text.is_empty() && probe.text.len() <= MAX_PROBE_LEN)
            .then(|| observe(index, probe))
            .flatten();
        let Some(survived) = observed else {
            tail_complete &= !in_tail;
            continue;
        };
        total += 1;
        let k = probe.kind as usize;
        kind_total[k] += 1;
        if in_tail {
            tail_total += 1;
        }
        if survived {
            recalled += 1;
            kind_recalled[k] += 1;
            if in_tail {
                tail_recalled += 1;
            }
        } else if missed_probes.len() < MAX_MISSED {
            missed_probes.push(probe.text.clone());
        }
    }
    let complete = total == probes.len();
    ProbeScore {
        probes_total: total,
        probes_recalled: recalled,
        recall: if total == 0 {
            0.0
        } else {
            recalled as f64 / total as f64
        },
        basis,
        probes_requested: probes.len(),
        complete,
        recall_available: total > 0,
        recall_wilson95: wilson_interval(recalled as u64, total as u64),
        tail_probes_total: tail_total,
        tail_probes_recalled: tail_recalled,
        tail_intact: tail_complete && complete && tail_total > 0 && tail_recalled == tail_total,
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

/// Semantic probe judge: a model-backed checker that answers "is this
/// fact still present or established in the post-rewrite context?"
/// where verbatim matching fails — facts preserved as paraphrase in a
/// state card or per-item stub. Implementations must bound request
/// count and payload; `None` means the judge could not run (missing
/// credentials, unreachable backend), not "nothing survived".
pub trait ProbeJudge {
    /// Per-probe survival probability in `post_text`, covering a prefix
    /// of `probes` — a bounded judge may answer fewer than it was given.
    /// Invalid/nonfinite values are unavailable, never clamped into success.
    fn score(&self, probes: &[Probe], post_text: &str) -> Option<Vec<f64>>;
}

/// Build a [`ProbeScore`] from judge probabilities instead of verbatim
/// matching: a probe counts as recalled at probability >= 0.5. The
/// score covers the first `probs.len().min(probes.len())` probes — a
/// bounded judge reports on the prefix it evaluated, and
/// `probes_total` reflects the judged subset.
pub fn score_from_probabilities(
    probes: &[Probe],
    probs: &[f64],
    tail_start_line: usize,
) -> ProbeScore {
    score_observations(
        probes,
        tail_start_line,
        ScoreBasis::ModelJudgment,
        |index, _| {
            probs
                .get(index)
                .filter(|p| p.is_finite() && (0.0..=1.0).contains(*p))
                .map(|p| *p >= 0.5)
        },
    )
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
    fn global_cap_stops_mid_round_without_starving_late_kinds() {
        let mut text = String::new();
        for i in 0..24 {
            text.push_str(&format!("/project/file{i}.rs\n"));
        }
        for i in 0..16 {
            text.push_str(&format!("cargo test case{i}\n"));
            text.push_str(&format!("decided option {i}\n"));
            text.push_str(&format!("error: failure {i}\n"));
        }
        // This kind appears only at the end, after the other buckets fill.
        for i in 0..16 {
            text.push_str(&format!("context_marker_{i}\n"));
        }

        let probes = extract_probes(&text);
        assert_eq!(probes.len(), MAX_PROBES);
        assert_eq!(probes, extract_probes(&text));
        let counts = ProbeKind::ALL.map(|kind| probes.iter().filter(|p| p.kind == kind).count());
        assert_eq!(counts, [11, 11, 11, 11, 10, 10]);
        assert!(has(&probes, ProbeKind::Identifier, "context_marker_9"));
        assert!(!has(&probes, ProbeKind::Identifier, "context_marker_10"));
        assert!(probes.windows(2).all(|pair| {
            (pair[0].line_index, pair[0].kind as usize)
                <= (pair[1].line_index, pair[1].kind as usize)
        }));
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
        assert!(!score.tail_intact); // no observation of a protected tail
    }

    #[test]
    fn empty_text_has_no_recall_measurement() {
        let score = score_text("", "", 0);
        assert_eq!(score.probes_total, 0);
        assert_eq!(score.recall, 0.0);
        assert!(!score.recall_available);
        assert_eq!(score.recall_wilson95, None);
        assert!(!score.tail_intact);
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

#[cfg(test)]
mod probability_tests {
    use super::*;

    #[test]
    fn invalid_and_missing_judgments_are_incomplete_not_failures_or_successes() {
        let probes = vec![probe(ProbeKind::Path, "file.rs", 1); 5];
        let score = score_from_probabilities(&probes, &[1.0, f64::NAN, f64::INFINITY, -1.0], 0);
        assert_eq!(score.probes_requested, 5);
        assert_eq!(score.probes_total, 1);
        assert_eq!(score.probes_recalled, 1);
        assert!(!score.complete);
        assert!(!score.tail_intact);
        assert_eq!(score.basis, ScoreBasis::ModelJudgment);
        let interval = score.recall_wilson95.unwrap();
        assert!(interval[0] > 0.0 && interval[0] < 0.5 && interval[1] == 1.0);
        assert_eq!(wilson_interval(1, 0), None);
        assert_eq!(wilson_interval(2, 1), None);
    }

    #[test]
    fn direct_probe_api_bounds_external_inputs_and_does_not_establish_obedience() {
        let empty = probe(ProbeKind::Decision, "", 0);
        let oversized = probe(ProbeKind::Decision, &"x".repeat(MAX_PROBE_LEN + 1), 0);
        let score = score_probes(&[empty, oversized], "", 0);
        assert_eq!(score.probes_total, 0);
        assert!(!score.recall_available);
        assert!(!score.complete);
        let many = vec![probe(ProbeKind::Decision, "delete production", 0); MAX_PROBES + 1];
        let score = score_probes(&many, "Do NOT delete production", 0);
        assert_eq!(score.probes_recalled, MAX_PROBES);
        assert!(!score.complete);
        assert_eq!(score.basis, ScoreBasis::Literal); // substring presence is not permission
    }

    fn probe(kind: ProbeKind, text: &str, line: usize) -> Probe {
        Probe {
            kind,
            text: text.into(),
            line_index: line,
        }
    }

    #[test]
    fn score_from_probabilities_thresholds_and_tails() {
        let probes = vec![
            probe(ProbeKind::Path, "/a/one.rs", 1),
            probe(ProbeKind::Command, "cargo test", 5),
            probe(ProbeKind::Path, "/a/three.rs", 9),
            probe(ProbeKind::ErrorSignature, "error[E1]", 9),
        ];
        // Two survive (0.5 counts, 0.99 counts), two lost; tail = line >= 9.
        let score = score_from_probabilities(&probes, &[0.9, 0.1, 0.5, 0.2], 9);
        assert_eq!(score.probes_total, 4);
        assert_eq!(score.probes_recalled, 2);
        assert_eq!(score.recall, 0.5);
        assert_eq!(score.tail_probes_total, 2);
        assert_eq!(score.tail_probes_recalled, 1);
        assert!(!score.tail_intact);
        assert_eq!(score.missed_probes.len(), 2);
        // Out-of-range probabilities are unavailable, never promoted to success.
        let clamped = score_from_probabilities(&probes[..1], &[7.0], usize::MAX);
        assert_eq!(clamped.probes_recalled, 0);
        assert!(!clamped.complete);
        assert!(!clamped.recall_available);
    }

    #[test]
    fn score_from_probabilities_covers_judged_prefix_only() {
        let probes = vec![
            probe(ProbeKind::Path, "/a/one.rs", 1),
            probe(ProbeKind::Path, "/a/two.rs", 2),
            probe(ProbeKind::Path, "/a/three.rs", 3),
        ];
        // Judge answered one probe; the score is over the judged subset.
        let score = score_from_probabilities(&probes, &[1.0], usize::MAX);
        assert_eq!(score.probes_total, 1);
        assert_eq!(score.probes_recalled, 1);
        assert_eq!(score.recall, 1.0);
        assert!(!score.complete);
        assert_eq!(score.probes_requested, 3);
        // No judge answers is unavailable, not perfect retention.
        let empty = score_from_probabilities(&probes, &[], usize::MAX);
        assert_eq!(empty.probes_total, 0);
        assert_eq!(empty.recall, 0.0);
        assert!(!empty.recall_available);
    }
}

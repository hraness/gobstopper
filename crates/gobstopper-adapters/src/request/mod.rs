//! Request-time compaction of coding-agent API traffic.
//!
//! The rule is CliffCompaction's (Nguyen, Cho, Chen and Dettmers,
//! [arXiv:2609.26779](https://arxiv.org/abs/2609.26779); reference
//! implementation MIT-licensed at
//! <https://github.com/nguyenvuthientrang/cliffcompaction>), ported for
//! `gobstopper proxy`. Clients resend their whole history on every request.
//! When the outgoing request exceeds a token threshold, the history is sent
//! as `[head verbatim] + [one mechanical summary] + [newest turns verbatim]`.
//! The summary keeps short tool results, reduces tool calls to one-line
//! signatures, keeps human and assistant text, and drops long observations.
//! A previous summary is never summarized again: every compaction replays
//! the original history the client resent.
//!
//! Everything here is pure (JSON in, JSON out). The proxy in the CLI crate
//! owns sockets and upstream calls; the originals stay in the client's own
//! transcript, so nothing here needs the vault. The reference
//! implementation's MIT notice is in `THIRD_PARTY_NOTICES.md`.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ops::Range;

pub mod anthropic;
pub mod calibrate;
pub mod chat;
mod engine;
mod evidence;
mod images;
pub mod replay;
pub mod responses;
mod store;

pub use engine::{Engine, RequestCtx};
pub use evidence::{evidence_observations, EvidenceEntry, EvidenceObservation};
pub use store::{Entry, PrefixStore};

/// Marks an injected summary so re-compaction can drop it. Byte-identical to
/// the reference implementation's header, so a history that passed through
/// either proxy is recognized by both.
pub const SUMMARY_HEADER: &str =
    "The following is a summary of your previous actions (long observations omitted):";
const PART_SEPARATOR: &str = "\n\n---\n\n";

/// Default trigger: below the auto-compaction point of 200k-window clients,
/// so their own summarizer never fires. A request that declares a 1M-token
/// window can be given a larger threshold through [`Engine::prepare_at`];
/// `gobstopper proxy` does that for Anthropic requests whose
/// `anthropic-beta` header declares the window (`--threshold-1m`, 256,000
/// by default). A request that does not declare its window gets this one,
/// whatever its model's window.
pub const DEFAULT_THRESHOLD_TOKENS: u64 = 128_000;

/// Largest `keep_tail_percent` the engine applies. A larger share leaves
/// too little margin above the kept tail, so compaction would fire again
/// after a few small requests.
pub const MAX_KEEP_TAIL_PERCENT: u8 = 60;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CliffConfig {
    /// Compact when the estimated outgoing request exceeds this.
    pub threshold_tokens: u64,
    /// Newest assistant-step turns kept verbatim.
    pub keep_recent: usize,
    /// Share, in percent, of the room above the verbatim floor that the
    /// summary and kept tail may fill: the tail grows past `keep_recent`
    /// turns while it fits. 0 keeps exactly `keep_recent` turns; values
    /// above [`MAX_KEEP_TAIL_PERCENT`] are treated as that maximum.
    pub keep_tail_percent: u8,
    /// Cap on assistant text per summarized turn; 0 = unlimited.
    pub thought_max_chars: usize,
    /// Cap on a tool-call signature's serialized arguments.
    pub cmd_max_chars: usize,
    /// Tool results longer than this are dropped from the summary.
    pub result_max_chars: usize,
    /// Sanity cap on human text inside the summary.
    pub human_max_chars: usize,
    /// Fold readable thinking/reasoning into the summary as text.
    pub keep_thinking: bool,
    /// Cap on thinking text per summarized turn; 0 = unlimited.
    pub thinking_max_chars: usize,
    /// Characters of the conversation's words carried across compactions:
    /// the human's instructions (including messages queued while the agent
    /// worked, text typed after an interrupt and feedback typed when
    /// rejecting a tool call) and the assistant's visible replies, from
    /// every summarized turn. Each later summary shows them as one section,
    /// oldest first; when they exceed the budget, the oldest text drops out
    /// first. Tool calls, other tool output, thinking, skill bodies, shell
    /// output, system reminders and task notifications are never carried.
    /// The engine also caps the budget at a quarter of the request's
    /// headroom. 0 turns carrying off, and each summary then covers only
    /// the turns since the previous compaction.
    pub carry_max_chars: usize,
    /// Serialized-byte ceiling for original retained tool evidence. The
    /// token budget below applies independently, including native images.
    /// Zero disables evidence retention.
    pub evidence_max_bytes: usize,
    /// Billable-character ceiling for evidence (four characters per
    /// estimated token), additionally capped at a quarter of headroom.
    pub evidence_max_chars: usize,
    /// Text retained per result; longer results get labeled head/tail
    /// excerpts. Zero disables evidence retention.
    pub evidence_item_max_chars: usize,
    /// Refuse a request still over budget after the escalation ladder
    /// instead of sending it anyway.
    pub strict: bool,
}

impl Default for CliffConfig {
    fn default() -> Self {
        Self {
            threshold_tokens: DEFAULT_THRESHOLD_TOKENS,
            keep_recent: 3,
            keep_tail_percent: 0,
            thought_max_chars: 0,
            cmd_max_chars: 150,
            result_max_chars: 500,
            human_max_chars: 20_000,
            keep_thinking: true,
            thinking_max_chars: 0,
            carry_max_chars: 24_000,
            evidence_max_bytes: 256 * 1024,
            evidence_max_chars: 32_000,
            evidence_item_max_chars: 2_000,
            strict: false,
        }
    }
}

/// The API shapes the proxy compacts. Everything else passes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Anthropic Messages (`/v1/messages`): Claude Code.
    Anthropic,
    /// OpenAI Responses (`/responses`): Codex.
    Responses,
    /// OpenAI Chat Completions (`/chat/completions`): OpenAI-compatible
    /// clients such as opencode, Crush, Aider, and Goose.
    ChatCompletions,
}

impl Dialect {
    /// The dialect for a request path, or `None` for verbatim passthrough.
    /// Token-count probes (`/v1/messages/count_tokens`) and provider-side
    /// compaction (`/responses/compact`) are deliberately not matched.
    pub fn detect(path: &str) -> Option<Self> {
        let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
        if path.ends_with("/messages") {
            Some(Self::Anthropic)
        } else if path.ends_with("/responses") {
            Some(Self::Responses)
        } else if path.ends_with("/chat/completions") {
            Some(Self::ChatCompletions)
        } else {
            None
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Responses => "openai-responses",
            Self::ChatCompletions => "openai-chat",
        }
    }

    /// Request-body key holding the history.
    pub fn messages_key(self) -> &'static str {
        match self {
            Self::Anthropic | Self::ChatCompletions => "messages",
            Self::Responses => "input",
        }
    }

    /// Canonical content digest of one message, volatile fields excluded.
    pub fn digest(self, message: &Value) -> String {
        match self {
            Self::Anthropic => anthropic::digest_message(message),
            Self::Responses => responses::digest_message(message),
            Self::ChatCompletions => chat::digest_message(message),
        }
    }

    /// True for a model turn, which starts an assistant-step turn.
    pub fn is_assistant(self, message: &Value) -> bool {
        match self {
            Self::Anthropic => anthropic::is_assistant(message),
            Self::Responses => responses::is_assistant(message),
            Self::ChatCompletions => chat::is_assistant(message),
        }
    }

    /// True if the message is a previously injected summary.
    pub fn is_summary(self, message: &Value) -> bool {
        match self {
            Self::Anthropic => anthropic::is_summary_message(message),
            Self::Responses => responses::is_summary_message(message),
            Self::ChatCompletions => chat::is_summary_message(message),
        }
    }

    fn summarize(self, message: &Value, cfg: &CliffConfig) -> Vec<String> {
        match self {
            Self::Anthropic => anthropic::summarize_message(message, cfg),
            Self::Responses => responses::summarize_message(message, cfg),
            Self::ChatCompletions => chat::summarize_message(message, cfg),
        }
    }

    /// The conversation's words in `messages`, oldest first, for the
    /// carried section of later summaries.
    fn carry_parts(self, messages: &[Value]) -> Vec<String> {
        match self {
            Self::Anthropic => anthropic::carry_parts(messages),
            Self::Responses => responses::carry_parts(messages),
            Self::ChatCompletions => chat::carry_parts(messages),
        }
    }

    fn user_message(self, text: String) -> Value {
        match self {
            Self::Anthropic => anthropic::user_message(text),
            Self::Responses => responses::user_message(text),
            Self::ChatCompletions => chat::user_message(text),
        }
    }

    /// Messages that may not sit directly before the injected user-role
    /// summary, so they are trimmed off the head into the compacted region.
    fn trim_from_head(self, message: &Value) -> bool {
        match self {
            Self::Anthropic => anthropic::trim_from_head(message),
            Self::Responses | Self::ChatCompletions => false,
        }
    }

    /// Partition `body` into assistant-step turns, in order. Leading
    /// non-assistant messages (a prior summary) form their own group.
    /// A contiguous run of model output is one turn in every dialect: one
    /// model step may span several Responses items, and Claude Code can
    /// record one Anthropic step as two consecutive assistant messages
    /// (tool calls, then text), which the API merges into one turn. A split
    /// inside the run would separate the calls from their results.
    fn group_turns(self, body: &[Value]) -> Vec<Range<usize>> {
        group_by_starts(body, |i| {
            self.is_assistant(&body[i]) && (i == 0 || !self.is_assistant(&body[i - 1]))
        })
    }
}

fn group_by_starts(body: &[Value], starts_turn: impl Fn(usize) -> bool) -> Vec<Range<usize>> {
    let mut turns = Vec::new();
    let mut start = 0;
    for i in 1..body.len() {
        if starts_turn(i) {
            turns.push(start..i);
            start = i;
        }
    }
    if !body.is_empty() {
        turns.push(start..body.len());
    }
    turns
}

/// Result of one compaction step over a message list.
#[derive(Debug, Clone)]
pub struct CompactResult {
    pub messages: Vec<Value>,
    /// Number of verbatim head messages before the summary.
    pub head_len: usize,
    pub summary: Value,
    /// Index into the input: `input[cut..]` was kept verbatim.
    pub cut: usize,
    /// The conversation's words to carry into the next summary, oldest
    /// first: the carry this step rendered plus the words of the turns it
    /// summarized, bounded. Empty when carrying is off.
    pub carry: Vec<String>,
}

/// One cliff step: `[head] + [summary of older turns] + [newest turns]`.
/// `None` when there is no assistant turn yet, too few turns, or no
/// reduction in message count.
pub fn compact(messages: &[Value], dialect: Dialect, cfg: &CliffConfig) -> Option<CompactResult> {
    compact_within(messages, dialect, cfg, 0, &[], 0)
}

/// [`compact`] with a kept tail that grows past `keep_recent` turns, one
/// whole turn at a time, while the summary and the kept tail together still
/// fit `budget_chars` (engine sizes: billable characters plus 2 per
/// message). The tail never drops below `keep_recent` turns, at least one
/// assistant-started turn stays summarized, and only splits that still
/// reduce the message count are considered, so a budget never turns a
/// `Some` into `None`. Budget 0 is exactly [`compact`].
///
/// `carry` is the previous step's carried words. The summary shows
/// `bound_carry(carry, carry_budget)` as its first part, after the header,
/// and the result's carry adds the words of the turns this step
/// summarizes. With an empty carry the summary is exactly the one without
/// carrying, and with `carry_budget` 0 the result is exactly the one
/// without carrying.
fn compact_within(
    messages: &[Value],
    dialect: Dialect,
    cfg: &CliffConfig,
    budget_chars: usize,
    carry: &[String],
    carry_budget: usize,
) -> Option<CompactResult> {
    let first_assistant = messages.iter().position(|m| dialect.is_assistant(m))?;
    let mut head_len = first_assistant;
    while head_len > 0
        && (dialect.is_summary(&messages[head_len - 1])
            || dialect.trim_from_head(&messages[head_len - 1]))
    {
        head_len -= 1;
    }
    let body = &messages[head_len..];
    let turns = dialect.group_turns(body);
    if turns.len() <= cfg.keep_recent {
        return None;
    }
    let min_split = turns.len() - cfg.keep_recent;
    // Each turn is summarized once; a shorter split reuses a prefix.
    let turn_parts: Vec<Vec<String>> = turns[..min_split]
        .iter()
        .map(|turn| {
            body[turn.clone()]
                .iter()
                .flat_map(|message| dialect.summarize(message, cfg))
                .collect()
        })
        .collect();
    let carried = bound_carry(carry, carry_budget);
    let section = carry_section(carried);
    let split = if budget_chars == 0 {
        min_split
    } else {
        extend_split(
            body,
            dialect,
            &turns,
            &turn_parts,
            section.as_deref(),
            budget_chars,
        )
    };
    let parts: Vec<&str> = section
        .iter()
        .map(String::as_str)
        .chain(turn_parts[..split].iter().flatten().map(String::as_str))
        .collect();
    let text = if parts.is_empty() {
        SUMMARY_HEADER.to_string()
    } else {
        format!("{SUMMARY_HEADER}\n\n{}", parts.join(PART_SEPARATOR))
    };
    let summary = dialect.user_message(text);
    let kept_start = head_len + turns.get(split).map_or(body.len(), |turn| turn.start);
    let kept = &messages[kept_start..];
    if head_len + 1 + kept.len() >= messages.len() {
        return None;
    }
    let carry = if carry_budget == 0 {
        Vec::new()
    } else {
        let mut all = carried.to_vec();
        all.extend(dialect.carry_parts(&messages[head_len..kept_start]));
        bound_carry(&all, carry_budget).to_vec()
    };
    let mut out = Vec::with_capacity(head_len + 1 + kept.len());
    out.extend_from_slice(&messages[..head_len]);
    out.push(summary.clone());
    out.extend_from_slice(kept);
    Some(CompactResult {
        messages: out,
        head_len,
        summary,
        cut: kept_start,
        carry,
    })
}

/// The carried parts to keep under `budget`: the longest suffix of whole
/// parts whose cost fits, each part costing its characters plus 2 for the
/// blank line that joins it. The walk stops at the first part that does not
/// fit, which gives `bound(bound(a) ++ b) == bound(a ++ b)`: a chain that
/// carries its words forward step by step keeps the same parts as one step
/// over the same turns. Skipping a part that does not fit and taking older
/// ones would break that.
fn bound_carry(parts: &[String], budget: usize) -> &[String] {
    let mut used = 0usize;
    let mut start = parts.len();
    while start > 0 {
        let cost = parts[start - 1].chars().count() + 2;
        if cost > budget - used {
            break;
        }
        used += cost;
        start -= 1;
    }
    &parts[start..]
}

/// The carried section of a summary: the label line, then the parts joined
/// by blank lines. `None` for no parts. Carried parts hold no `---` line,
/// so the section is exactly one summary part.
fn carry_section(parts: &[String]) -> Option<String> {
    (!parts.is_empty()).then(|| format!("{CARRY_LABEL}\n{}", parts.join("\n\n")))
}

/// The split for [`compact_within`]: start at the minimum (`keep_recent`
/// turns kept, which is `turn_parts.len()`) and move back one whole turn
/// while the summary plus the kept tail still fits `budget_chars`. Sizes
/// come from per-turn part sizes, so the walk is linear in turns.
fn extend_split(
    body: &[Value],
    dialect: Dialect,
    turns: &[Range<usize>],
    turn_parts: &[Vec<String>],
    section: Option<&str>,
    budget_chars: usize,
) -> usize {
    let min_split = turn_parts.len();
    // A leading non-assistant group (a prior summary) is its own turn, and
    // at least the first assistant-started turn after it stays summarized.
    let lower = if body.first().is_some_and(|m| dialect.is_assistant(m)) {
        1
    } else {
        2
    };
    // The summary message's size is its shell plus the escaped text:
    // escaping is per character, so the text's size is additive in parts.
    let shell = billable_chars(&dialect.user_message(String::new())) + 2;
    let header = escaped_chars(SUMMARY_HEADER);
    let lead = escaped_chars("\n\n");
    let separator = escaped_chars(PART_SEPARATOR);
    let summary_chars = |part_chars: usize, part_count: usize| {
        shell
            + header
            + if part_count == 0 {
                0
            } else {
                lead + part_chars + (part_count - 1) * separator
            }
    };
    let turn_chars = |turn: &Range<usize>| -> usize {
        body[turn.clone()]
            .iter()
            .map(|message| billable_chars(message) + 2)
            .sum()
    };
    // The carried section is one more part, the same at every split.
    let mut part_chars: usize = turn_parts
        .iter()
        .flatten()
        .map(|part| escaped_chars(part))
        .sum::<usize>()
        + section.map_or(0, escaped_chars);
    let mut part_count: usize =
        turn_parts.iter().map(Vec::len).sum::<usize>() + usize::from(section.is_some());
    let mut tail_chars: usize = turns[min_split..].iter().map(turn_chars).sum();
    if summary_chars(part_chars, part_count) + tail_chars > budget_chars {
        return min_split;
    }
    let mut split = min_split;
    // `turns[split].start >= 2` keeps at least two body messages summarized,
    // so the message count still drops.
    while split > lower && turns[split - 1].start >= 2 {
        let candidate = &turn_parts[split - 1];
        let next_part_chars =
            part_chars - candidate.iter().map(|p| escaped_chars(p)).sum::<usize>();
        let next_part_count = part_count - candidate.len();
        let next_tail_chars = tail_chars + turn_chars(&turns[split - 1]);
        if summary_chars(next_part_chars, next_part_count) + next_tail_chars > budget_chars {
            break;
        }
        split -= 1;
        part_chars = next_part_chars;
        part_count = next_part_count;
        tail_chars = next_tail_chars;
    }
    split
}

/// Deterministic JSON for hashing: sorted keys, compact separators, no
/// ASCII escaping.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn digest_value(value: &Value) -> String {
    sha256_hex(canonical_json(value).as_bytes())
}

/// Known fields are normalized by each dialect. Preserve all remaining
/// fields so provider additions cannot silently alias an older prefix.
fn other_fields(value: &Value, known: &[&str]) -> Value {
    Value::Object(
        value
            .as_object()
            .into_iter()
            .flat_map(|map| map.iter())
            .filter(|(key, _)| !known.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// `chain[i]` identifies `messages[0..=i]` as a sequence, so prefix identity
/// is one lookup: `h_0 = H(d_0)`, `h_i = H(h_{i-1} || d_i)`.
pub fn chain_hashes(digests: &[String]) -> Vec<String> {
    let mut chain = Vec::with_capacity(digests.len());
    let mut previous = String::new();
    for digest in digests {
        previous.push_str(digest);
        previous = sha256_hex(previous.as_bytes());
        chain.push(previous.clone());
    }
    chain
}

/// Serialized length in characters of `value` as a spaced JSON dump
/// (`", "` and `": "` separators, non-ASCII unescaped), with each image
/// payload priced at its estimated tokens times four instead of its base64
/// length. Every size decision goes through here so the trigger and the
/// replay loop agree on what a message costs.
pub fn billable_chars(value: &Value) -> usize {
    let mut chars = json_chars(value);
    for payload in images::payloads(value) {
        // Base64 needs no escaping, so the payload's serialized length is its
        // own length; the two quotes stay in the count.
        chars =
            chars.saturating_sub(payload.len()) + images::tokens_for_payload(payload) as usize * 4;
    }
    chars
}

fn json_chars(value: &Value) -> usize {
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(number) => number.to_string().len(),
        Value::String(text) => string_chars(text),
        Value::Array(items) => {
            2 + items.iter().map(json_chars).sum::<usize>() + 2 * items.len().saturating_sub(1)
        }
        Value::Object(map) => {
            2 + map
                .iter()
                .map(|(key, value)| string_chars(key) + 2 + json_chars(value))
                .sum::<usize>()
                + 2 * map.len().saturating_sub(1)
        }
    }
}

/// Characters `text` adds inside a JSON string: [`string_chars`] without
/// the two quotes.
fn escaped_chars(text: &str) -> usize {
    string_chars(text) - 2
}

fn string_chars(text: &str) -> usize {
    2 + text
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
            c if (c as u32) < 0x20 => 6,
            _ => 1,
        })
        .sum::<usize>()
}

/// Estimated tokens at four characters per token.
pub fn estimate_tokens(value: &Value) -> u64 {
    (billable_chars(value) / 4) as u64
}

/// Truncate to `max_chars` characters with an ellipsis; 0 = unlimited.
fn truncate(text: &str, max_chars: usize) -> String {
    if max_chars > 0 && text.chars().count() > max_chars {
        let mut cut: String = text.chars().take(max_chars).collect();
        cut.push_str("...");
        cut
    } else {
        text.to_string()
    }
}

const TASK_OPEN: &str = "<task-notification>";
const TASK_CLOSE: &str = "</task-notification>";

/// Drop harness-injected subagent reports from summarized user text: the
/// assistant's next message restates the result, and their output-file
/// paths are temporary.
fn strip_task_notifications(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(TASK_OPEN) {
        let Some(close) = rest[open..].find(TASK_CLOSE) else {
            break;
        };
        out.push_str(&rest[..open]);
        rest = rest[open + close + TASK_CLOSE.len()..].trim_start();
    }
    out.push_str(rest);
    out
}

// Carried conversation text (C3): the human's words and the assistant's
// visible replies, kept across compaction cycles. Extraction uses these
// fixed constants and never a step's `CliffConfig`, so every rung of a
// compaction chain extracts the same parts.

/// Cap on one carried part, its `user: ` or `assistant: ` prefix and any
/// ellipsis included.
const CARRY_PART_MAX_CHARS: usize = 4_000;
/// First line of the carried section in a summary.
const CARRY_LABEL: &str = "Earlier in this session (oldest first; older text omitted):";
/// Openings of a message the human (or a coordinating agent) queued while
/// the agent worked, as Claude Code 2.1.283 renders them: the human's and
/// the coordinator's (sent as a role "system" message, or in older and
/// future versions as a whole system-reminder block) and a verified Slack
/// human's in a bound thread, one message or a batch.
const QUEUED_MARKERS: [&str; 4] = [
    "The user sent a new message while you were working:",
    "The coordinator sent a message",
    "A message arrived in the bound thread while you were working:",
    "Messages arrived in the bound thread while you were working:",
];
/// Harness text Claude Code 2.1.283 appends to a queued message: after the
/// human's (and an auto-continuation's) and after the coordinator's.
const QUEUED_TRAILERS: [&str; 2] = [
    "\n\nThis is how Claude Code surfaces messages the user sends mid-turn \u{2014} within \
     the running turn, often alongside the next tool result, rather than as a separate \
     conversation turn. Address the message above as you continue this turn.",
    "\n\nAddress this before completing your current task.",
];
/// The token-count note Claude Code sends after tool results; it can share
/// the queued message's system message.
const TOKENS_OPEN: &str = "<total_tokens>";
const TOKENS_CLOSE: &str = " tokens left</total_tokens>";
/// Openings of harness text that can arrive as a whole block of a human
/// turn: shell and local-command output, the caveat Claude Code sends
/// before a local command's messages, a skill a slash command loaded, and
/// the context items Codex re-sends as user messages.
const HARNESS_BLOCKS: [&str; 9] = [
    "<bash-stdout>",
    "<bash-stderr>",
    "<local-command-stdout>",
    "<local-command-stderr>",
    "<local-command-caveat>",
    "Base directory for this skill:",
    "<environment_context>",
    "<user_instructions>",
    "# AGENTS.md instructions",
];
/// Opening of the harness text block that follows an interrupted tool call.
const INTERRUPT_MARKER: &str = "[Request interrupted by user";
const REMINDER_OPEN: &str = "<system-reminder>";
const REMINDER_CLOSE: &str = "</system-reminder>";

/// The queued message in `text` with its marker removed: everything
/// through the first colon on the marker's line, or that whole line when it
/// has no colon. `None` when `text` does not start with a marker.
fn strip_queued_marker(text: &str) -> Option<&str> {
    let text = text.trim();
    if !QUEUED_MARKERS.iter().any(|marker| text.starts_with(marker)) {
        return None;
    }
    let line_end = text.find('\n').unwrap_or(text.len());
    let rest = match text[..line_end].find(':') {
        Some(colon) => &text[colon + 1..],
        None => &text[line_end..],
    };
    Some(rest.trim())
}

/// `text` without a closing token-count note, when it ends with one.
fn strip_tokens_note(text: &str) -> &str {
    let text = text.trim_end();
    let Some(open) = text.rfind(TOKENS_OPEN) else {
        return text;
    };
    let count = text
        .strip_suffix(TOKENS_CLOSE)
        .and_then(|head| head.get(open + TOKENS_OPEN.len()..));
    match count {
        Some(count) if !count.is_empty() && count.chars().all(|c| c.is_ascii_alphanumeric()) => {
            text[..open].trim_end()
        }
        _ => text,
    }
}

/// The words of a queued message in `text`, which starts with a marker:
/// the marker removed, then a closing token-count note and one of Claude
/// Code's trailers, each only on an exact match (anything else stays), and
/// reminder spans and task notifications stripped as in `strip_spans`.
fn queued_message(text: &str) -> Option<String> {
    let rest = strip_tokens_note(strip_queued_marker(text)?);
    let rest = QUEUED_TRAILERS
        .iter()
        .find_map(|trailer| {
            rest.strip_suffix(trailer)
                .or_else(|| (rest == trailer.trim_start()).then_some(""))
        })
        .unwrap_or(rest);
    Some(strip_spans(rest).trim().to_string())
}

/// The queued message a whole human text block holds: the block, trimmed,
/// is one closed system-reminder span, or plain text, that opens with a
/// marker. A span elsewhere in a block is stripped, never carried.
fn queued_block(text: &str) -> Option<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix(REMINDER_OPEN)
        .and_then(|inner| inner.strip_suffix(REMINDER_CLOSE))
        .filter(|inner| !inner.contains(REMINDER_OPEN) && !inner.contains(REMINDER_CLOSE))
        .unwrap_or(text);
    queued_message(inner)
}

/// `text` without the closed `open`..`close` spans in it. Text that opens
/// with an unclosed span is harness text and yields nothing; an unclosed
/// opening tag after other text stays, as typed.
fn strip_closed(text: &str, open: &str, close: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let inner = start + open.len();
        let Some(end) = rest[inner..].find(close) else {
            if kept.trim().is_empty() && rest[..start].trim().is_empty() {
                return String::new();
            }
            break;
        };
        kept.push_str(&rest[..start]);
        rest = rest[inner + end + close.len()..].trim_start();
    }
    kept.push_str(rest);
    kept
}

/// Carried text without system-reminder spans and task notifications, as
/// `strip_closed` removes them.
fn strip_spans(text: &str) -> String {
    let kept = strip_closed(text, REMINDER_OPEN, REMINDER_CLOSE);
    strip_closed(&kept, TASK_OPEN, TASK_CLOSE)
}

/// One human text block as carried: its typed text with spans stripped,
/// or the queued message when the whole block is one. Harness blocks and
/// summary text yield neither.
fn human_block(text: &str) -> (String, Option<String>) {
    if let Some(queued) = queued_block(text) {
        return (String::new(), Some(queued));
    }
    let opening = text.trim_start();
    if opening.starts_with(SUMMARY_HEADER)
        || HARNESS_BLOCKS
            .iter()
            .any(|prefix| opening.starts_with(prefix))
    {
        return (String::new(), None);
    }
    (strip_spans(text).trim().to_string(), None)
}

/// True for the harness's marker block after an interrupted tool call.
fn is_interrupt_marker(text: &str) -> bool {
    text.trim_start().starts_with(INTERRUPT_MARKER)
}

/// One carried part: trimmed, every line that is `---` after trimming
/// rewritten as `- - -` (so no part, and no join of parts, can hold a
/// PART_SEPARATOR), prefixed, and cut to `CARRY_PART_MAX_CHARS` with the
/// prefix and ellipsis counted. `None` for empty text and for text that
/// starts with the summary header.
fn carry_part(prefix: &str, text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.starts_with(SUMMARY_HEADER) {
        return None;
    }
    let text = text
        .split('\n')
        .map(|line| if line.trim() == "---" { "- - -" } else { line })
        .collect::<Vec<_>>()
        .join("\n");
    let part = format!("{prefix}{text}");
    Some(if part.chars().count() > CARRY_PART_MAX_CHARS {
        truncate(&part, CARRY_PART_MAX_CHARS - "...".len())
    } else {
        part
    })
}

/// Visible assistant text as one carried part: each text with reminder
/// spans and task notifications stripped (a queued span there is dropped,
/// not carried), trimmed, summary-header text skipped, joined by newlines.
fn carry_assistant(texts: &[&str]) -> Option<String> {
    let visible: Vec<String> = texts
        .iter()
        .filter(|text| !text.trim_start().starts_with(SUMMARY_HEADER))
        .map(|text| strip_spans(text).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect();
    carry_part("assistant: ", &visible.join("\n"))
}

/// Human text blocks as carried parts: their typed text, as `human_block`
/// keeps it, joined into one `user: ` part, then one part per block that
/// is a queued message.
fn carry_human<S: AsRef<str>>(texts: &[S]) -> Vec<String> {
    let mut typed = Vec::new();
    let mut queued = Vec::new();
    for text in texts {
        let (kept, found) = human_block(text.as_ref());
        queued.extend(found);
        if !kept.is_empty() {
            typed.push(kept);
        }
    }
    let mut parts: Vec<String> = carry_part("user: ", &typed.join("\n"))
        .into_iter()
        .collect();
    parts.extend(queued.iter().filter_map(|text| carry_part("user: ", text)));
    parts
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::replay::pairing_intact;
    use super::*;
    use serde_json::json;

    fn cfg(keep_recent: usize) -> CliffConfig {
        CliffConfig {
            keep_recent,
            ..CliffConfig::default()
        }
    }

    #[test]
    fn the_default_keeps_exactly_the_recent_turns() {
        // v0.7.3: the default tail budget is 0, the reference tail. One
        // Terminal-Bench 2.1 trial found tail 0 cheaper than tail 40 at the
        // same resolution; `--keep-tail-percent` still selects a larger tail.
        let cfg = CliffConfig::default();
        assert_eq!(cfg.keep_tail_percent, 0);
        assert_eq!(cfg.keep_recent, 3);
    }

    #[test]
    fn detects_dialects_by_path_and_skips_probes() {
        assert_eq!(Dialect::detect("/v1/messages"), Some(Dialect::Anthropic));
        assert_eq!(
            Dialect::detect("/v1/messages?beta=true"),
            Some(Dialect::Anthropic)
        );
        assert_eq!(Dialect::detect("/v1/messages/count_tokens"), None);
        assert_eq!(
            Dialect::detect("/backend-api/codex/responses"),
            Some(Dialect::Responses)
        );
        assert_eq!(Dialect::detect("/v1/responses/"), Some(Dialect::Responses));
        assert_eq!(Dialect::detect("/v1/responses/compact"), None);
        assert_eq!(
            Dialect::detect("/chat/completions"),
            Some(Dialect::ChatCompletions)
        );
        assert_eq!(
            Dialect::detect("/v1/chat/completions?stream=true"),
            Some(Dialect::ChatCompletions)
        );
        assert_eq!(
            Dialect::detect("/openai/deployments/x/chat/completions/"),
            Some(Dialect::ChatCompletions)
        );
        assert_eq!(Dialect::detect("/v1/models"), None);
    }

    #[test]
    fn keeps_head_summary_and_recent_turns() {
        let messages = a_session(10, 3000);
        let result = compact(&messages, Dialect::Anthropic, &cfg(3)).unwrap();
        assert_eq!(result.head_len, 1);
        assert_eq!(result.messages[0], messages[0]);
        assert_eq!(summaries(&result.messages).len(), 1);
        // Three kept turns of (assistant, result) after head and summary.
        assert_eq!(result.messages.len(), 1 + 1 + 6);
        assert_eq!(result.messages[2..], messages[messages.len() - 6..]);
        assert_eq!(result.cut, messages.len() - 6);
    }

    #[test]
    fn long_results_drop_short_results_and_signatures_stay() {
        let messages = a_session(10, 3000);
        let result = compact(&messages, Dialect::Anthropic, &cfg(1)).unwrap();
        let summary = &summaries(&result.messages)[0];
        assert!(!summary.contains("XXXX"));
        assert!(summary.contains("result: test_1 passed (short output)"));
        assert!(summary.contains("[bash] {\"command\":\"pytest tests/test_0.py -x\"}"));
        assert!(summary.contains("assistant: Step 0: I will inspect module 0"));
        assert!(
            !summary.contains("Fix the failing test"),
            "the head is never summarized"
        );
    }

    #[test]
    fn nothing_to_compact_without_enough_turns_or_any_assistant() {
        assert!(compact(&a_session(3, 3000), Dialect::Anthropic, &cfg(3)).is_none());
        assert!(compact(&[a_user("hello")], Dialect::Anthropic, &cfg(0)).is_none());
    }

    #[test]
    fn recompaction_stays_flat_and_drops_the_prior_summary() {
        let messages = a_session(10, 3000);
        let first = compact(&messages, Dialect::Anthropic, &cfg(1)).unwrap();
        let mut grown = first.messages.clone();
        for i in 10..16 {
            let id = format!("tu_{i}");
            grown.push(a_assistant(
                &format!("Step {i}"),
                Some((&id, "bash", json!({"command": format!("cmd {i}")}))),
            ));
            grown.push(a_result(&id, &"K".repeat(2000)));
        }
        let second = compact(&grown, Dialect::Anthropic, &cfg(1)).unwrap();
        let found = summaries(&second.messages);
        assert_eq!(found.len(), 1);
        assert!(!found[0].contains("Step 0"));
        assert!(found[0].contains("Step 14"));
        assert_eq!(second.messages[0], messages[0]);
    }

    #[test]
    fn system_messages_never_precede_the_summary() {
        let mut messages = vec![
            json!({"role": "user", "content": "the task"}),
            json!({"role": "system", "content": "directive: be careful"}),
        ];
        for i in 0..6 {
            messages.push(a_assistant(
                &format!("step {i}"),
                Some((
                    &format!("t{i}"),
                    "bash",
                    json!({"command": format!("make {i}")}),
                )),
            ));
            messages.push(a_result(&format!("t{i}"), &"R".repeat(2000)));
        }
        let result = compact(&messages, Dialect::Anthropic, &cfg(2)).unwrap();
        assert_eq!(result.head_len, 1);
        let summary = result.messages[1]["content"].as_str().unwrap();
        assert!(summary.contains("system: directive: be careful"));
    }

    #[test]
    fn task_notifications_are_stripped() {
        let text = "before <task-notification>id 7\npath /tmp/x</task-notification>\n after";
        assert_eq!(strip_task_notifications(text), "before after");
        assert_eq!(
            strip_task_notifications("<task-notification>open"),
            "<task-notification>open"
        );
    }

    #[test]
    fn canonical_json_sorts_keys_and_chain_is_prefix_stable() {
        let a = json!({"b": 1, "a": [true, null, "é"]});
        assert_eq!(canonical_json(&a), "{\"a\":[true,null,\"é\"],\"b\":1}");
        let digests: Vec<String> = ["x", "y", "z"]
            .iter()
            .map(|s| sha256_hex(s.as_bytes()))
            .collect();
        let full = chain_hashes(&digests);
        let prefix = chain_hashes(&digests[..2]);
        assert_eq!(full[..2], prefix[..]);
        assert_ne!(full[2], full[1]);
    }

    #[test]
    fn character_accounting_matches_a_spaced_json_dump() {
        // json.dumps({"a": [1, "x\n"], "b": null}, ensure_ascii=False)
        // == '{"a": [1, "x\\n"], "b": null}' -> 28 characters.
        assert_eq!(json_chars(&json!({"a": [1, "x\n"], "b": null})), 28);
        assert_eq!(json_chars(&json!("é")), 3);
        assert_eq!(json_chars(&json!([])), 2);
        assert_eq!(estimate_tokens(&json!({"k": "x".repeat(4000)})), 1002);
    }

    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate("héllo", 2), "hé...");
        assert_eq!(truncate("héllo", 0), "héllo");
        assert_eq!(truncate("héllo", 5), "héllo");
    }

    const DIALECTS: [Dialect; 3] = [
        Dialect::Anthropic,
        Dialect::Responses,
        Dialect::ChatCompletions,
    ];

    /// Engine size of a step's summary and kept tail: billable characters
    /// plus 2 per message.
    fn step_chars(result: &CompactResult) -> usize {
        result.messages[result.head_len..]
            .iter()
            .map(|m| billable_chars(m) + 2)
            .sum()
    }

    /// Model steps `steps`, each making `calls` tool calls; alternate
    /// results are `result_chars` long.
    fn dialect_steps(
        dialect: Dialect,
        steps: Range<usize>,
        calls: usize,
        result_chars: usize,
    ) -> Vec<Value> {
        let mut messages = Vec::new();
        for i in steps {
            let ids: Vec<String> = (0..calls).map(|c| format!("call_{i}_{c}")).collect();
            let result = |c: usize| {
                if (i + c).is_multiple_of(2) {
                    "R".repeat(result_chars)
                } else {
                    format!("step {i} call {c} ok")
                }
            };
            match dialect {
                Dialect::Anthropic => {
                    let mut content = vec![json!({"type": "text", "text": format!("Step {i}.")})];
                    content.extend(ids.iter().map(|id| {
                        json!({"type": "tool_use", "id": id, "name": "bash",
                               "input": {"command": format!("run {id}")}})
                    }));
                    messages.push(json!({"role": "assistant", "content": content}));
                    let results: Vec<Value> = ids
                        .iter()
                        .enumerate()
                        .map(|(c, id)| {
                            json!({"type": "tool_result", "tool_use_id": id, "content": result(c)})
                        })
                        .collect();
                    messages.push(json!({"role": "user", "content": results}));
                }
                Dialect::ChatCompletions => {
                    let tool_calls: Vec<Value> = ids
                        .iter()
                        .map(|id| {
                            json!({"id": id, "type": "function", "function": {"name": "bash",
                                   "arguments": format!("{{\"command\":\"run {id}\"}}")}})
                        })
                        .collect();
                    messages.push(json!({"role": "assistant", "content": format!("Step {i}."),
                                         "tool_calls": tool_calls}));
                    for (c, id) in ids.iter().enumerate() {
                        messages.push(
                            json!({"role": "tool", "tool_call_id": id, "content": result(c)}),
                        );
                    }
                }
                Dialect::Responses => {
                    messages.push(json!({"type": "reasoning", "id": format!("rs_{i}"),
                        "encrypted_content": format!("enc{i}"),
                        "summary": [{"type": "summary_text", "text": format!("plan {i}")}]}));
                    for id in &ids {
                        messages.push(json!({"type": "function_call", "call_id": id,
                            "name": "shell", "arguments": format!("{{\"command\":\"run {id}\"}}")}));
                    }
                    for (c, id) in ids.iter().enumerate() {
                        messages.push(
                            json!({"type": "function_call_output", "call_id": id, "output": result(c)}),
                        );
                    }
                }
            }
        }
        messages
    }

    /// Fresh sessions and sessions that grew after a first compaction.
    fn tail_fixtures(dialect: Dialect) -> Vec<Vec<Value>> {
        let task = "Fix the failing test in repo X.".to_string();
        let mut out = Vec::new();
        for (steps, calls) in [(1, 1), (4, 1), (9, 2), (14, 1)] {
            let mut fresh = vec![dialect.user_message(task.clone())];
            fresh.extend(dialect_steps(dialect, 0..steps, calls, 1500));
            if let Some(first) = compact(&fresh, dialect, &cfg(1)) {
                let mut grown = first.messages;
                grown.extend(dialect_steps(dialect, steps..steps + 5, calls, 900));
                out.push(grown);
            }
            out.push(fresh);
        }
        out
    }

    #[test]
    fn a_budget_below_the_minimum_tail_is_the_reference_step() {
        for dialect in DIALECTS {
            for messages in tail_fixtures(dialect) {
                for keep_recent in 0..5 {
                    let reference = compact(&messages, dialect, &cfg(keep_recent));
                    let minimum = reference.as_ref().map_or(1, step_chars);
                    for budget in [0, 1, minimum - 1] {
                        let budgeted =
                            compact_within(&messages, dialect, &cfg(keep_recent), budget, &[], 0);
                        assert_eq!(
                            budgeted.map(|r| (r.messages, r.head_len, r.cut)),
                            reference.clone().map(|r| (r.messages, r.head_len, r.cut)),
                            "{dialect:?} keep_recent {keep_recent} budget {budget}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_budget_never_turns_a_step_into_none_and_keeps_whole_paired_turns() {
        for dialect in DIALECTS {
            let mut extended = 0;
            for messages in tail_fixtures(dialect) {
                for keep_recent in 0..5 {
                    let reference = compact(&messages, dialect, &cfg(keep_recent));
                    for budget in [100, 2_000, 8_000, 20_000, 60_000, usize::MAX / 2] {
                        let budgeted =
                            compact_within(&messages, dialect, &cfg(keep_recent), budget, &[], 0);
                        let (Some(reference), Some(budgeted)) = (&reference, &budgeted) else {
                            assert_eq!(reference.is_some(), budgeted.is_some());
                            continue;
                        };
                        let cut = budgeted.cut;
                        assert!(cut <= reference.cut);
                        assert_eq!(budgeted.head_len, reference.head_len);
                        assert!(budgeted.messages.len() < messages.len());
                        if cut < messages.len() {
                            assert!(dialect.is_assistant(&messages[cut]));
                            assert!(!dialect.is_assistant(&messages[cut - 1]));
                        }
                        assert!(pairing_intact(&budgeted.messages, dialect));
                        assert!(
                            messages[budgeted.head_len..cut]
                                .iter()
                                .any(|m| dialect.is_assistant(m)),
                            "an assistant-started turn stays summarized"
                        );
                        if cut < reference.cut {
                            assert!(step_chars(budgeted) <= budget);
                            extended += 1;
                        }
                    }
                }
            }
            assert!(extended > 0, "{dialect:?}");
        }
    }

    #[test]
    fn the_extension_fills_the_budget_exactly() {
        for dialect in DIALECTS {
            let mut extended = 0;
            for messages in tail_fixtures(dialect) {
                let Some(reference) = compact(&messages, dialect, &cfg(1)) else {
                    continue;
                };
                let mut budget = step_chars(&reference);
                while budget < 400_000 {
                    let reached =
                        compact_within(&messages, dialect, &cfg(1), budget, &[], 0).unwrap();
                    let size = step_chars(&reached);
                    let exact = compact_within(&messages, dialect, &cfg(1), size, &[], 0).unwrap();
                    assert_eq!(exact.cut, reached.cut, "{dialect:?} budget {size}");
                    if reached.cut < reference.cut {
                        assert!(size <= budget);
                        let short =
                            compact_within(&messages, dialect, &cfg(1), size - 1, &[], 0).unwrap();
                        assert!(short.cut > reached.cut, "{dialect:?} budget {}", size - 1);
                        extended += 1;
                    }
                    budget = budget * 5 / 4;
                }
            }
            assert!(extended > 0, "{dialect:?}");
        }
    }

    #[test]
    fn the_tail_extension_stops_after_the_first_summarized_turn() {
        let session = a_session(4, 100);
        let prior = a_user(&format!(
            "{SUMMARY_HEADER}\n\nprior-notes {}",
            "S".repeat(20_000)
        ));
        let mut messages = vec![session[0].clone(), prior.clone()];
        messages.extend_from_slice(&session[1..]);
        for keep_recent in [1, 2, 3] {
            let result = compact_within(
                &messages,
                Dialect::Anthropic,
                &cfg(keep_recent),
                usize::MAX / 2,
                &[],
                0,
            )
            .unwrap();
            // The prior summary and the first turn are summarized.
            assert_eq!(result.cut, 4);
            assert_eq!(result.messages.len(), 1 + 1 + 6);
            let summary = &summaries(&result.messages)[0];
            assert!(summary.contains("Step 0"));
            assert!(!summary.contains("prior-notes"));
        }
        // Without a prior summary the first turn is still summarized.
        let result = compact_within(
            &session,
            Dialect::Anthropic,
            &cfg(1),
            usize::MAX / 2,
            &[],
            0,
        )
        .unwrap();
        assert_eq!(result.cut, 3);
        // A two-message leading group (a trimmed system message and the
        // prior summary) could be dropped alone and still shrink the list;
        // the bound keeps the first assistant turn summarized anyway.
        let mut led = vec![
            session[0].clone(),
            json!({"role": "system", "content": "directive: be careful"}),
            prior,
        ];
        led.extend_from_slice(&session[1..]);
        let result =
            compact_within(&led, Dialect::Anthropic, &cfg(1), usize::MAX / 2, &[], 0).unwrap();
        assert_eq!(result.head_len, 1);
        assert_eq!(result.cut, 5);
        assert!(summaries(&result.messages)[0].contains("Step 0"));
    }

    /// Anthropic steps as Claude Code records them. Every third step (from
    /// step 1) is split into two consecutive assistant messages, the tool
    /// calls and then the text, followed by one result message per call.
    fn split_steps(steps: Range<usize>, result_chars: usize) -> Vec<Value> {
        let mut messages = Vec::new();
        for i in steps {
            let long = "R".repeat(result_chars);
            if i % 3 == 1 {
                let (a, b) = (format!("call_{i}_a"), format!("call_{i}_b"));
                messages.push(json!({"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "", "signature": format!("sig{i}")},
                    {"type": "tool_use", "id": a, "name": "read", "input": {"file_path": format!("f{i}")}},
                    {"type": "tool_use", "id": b, "name": "bash", "input": {"command": format!("run {i}")}},
                ]}));
                messages.push(a_assistant(
                    &format!("Reading two things for step {i}."),
                    None,
                ));
                messages.push(a_result(&a, &long));
                messages.push(a_result(&b, &format!("step {i} ok")));
            } else {
                let id = format!("call_{i}");
                messages.push(a_assistant(
                    &format!("Step {i}."),
                    Some((&id, "bash", json!({"command": format!("run {i}")}))),
                ));
                let text = if i % 2 == 0 {
                    long
                } else {
                    format!("step {i} ok")
                };
                messages.push(a_result(&id, &text));
            }
        }
        messages
    }

    /// Kept assistant-started turns: runs of assistant messages.
    fn kept_turns(messages: &[Value]) -> usize {
        (0..messages.len())
            .filter(|&i| {
                Dialect::Anthropic.is_assistant(&messages[i])
                    && (i == 0 || !Dialect::Anthropic.is_assistant(&messages[i - 1]))
            })
            .count()
    }

    #[test]
    fn a_split_assistant_step_is_one_turn() {
        let dialect = Dialect::Anthropic;
        let mut messages = vec![a_user("Fix the failing test in repo X.")];
        messages.extend(split_steps(0..12, 1500));
        assert_eq!(dialect.group_turns(&messages[1..]).len(), 12);
        let mut split_boundaries = 0;
        for keep_recent in 1..6 {
            for budget in [0, 2_000, 8_000, 20_000, usize::MAX / 2] {
                let result =
                    compact_within(&messages, dialect, &cfg(keep_recent), budget, &[], 0).unwrap();
                assert!(
                    pairing_intact(&result.messages, dialect),
                    "keep_recent {keep_recent} budget {budget}"
                );
                assert!(dialect.is_assistant(&messages[result.cut]));
                assert!(!dialect.is_assistant(&messages[result.cut - 1]));
                if budget == 0 {
                    assert_eq!(kept_turns(&messages[result.cut..]), keep_recent);
                    split_boundaries +=
                        usize::from(dialect.is_assistant(&messages[result.cut + 1]));
                }
            }
        }
        // The fixture puts a split step at the boundary for some keep_recent.
        assert!(split_boundaries > 0);
    }

    #[test]
    fn single_assistant_messages_group_as_one_turn_each() {
        let per_message =
            |body: &[Value]| group_by_starts(body, |i| Dialect::Anthropic.is_assistant(&body[i]));
        let mut fixtures = tail_fixtures(Dialect::Anthropic);
        fixtures.push(a_session(9, 3_000));
        for messages in fixtures {
            let body = &messages[1..];
            assert!(body
                .windows(2)
                .all(|pair| !(Dialect::Anthropic.is_assistant(&pair[0])
                    && Dialect::Anthropic.is_assistant(&pair[1]))));
            assert_eq!(Dialect::Anthropic.group_turns(body), per_message(body));
        }
    }

    #[test]
    fn a_replay_with_split_steps_keeps_every_pair_at_any_tail_budget() {
        let mut history = vec![a_user("Fix the failing test in repo X.")];
        history.extend(split_steps(0..90, 2_500));
        let mut compactions = 0;
        for keep_recent in [1, 2, 3] {
            for threshold_tokens in [4_000, 6_000, 9_000] {
                for keep_tail_percent in [0, 25, 40, 60] {
                    let cfg = CliffConfig {
                        threshold_tokens,
                        keep_recent,
                        keep_tail_percent,
                        ..CliffConfig::default()
                    };
                    let report = super::replay::replay(&history, Dialect::Anthropic, cfg, 1_000);
                    assert_eq!(report.source_pairing_violations, 0);
                    compactions += report.compacted;
                    assert_eq!(
                        report.pairing_violations, 0,
                        "keep_recent {keep_recent} threshold {threshold_tokens} p{keep_tail_percent}"
                    );
                }
            }
        }
        assert!(compactions > 100);
    }

    #[test]
    fn span_stripping_removes_closed_spans_and_blocks_that_open_unclosed() {
        assert_eq!(
            strip_spans(
                "before <system-reminder>harness</system-reminder><system-reminder>more</system-reminder>after"
            ),
            "before after"
        );
        // A block that opens with an unclosed span is harness text.
        assert_eq!(strip_spans("<system-reminder>harness never closed"), "");
        assert_eq!(strip_spans("  \n<system-reminder>harness"), "");
        assert_eq!(
            strip_spans("<system-reminder>a</system-reminder><system-reminder>b"),
            ""
        );
        // A tag the human typed after their words stays, and so does the
        // rest of the block.
        let typed = "Grep for\n<system-reminder> in the fixtures, then fix them";
        assert_eq!(strip_spans(typed), typed);
        // Task notifications follow the same rule.
        assert_eq!(
            strip_spans("<task-notification>done</task-notification>\nplease"),
            "please"
        );
        assert_eq!(
            strip_spans("<task-notification>\n<task-id>b1</task-id> TASK-BODY"),
            ""
        );
        let typed = "Why does <task-notification> appear here?";
        assert_eq!(strip_spans(typed), typed);
    }

    #[test]
    fn only_a_whole_block_is_read_as_a_queued_message() {
        let user = QUEUED_MARKERS[0];
        assert_eq!(
            queued_block(&format!(
                "<system-reminder>\n{user}\nfix the tests\n</system-reminder>"
            )),
            Some("fix the tests".to_string())
        );
        assert_eq!(
            queued_block(&format!("{user}\nplain block")),
            Some("plain block".to_string())
        );
        // A span inside other text is stripped, never carried.
        let inside = format!("a<system-reminder>{user}\nfix the tests</system-reminder>b");
        assert_eq!(queued_block(&inside), None);
        assert_eq!(human_block(&inside), ("ab".to_string(), None));
        // Two spans, or one left open, are not one queued message.
        let two = format!(
            "<system-reminder>{user}\nx</system-reminder><system-reminder>y</system-reminder>"
        );
        assert_eq!(queued_block(&two), None);
        assert_eq!(human_block(&two), (String::new(), None));
        let open = format!("<system-reminder>{user}\nx");
        assert_eq!(queued_block(&open), None);
        assert_eq!(human_block(&open), (String::new(), None));
    }

    #[test]
    fn queued_messages_lose_claude_codes_trailers_only_on_an_exact_match() {
        let human = format!(
            "The user sent a new message while you were working:\nTYPED words{}",
            QUEUED_TRAILERS[0]
        );
        assert_eq!(queued_message(&human).as_deref(), Some("TYPED words"));
        // The token-count note that shares the system message goes first.
        let noted = format!("{human}\n\n<total_tokens>14999985 tokens left</total_tokens>");
        assert_eq!(noted.chars().count(), 52 + 11 + 230 + 2 + 49);
        assert_eq!(queued_message(&noted).as_deref(), Some("TYPED words"));
        let coordinator = format!(
            "The coordinator sent a message while you were working:\nrebase first{}",
            QUEUED_TRAILERS[1]
        );
        assert_eq!(
            queued_message(&coordinator).as_deref(),
            Some("rebase first")
        );
        assert_eq!(
            queued_message(&format!("{}{}", QUEUED_MARKERS[0], QUEUED_TRAILERS[0])).as_deref(),
            Some("")
        );
        // Anything but the exact trailer stays.
        let near = human.replace("Address the message above", "Address the message");
        assert!(queued_message(&near)
            .unwrap()
            .ends_with("as you continue this turn."));
        assert_eq!(
            strip_tokens_note("x\n\n<total_tokens>Infinite tokens left</total_tokens>"),
            "x"
        );
        for kept in [
            "x <total_tokens>5 tokens</total_tokens>",
            "x <total_tokens> tokens left</total_tokens>",
            "x <total_tokens>5 6 tokens left</total_tokens>",
        ] {
            assert_eq!(strip_tokens_note(kept), kept);
        }
        // Bound-thread messages have no trailer.
        for marker in &QUEUED_MARKERS[2..] {
            assert_eq!(
                queued_message(&format!("{marker}\nfrom Slack")).as_deref(),
                Some("from Slack")
            );
        }
    }

    #[test]
    fn harness_blocks_in_a_human_turn_are_not_carried() {
        for block in [
            "<bash-stdout>BUILD-OUTPUT</bash-stdout><bash-stderr></bash-stderr>",
            "<bash-stderr>ERR</bash-stderr>",
            "<local-command-stdout>COMMAND-OUTPUT</local-command-stdout>",
            "<local-command-stderr>COMMAND-ERR</local-command-stderr>",
            "<local-command-caveat>Caveat: CAVEAT-TEXT DO NOT respond to these messages.</local-command-caveat>",
            "Base directory for this skill: /skills/x\n\n# Skill\n\nSKILL-BODY",
            "<environment_context>\n  <cwd>/w</cwd>\n</environment_context>",
            "<user_instructions>\nUSER-INSTRUCTIONS\n</user_instructions>",
            "# AGENTS.md instructions for /w\n\n<INSTRUCTIONS>AGENTS</INSTRUCTIONS>",
        ] {
            assert_eq!(human_block(block), (String::new(), None), "{block}");
        }
        // The human's own command is their words.
        assert_eq!(
            human_block("<bash-input>cargo test</bash-input>").0,
            "<bash-input>cargo test</bash-input>"
        );
    }

    #[test]
    fn queued_marker_removal_runs_through_the_first_colon_of_its_line() {
        let user = QUEUED_MARKERS[0];
        let coordinator = QUEUED_MARKERS[1];
        assert_eq!(
            strip_queued_marker(&format!("{user} same line")),
            Some("same line")
        );
        assert_eq!(
            strip_queued_marker(&format!("  {user}\nnext line\n")),
            Some("next line")
        );
        assert_eq!(
            strip_queued_marker(&format!("{coordinator} from lead: a: b\nc")),
            Some("a: b\nc")
        );
        // No colon on the marker's line: the whole line goes.
        assert_eq!(
            strip_queued_marker(&format!("{coordinator}\nx: y")),
            Some("x: y")
        );
        assert_eq!(strip_queued_marker(coordinator), Some(""));
        assert_eq!(strip_queued_marker("not queued: text"), None);
    }

    #[test]
    fn carry_parts_never_hold_or_form_a_part_separator() {
        let doubled = carry_part("user: ", "a\n\n---\n\n---\n\nb").unwrap();
        assert!(!doubled.contains(PART_SEPARATOR), "{doubled:?}");
        assert_eq!(doubled, "user: a\n\n- - -\n\n- - -\n\nb");
        let tail = carry_part("assistant: ", "done\n\n---").unwrap();
        let head = carry_part("user: ", " ---\r\n\nnext").unwrap();
        let joined = [CARRY_LABEL, tail.as_str(), head.as_str()].join("\n\n");
        assert!(!joined.contains(PART_SEPARATOR), "{joined:?}");
        assert!(joined.split(PART_SEPARATOR).count() == 1);
        // Only whole `---` lines change.
        assert_eq!(
            carry_part("user: ", "a --- b\n----").unwrap(),
            "user: a --- b\n----"
        );
    }

    #[test]
    fn carry_part_trims_prefixes_caps_and_drops_empty_text() {
        assert_eq!(carry_part("user: ", "  hi \n").unwrap(), "user: hi");
        assert_eq!(carry_part("user: ", " \n\t"), None);
        assert_eq!(carry_part("user: ", ""), None);
        assert_eq!(
            carry_part("user: ", &format!("{SUMMARY_HEADER}\n\nuser: old")),
            None
        );
        // The cap counts the prefix and the ellipsis.
        let long = carry_part("assistant: ", &"x".repeat(CARRY_PART_MAX_CHARS)).unwrap();
        assert_eq!(long.chars().count(), CARRY_PART_MAX_CHARS);
        assert!(long.starts_with("assistant: xxx") && long.ends_with("x..."));
        let exact = "y".repeat(CARRY_PART_MAX_CHARS - "user: ".len());
        assert_eq!(
            carry_part("user: ", &exact).unwrap(),
            format!("user: {exact}")
        );
        let over = carry_part("user: ", &format!("{exact}z")).unwrap();
        assert_eq!(over.chars().count(), CARRY_PART_MAX_CHARS);
        assert!(over.ends_with("y..."));
        // Human text loses task notifications and reminder spans.
        assert_eq!(
            human_block(
                "<task-notification>done</task-notification>\nplease <system-reminder>x</system-reminder>continue",
            ),
            ("please continue".to_string(), None)
        );
        assert!(is_interrupt_marker(
            "[Request interrupted by user for tool use]\n"
        ));
        assert!(!is_interrupt_marker("I was [Request interrupted by user"));
    }

    /// A distinct part of `cost` (characters plus 2).
    fn part_of_cost(label: u32, cost: usize) -> String {
        let letter = char::from_u32(0x4E00 + label % 20_000).unwrap();
        std::iter::repeat_n(letter, cost - 2).collect()
    }

    fn parts_of_cost(first_label: u32, costs: &[usize]) -> Vec<String> {
        costs
            .iter()
            .zip(first_label..)
            .map(|(&cost, label)| part_of_cost(label, cost))
            .collect()
    }

    fn carry_cost(parts: &[String]) -> usize {
        parts.iter().map(|part| part.chars().count() + 2).sum()
    }

    #[test]
    fn the_carry_bound_keeps_the_longest_fitting_suffix() {
        let a = parts_of_cost(0, &[2, 9]);
        let b = parts_of_cost(10, &[5]);
        let ab = [a.clone(), b.clone()].concat();
        assert_eq!(bound_carry(&a, 10), &a[1..]);
        assert_eq!(bound_carry(&ab, 10), &b[..]);
        // A greedy bound that skipped the 9 and took the older 2 would keep
        // [2, 5] from a ++ b but only [5] from bound(a) ++ b.
        let rebound = [bound_carry(&a, 10).to_vec(), b.clone()].concat();
        assert_eq!(bound_carry(&rebound, 10), bound_carry(&ab, 10));
        assert!(bound_carry(&ab, 0).is_empty());
        assert!(bound_carry(&[], 10).is_empty());
        assert_eq!(bound_carry(&ab, 16), &ab[..]);
        assert_eq!(bound_carry(&ab, 15), &ab[1..]);
    }

    #[test]
    fn the_carry_bound_has_the_suffix_property_on_random_vectors() {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |n: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize % n
        };
        let mut label = 0u32;
        for _ in 0..4_000 {
            let budget = next(48);
            let mut draw = |count: usize, next: &mut dyn FnMut(usize) -> usize| -> Vec<String> {
                (0..count)
                    .map(|_| {
                        label += 1;
                        part_of_cost(label, 2 + next(16))
                    })
                    .collect()
            };
            let (a_len, b_len) = (next(7), next(5));
            let a = draw(a_len, &mut next);
            let b = draw(b_len, &mut next);
            let bound_a = bound_carry(&a, budget);
            assert_eq!(bound_carry(bound_a, budget), bound_a);
            let ab = [a.clone(), b.clone()].concat();
            let rebound = [bound_a.to_vec(), b].concat();
            let kept = bound_carry(&ab, budget);
            assert_eq!(bound_carry(&rebound, budget), kept);
            // A suffix that fits, and the next older part does not.
            assert_eq!(kept, &ab[ab.len() - kept.len()..]);
            assert!(carry_cost(kept) <= budget);
            if kept.len() < ab.len() {
                assert!(carry_cost(&ab[ab.len() - kept.len() - 1..]) > budget);
            }
        }
    }

    fn text_of(summary: &Value) -> String {
        match summary.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(parts)) => str_field(&parts[0], "text").to_string(),
            _ => String::new(),
        }
    }

    fn sample_carry() -> Vec<String> {
        [
            carry_part("user: ", "Keep the public API.\n---\nNo new dependencies."),
            carry_part("assistant: ", "Understood.\n\n  ---  \n\nWorking on it."),
            carry_part("user: ", "Also run the benchmarks."),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    #[test]
    fn the_carried_section_is_one_part_after_the_byte_identical_header() {
        let carry = sample_carry();
        let section = format!("{CARRY_LABEL}\n{}", carry.join("\n\n"));
        assert_eq!(section.split(PART_SEPARATOR).count(), 1);
        let mut checked = 0;
        for dialect in DIALECTS {
            for messages in tail_fixtures(dialect) {
                let Some(plain) = compact(&messages, dialect, &cfg(1)) else {
                    continue;
                };
                let step = compact_within(&messages, dialect, &cfg(1), 0, &carry, 24_000).unwrap();
                assert_eq!((step.head_len, step.cut), (plain.head_len, plain.cut));
                assert!(dialect.is_summary(&step.summary));
                let text = text_of(&step.summary);
                let plain_text = text_of(&plain.summary);
                let expected = match plain_text.strip_prefix(&format!("{SUMMARY_HEADER}\n\n")) {
                    Some(rest) => format!("{SUMMARY_HEADER}\n\n{section}{PART_SEPARATOR}{rest}"),
                    None => format!("{SUMMARY_HEADER}\n\n{section}"),
                };
                assert_eq!(text, expected, "{dialect:?}");
                let body = &text[SUMMARY_HEADER.len() + 2..];
                assert_eq!(body.split(PART_SEPARATOR).next(), Some(section.as_str()));
                // The rest of the message is the step without carrying.
                let mut stripped = step.messages.clone();
                stripped[step.head_len] = plain.summary.clone();
                assert_eq!(stripped, plain.messages);
                checked += 1;
            }
        }
        assert!(checked >= 6);
    }

    #[test]
    fn an_empty_carry_or_a_zero_budget_is_the_step_without_carrying() {
        let carry = sample_carry();
        let key = |result: Option<CompactResult>| {
            result.map(|r| (r.messages, r.head_len, r.summary, r.cut, r.carry))
        };
        for dialect in DIALECTS {
            for messages in tail_fixtures(dialect) {
                for keep_recent in 0..4 {
                    let config = cfg(keep_recent);
                    for budget in [0, 8_000, usize::MAX / 2] {
                        let plain = compact_within(&messages, dialect, &config, budget, &[], 0);
                        if budget == 0 {
                            assert_eq!(
                                key(compact(&messages, dialect, &config)),
                                key(plain.clone())
                            );
                        }
                        let off = compact_within(&messages, dialect, &config, budget, &carry, 0);
                        assert_eq!(key(off), key(plain.clone()));
                        // An empty carry renders no section; only the
                        // returned carry can differ.
                        let started =
                            compact_within(&messages, dialect, &config, budget, &[], 24_000);
                        assert_eq!(
                            started.map(|r| (r.messages, r.head_len, r.summary, r.cut)),
                            plain.map(|r| (r.messages, r.head_len, r.summary, r.cut)),
                            "{dialect:?} keep_recent {keep_recent} budget {budget}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_second_step_renders_the_first_steps_words_newest_kept() {
        for dialect in [Dialect::Anthropic, Dialect::ChatCompletions] {
            let mut messages = vec![dialect.user_message("Fix the failing test.".to_string())];
            messages.extend(dialect_steps(dialect, 0..6, 1, 1500));
            let first = compact_within(&messages, dialect, &cfg(1), 0, &[], 24_000).unwrap();
            let words = |range: Range<usize>| -> Vec<String> {
                range.map(|i| format!("assistant: Step {i}.")).collect()
            };
            // The first step has no carry to render, and the task prompt
            // stays in the verbatim head.
            assert_eq!(
                first.summary,
                compact(&messages, dialect, &cfg(1)).unwrap().summary
            );
            assert_eq!(first.carry, words(0..5));
            let mut grown = first.messages.clone();
            grown.extend(dialect_steps(dialect, 6..10, 1, 1500));
            let second = compact_within(&grown, dialect, &cfg(1), 0, &first.carry, 24_000).unwrap();
            let text = text_of(&second.summary);
            let section = format!("{CARRY_LABEL}\n{}", words(0..5).join("\n\n"));
            assert!(text.starts_with(&format!("{SUMMARY_HEADER}\n\n{section}{PART_SEPARATOR}")));
            assert_eq!(text.matches(SUMMARY_HEADER).count(), 1);
            assert_eq!(second.carry, words(0..9));
            assert!(second
                .carry
                .iter()
                .all(|part| !part.contains(SUMMARY_HEADER)));
            // Under a smaller budget the oldest words drop out first, in
            // the section and in the new carry.
            let budget = carry_cost(&words(0..3));
            let tight = compact_within(&grown, dialect, &cfg(1), 0, &first.carry, budget).unwrap();
            let section = format!("{CARRY_LABEL}\n{}", words(2..5).join("\n\n"));
            assert!(text_of(&tight.summary).contains(&section));
            assert!(!text_of(&tight.summary).contains("assistant: Step 1."));
            assert_eq!(tight.carry, words(6..9));
        }
    }

    #[test]
    fn the_tail_extension_leaves_exact_room_for_the_carried_section() {
        let carry: Vec<String> = (0..20)
            .map(|i| format!("user: instruction {i} {}", "w".repeat(300)))
            .collect();
        for dialect in DIALECTS {
            let mut extended = 0;
            for messages in tail_fixtures(dialect) {
                let Some(reference) = compact(&messages, dialect, &cfg(1)) else {
                    continue;
                };
                let mut budget = step_chars(&reference);
                while budget < 400_000 {
                    let step = |budget| {
                        compact_within(&messages, dialect, &cfg(1), budget, &carry, 24_000).unwrap()
                    };
                    let plain =
                        compact_within(&messages, dialect, &cfg(1), budget, &[], 0).unwrap();
                    let reached = step(budget);
                    assert!(reached.cut >= plain.cut);
                    let size = step_chars(&reached);
                    assert_eq!(step(size).cut, reached.cut, "{dialect:?} budget {size}");
                    if reached.cut < reference.cut {
                        assert!(size <= budget);
                        assert!(step(size - 1).cut > reached.cut);
                        extended += 1;
                    }
                    budget = budget * 5 / 4;
                }
            }
            assert!(extended > 0, "{dialect:?}");
        }
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use serde_json::{json, Value};

    pub fn a_user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    pub fn a_assistant(text: &str, tool: Option<(&str, &str, Value)>) -> Value {
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
        if let Some((id, name, input)) = tool {
            content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
        }
        json!({"role": "assistant", "content": content})
    }

    pub fn a_result(id: &str, text: &str) -> Value {
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": text}
        ]})
    }

    /// Task plus `turns` steps; even steps get a long observation.
    pub fn a_session(turns: usize, long_chars: usize) -> Vec<Value> {
        let mut messages = vec![a_user("Fix the failing test in repo X.")];
        for i in 0..turns {
            let id = format!("tu_{i}");
            messages.push(a_assistant(
                &format!("Step {i}: I will inspect module {i} to find the bug."),
                Some((
                    &id,
                    "bash",
                    json!({"command": format!("pytest tests/test_{i}.py -x")}),
                )),
            ));
            if i % 2 == 0 {
                messages.push(a_result(&id, &"X".repeat(long_chars)));
            } else {
                messages.push(a_result(&id, &format!("test_{i} passed (short output)")));
            }
        }
        messages
    }

    pub fn a_body(messages: Vec<Value>) -> serde_json::Map<String, Value> {
        let Value::Object(map) = json!({
            "model": "claude-sonnet-5",
            "max_tokens": 4096,
            "system": "You are a coding agent.",
            "messages": messages,
        }) else {
            unreachable!()
        };
        map
    }

    /// A C2 test configuration: `keep_recent` 3 and a 40% tail budget.
    /// Keep its original carry/tail assumptions independent of tool evidence;
    /// evidence-enabled replay properties have separate regression cases.
    pub fn tail_cfg(threshold_tokens: u64) -> super::CliffConfig {
        super::CliffConfig {
            threshold_tokens,
            keep_recent: 3,
            keep_tail_percent: 40,
            evidence_max_chars: 0,
            ..super::CliffConfig::default()
        }
    }

    /// Result length of step `i`: 3,000 characters every third step and
    /// 700 otherwise, just over the summary's result cap, so summaries stay
    /// small and the tail budget has room to keep extra turns.
    pub fn mixed_results(i: usize) -> usize {
        if i.is_multiple_of(3) {
            3_000
        } else {
            700
        }
    }

    /// `tail_cfg` with a 24,000-character carry, pinned rather than read
    /// from the default; at these small thresholds a quarter of the
    /// headroom binds instead.
    pub fn carry_cfg(threshold_tokens: u64) -> super::CliffConfig {
        super::CliffConfig {
            carry_max_chars: 24_000,
            ..tail_cfg(threshold_tokens)
        }
    }

    /// `tail_cfg` with the carry off: each summary covers only the turns
    /// since the previous compaction, as in the reference.
    pub fn uncarried_cfg(threshold_tokens: u64) -> super::CliffConfig {
        super::CliffConfig {
            carry_max_chars: 0,
            ..tail_cfg(threshold_tokens)
        }
    }

    /// Requests after which the live chain held a nonempty carry and sent a
    /// carried section, compared with the fresh ones.
    pub fn carried(steps: &[ChainStep]) -> usize {
        steps
            .iter()
            .filter(|s| s.carry_parts > 0 && s.carry_chars > 0)
            .count()
    }

    /// `mixed_results` with three 10,000-character results every 20 steps
    /// before step 60, and none after. At a 6,000-token threshold the kept
    /// turns alone then exceed it, so those requests escalate to rung 1.
    pub fn burst_results(i: usize) -> usize {
        if i < 60 && (5..8).contains(&(i % 20)) {
            10_000
        } else {
            mixed_results(i)
        }
    }

    /// Requests where a live chain that differed from a fresh one agrees
    /// with it again.
    pub fn reconvergences(steps: &[ChainStep]) -> usize {
        steps
            .windows(2)
            .filter(|pair| !pair[0].equal && pair[1].equal)
            .count()
    }

    /// One request of a live chain beside a fresh engine that prepares the
    /// same body with an empty store.
    #[derive(Debug)]
    pub struct ChainStep {
        pub compacted: bool,
        pub rung: u8,
        /// Turns after the summary; 0 without one.
        pub tail_turns: usize,
        /// Parts of the live carry after this request; 0 with carrying off.
        pub carry_parts: usize,
        /// Characters of the carried section in the live outgoing summary.
        pub carry_chars: usize,
        /// The outgoing list, `base_cut`, `base_head` and the carry equal
        /// the fresh ones.
        pub equal: bool,
    }

    /// Drive one live engine through `bodies`, a request each, and compare
    /// every request with a fresh engine's `prepare` of the same body.
    pub fn chain_against_fresh(
        cfg: &super::CliffConfig,
        dialect: super::Dialect,
        bodies: &[serde_json::Map<String, Value>],
    ) -> Vec<ChainStep> {
        let live = super::Engine::new(cfg.clone());
        bodies
            .iter()
            .map(|body| {
                let l = live.prepare(body.clone(), dialect).expect("a live request");
                let f = super::Engine::new(cfg.clone())
                    .prepare(body.clone(), dialect)
                    .expect("a fresh request");
                let tail_turns = match l.base_cut {
                    0 => 0,
                    _ => dialect.group_turns(&l.messages()[l.base_head + 1..]).len(),
                };
                ChainStep {
                    compacted: l.compacted,
                    rung: l.rung,
                    tail_turns,
                    carry_parts: l.carry.len(),
                    carry_chars: l.carry_chars,
                    equal: l.messages() == f.messages()
                        && (l.base_cut, l.base_head) == (f.base_cut, f.base_head)
                        && l.carry == f.carry,
                }
            })
            .collect()
    }

    pub fn summaries(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .filter(|text| text.starts_with(super::SUMMARY_HEADER))
            .map(str::to_string)
            .collect()
    }
}

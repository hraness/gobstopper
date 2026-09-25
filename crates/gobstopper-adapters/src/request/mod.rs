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
pub mod chat;
mod engine;
mod images;
pub mod replay;
pub mod responses;
mod store;

pub use engine::{Engine, RequestCtx};
pub use store::{Entry, PrefixStore};

/// Marks an injected summary so re-compaction can drop it. Byte-identical to
/// the reference implementation's header, so a history that passed through
/// either proxy is recognized by both.
pub const SUMMARY_HEADER: &str =
    "The following is a summary of your previous actions (long observations omitted):";
const PART_SEPARATOR: &str = "\n\n---\n\n";

/// Default trigger: below the auto-compaction point of 200k-window clients,
/// so their own summarizer never fires, and a bounded context for 1M-window
/// models.
pub const DEFAULT_THRESHOLD_TOKENS: u64 = 128_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliffConfig {
    /// Compact when the estimated outgoing request exceeds this.
    pub threshold_tokens: u64,
    /// Newest assistant-step turns kept verbatim.
    pub keep_recent: usize,
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
    /// Refuse a request still over budget after the escalation ladder
    /// instead of sending it anyway.
    pub strict: bool,
}

impl Default for CliffConfig {
    fn default() -> Self {
        Self {
            threshold_tokens: DEFAULT_THRESHOLD_TOKENS,
            keep_recent: 3,
            thought_max_chars: 0,
            cmd_max_chars: 150,
            result_max_chars: 500,
            human_max_chars: 20_000,
            keep_thinking: true,
            thinking_max_chars: 0,
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
    /// Responses and Chat Completions keep contiguous model-output runs
    /// together: one model step may span several items.
    fn group_turns(self, body: &[Value]) -> Vec<Range<usize>> {
        match self {
            Self::Anthropic => group_by_starts(body, |i| self.is_assistant(&body[i])),
            Self::Responses | Self::ChatCompletions => group_by_starts(body, |i| {
                self.is_assistant(&body[i]) && (i == 0 || !self.is_assistant(&body[i - 1]))
            }),
        }
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
}

/// One cliff step: `[head] + [summary of older turns] + [newest turns]`.
/// `None` when there is no assistant turn yet, too few turns, or no
/// reduction in message count.
pub fn compact(messages: &[Value], dialect: Dialect, cfg: &CliffConfig) -> Option<CompactResult> {
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
    let split = turns.len() - cfg.keep_recent;
    let parts: Vec<String> = turns[..split]
        .iter()
        .flat_map(|turn| body[turn.clone()].iter())
        .flat_map(|message| dialect.summarize(message, cfg))
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
    let mut out = Vec::with_capacity(head_len + 1 + kept.len());
    out.extend_from_slice(&messages[..head_len]);
    out.push(summary.clone());
    out.extend_from_slice(kept);
    Some(CompactResult {
        messages: out,
        head_len,
        summary,
        cut: kept_start,
    })
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

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use serde_json::json;

    fn cfg(keep_recent: usize) -> CliffConfig {
        CliffConfig {
            keep_recent,
            ..CliffConfig::default()
        }
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

    pub fn summaries(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .filter(|text| text.starts_with(super::SUMMARY_HEADER))
            .map(str::to_string)
            .collect()
    }
}

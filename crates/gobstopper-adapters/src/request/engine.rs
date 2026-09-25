//! Request pipeline: substitute the longest stored prefix, then compact the
//! outgoing request when it is over the threshold.
//!
//! Clients resend their original history on every request, so the store is
//! keyed by original-prefix chain hashes. When an entry matches, the
//! outgoing list is `messages[..base_head] + [summary] + messages[base_cut..]`,
//! and an index `k` in that list maps back to the original list as
//! `base_cut + (k - (base_head + 1))`.

use super::{
    billable_chars, chain_hashes, compact, CliffConfig, CompactResult, Dialect, Entry, PrefixStore,
    PART_SEPARATOR, SUMMARY_HEADER,
};
use serde_json::{Map, Value};
use std::sync::{Mutex, MutexGuard};

/// One request's view: the original history, the list actually sent, and
/// what happened to it. Sizes are billable characters per message plus two
/// for the array separator, so the trigger and the replay agree exactly.
#[derive(Debug, Clone)]
pub struct RequestCtx {
    body: Map<String, Value>,
    pub dialect: Dialect,
    msgs: Vec<Value>,
    msg_chars: Vec<usize>,
    fixed_chars: usize,
    chain: Vec<String>,
    /// Original-prefix length replaced by the summary (0 = none).
    pub base_cut: usize,
    /// Verbatim head messages before the summary.
    pub base_head: usize,
    substituted: Option<(Vec<Value>, Vec<usize>)>,
    /// A stored prefix matched before any compaction in this request.
    pub matched: bool,
    /// The outgoing list differs from the original.
    pub modified: bool,
    /// A compaction ran during this request.
    pub compacted: bool,
    /// Highest escalation rung applied: 0 base, 1 one kept turn, 2 plus
    /// thought caps and no thinking, 3 truncated summary.
    pub rung: u8,
    /// Still over the threshold after the ladder.
    pub over_budget: bool,
    /// The threshold applied to this request: the configured one, or the
    /// verbatim floor (fixed fields plus head) plus half of it when the head
    /// alone would keep every request over the configured value.
    pub threshold_tokens: u64,
    pub est_tokens_in: u64,
    pub est_tokens_out: u64,
    /// Threshold crossings replayed by the last compaction.
    pub chain_steps: usize,
}

impl RequestCtx {
    /// The list that will be sent.
    pub fn messages(&self) -> &[Value] {
        self.substituted
            .as_ref()
            .map_or(self.msgs.as_slice(), |(messages, _)| messages.as_slice())
    }

    fn sizes(&self) -> &[usize] {
        self.substituted
            .as_ref()
            .map_or(self.msg_chars.as_slice(), |(_, sizes)| sizes.as_slice())
    }

    pub fn original_len(&self) -> usize {
        self.msgs.len()
    }

    /// The request body to forward.
    pub fn outgoing_body(&self) -> Value {
        let mut body = self.body.clone();
        body.insert(
            self.dialect.messages_key().to_string(),
            Value::Array(self.messages().to_vec()),
        );
        Value::Object(body)
    }

    fn tokens_of(&self, sizes: &[usize]) -> u64 {
        ((self.fixed_chars + sizes.iter().sum::<usize>()) / 4) as u64
    }

    fn refresh_estimate(&mut self) {
        self.est_tokens_out = self.tokens_of(self.sizes());
    }
}

/// Working state of one replay: `working == replacement + msgs[orig_cut..fed]`
/// where `replacement = msgs[..head_len] + [summary]` once a compaction exists.
struct Replay {
    working: Vec<Value>,
    sizes: Vec<usize>,
    head_len: usize,
    orig_cut: usize,
    have_summary: bool,
    last_summary: Option<Value>,
    steps: usize,
}

impl Replay {
    /// Map a compaction of `working` back to original coordinates and adopt
    /// it. `false` means the result is unusable and the request fails open.
    fn adopt(&mut self, result: CompactResult, original_len: usize) -> bool {
        let new_cut = if self.have_summary {
            if result.cut < self.head_len + 1 {
                return false;
            }
            self.orig_cut + (result.cut - (self.head_len + 1))
        } else {
            result.cut
        };
        if !(1..=original_len).contains(&new_cut) {
            return false;
        }
        let mut sizes = Vec::with_capacity(result.messages.len());
        sizes.extend_from_slice(&self.sizes[..result.head_len]);
        sizes.push(billable_chars(&result.summary) + 2);
        sizes.extend_from_slice(&self.sizes[result.cut..]);
        self.sizes = sizes;
        self.working = result.messages;
        self.head_len = result.head_len;
        self.orig_cut = new_cut;
        self.have_summary = true;
        self.last_summary = Some(result.summary);
        self.steps += 1;
        true
    }
}

pub struct Engine {
    cfg: CliffConfig,
    store: Mutex<PrefixStore>,
}

impl Engine {
    pub fn new(cfg: CliffConfig) -> Self {
        Self::with_store(cfg, PrefixStore::default())
    }

    pub fn with_store(cfg: CliffConfig, store: PrefixStore) -> Self {
        Self {
            cfg,
            store: Mutex::new(store),
        }
    }

    pub fn into_store(self) -> PrefixStore {
        self.store.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> &CliffConfig {
        &self.cfg
    }

    fn store(&self) -> MutexGuard<'_, PrefixStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `(entries, summary characters)` held by the prefix store.
    pub fn store_stats(&self) -> (usize, usize) {
        let store = self.store();
        (store.len(), store.bytes())
    }

    /// Prepare one request. `None` when the body has no non-empty history
    /// of message objects under the dialect's key: pass it through verbatim.
    pub fn prepare(&self, mut body: Map<String, Value>, dialect: Dialect) -> Option<RequestCtx> {
        let msgs = match body.get_mut(dialect.messages_key()) {
            Some(Value::Array(items))
                if !items.is_empty() && items.iter().all(Value::is_object) =>
            {
                std::mem::take(items)
            }
            _ => return None,
        };
        let as_value = Value::Object(body);
        let fixed_chars = billable_chars(&as_value);
        let Value::Object(body) = as_value else {
            return None;
        };
        let msg_chars: Vec<usize> = msgs.iter().map(|m| billable_chars(m) + 2).collect();
        let digests: Vec<String> = msgs.iter().map(|m| dialect.digest(m)).collect();
        let mut ctx = RequestCtx {
            body,
            dialect,
            chain: chain_hashes(&digests),
            msgs,
            msg_chars,
            fixed_chars,
            base_cut: 0,
            base_head: 0,
            substituted: None,
            matched: false,
            modified: false,
            compacted: false,
            rung: 0,
            over_budget: false,
            threshold_tokens: self.cfg.threshold_tokens,
            est_tokens_in: 0,
            est_tokens_out: 0,
            chain_steps: 0,
        };
        ctx.est_tokens_in = ctx.tokens_of(&ctx.msg_chars);
        // The head (everything before the first model turn) is always sent
        // verbatim. A head near the threshold, such as a history the client
        // already compacted itself, would otherwise re-compact every request
        // and never leave a stable prefix; bound growth above it instead.
        let head_end = ctx
            .msgs
            .iter()
            .position(|m| dialect.is_assistant(m))
            .unwrap_or(ctx.msgs.len());
        let floor = ctx.tokens_of(&ctx.msg_chars[..head_end]);
        ctx.threshold_tokens = self
            .cfg
            .threshold_tokens
            .max(floor.saturating_add(self.cfg.threshold_tokens / 2));
        self.substitute_longest_prefix(&mut ctx);
        ctx.refresh_estimate();

        let threshold = ctx.threshold_tokens;
        if ctx.est_tokens_out > threshold {
            self.compact_chain(&mut ctx, false, None);
            // Escalate with harsher knobs while still over budget. Summary
            // truncation is reactive-only unless strict: a soft over-budget
            // send lets oversized content age into the compacted region.
            let rungs: &[u8] = if self.cfg.strict { &[1, 2, 3] } else { &[1, 2] };
            for &rung in rungs {
                if ctx.est_tokens_out <= threshold {
                    break;
                }
                if rung == 3 {
                    if self.truncate_summary(&mut ctx) {
                        ctx.rung = 3;
                    }
                } else if self.compact_chain(&mut ctx, true, Some(&self.rung_cfg(rung))) {
                    ctx.rung = rung;
                }
            }
            ctx.over_budget = ctx.est_tokens_out > threshold;
        }
        Some(ctx)
    }

    /// Called after the provider rejected the request for length. Walks the
    /// ladder one rung per call; `true` means replay `ctx.outgoing_body()`.
    pub fn reactive(&self, ctx: &mut RequestCtx) -> bool {
        if !ctx.compacted && ctx.rung == 0 && self.compact_chain(ctx, true, None) {
            return true;
        }
        while ctx.rung < 3 {
            ctx.rung += 1;
            if ctx.rung < 3 {
                if self.compact_chain(ctx, true, Some(&self.rung_cfg(ctx.rung))) {
                    return true;
                }
            } else if self.truncate_summary(ctx) {
                return true;
            }
        }
        false
    }

    fn rung_cfg(&self, rung: u8) -> CliffConfig {
        let mut cfg = CliffConfig {
            keep_recent: 1,
            ..self.cfg.clone()
        };
        if rung >= 2 {
            cfg.thought_max_chars = match self.cfg.thought_max_chars {
                0 => 300,
                cap => cap.min(300),
            };
            cfg.keep_thinking = false;
        }
        cfg
    }

    fn substitute_longest_prefix(&self, ctx: &mut RequestCtx) {
        let mut store = self.store();
        for i in (0..ctx.msgs.len()).rev() {
            let Some(entry) = store.get(&ctx.chain[i]) else {
                continue;
            };
            if entry.cut != i + 1 || entry.head_len > entry.cut {
                // Inconsistent entry: ignore the store for this request.
                return;
            }
            let mut messages = Vec::with_capacity(entry.head_len + 1 + ctx.msgs.len() - entry.cut);
            messages.extend_from_slice(&ctx.msgs[..entry.head_len]);
            messages.push(entry.summary.clone());
            messages.extend_from_slice(&ctx.msgs[entry.cut..]);
            let mut sizes = Vec::with_capacity(messages.len());
            sizes.extend_from_slice(&ctx.msg_chars[..entry.head_len]);
            sizes.push(billable_chars(&entry.summary) + 2);
            sizes.extend_from_slice(&ctx.msg_chars[entry.cut..]);
            ctx.base_cut = entry.cut;
            ctx.base_head = entry.head_len;
            ctx.substituted = Some((messages, sizes));
            ctx.matched = true;
            ctx.modified = true;
            return;
        }
    }

    /// Compact by replaying threshold crossings over the history: messages
    /// are fed in order and compacted whenever the running size crosses the
    /// threshold, each step dropping the previous summary. A long history
    /// arriving at once (fresh proxy, evicted store) therefore yields the
    /// same recent-only summary an incrementally built chain would. With
    /// `force`, a history that never crosses is compacted once at the end.
    fn compact_chain(
        &self,
        ctx: &mut RequestCtx,
        force: bool,
        knobs: Option<&CliffConfig>,
    ) -> bool {
        let knobs = knobs.unwrap_or(&self.cfg);
        let threshold_chars = ctx.threshold_tokens.saturating_mul(4) as usize;
        let mut replay = if ctx.base_cut > 0 {
            Replay {
                working: ctx.messages()[..=ctx.base_head].to_vec(),
                sizes: ctx.sizes()[..=ctx.base_head].to_vec(),
                head_len: ctx.base_head,
                orig_cut: ctx.base_cut,
                have_summary: true,
                last_summary: None,
                steps: 0,
            }
        } else {
            Replay {
                working: Vec::new(),
                sizes: Vec::new(),
                head_len: 0,
                orig_cut: 0,
                have_summary: false,
                last_summary: None,
                steps: 0,
            }
        };
        let original_len = ctx.msgs.len();
        let mut chars = ctx.fixed_chars + replay.sizes.iter().sum::<usize>();
        for i in replay.orig_cut..original_len {
            replay.working.push(ctx.msgs[i].clone());
            replay.sizes.push(ctx.msg_chars[i]);
            chars += ctx.msg_chars[i];
            if chars > threshold_chars {
                // Not enough turns yet: keep feeding.
                let Some(result) = compact(&replay.working, ctx.dialect, knobs) else {
                    continue;
                };
                if !replay.adopt(result, original_len) {
                    return false;
                }
                chars = ctx.fixed_chars + replay.sizes.iter().sum::<usize>();
            }
        }
        if replay.steps == 0 && force {
            if let Some(result) = compact(&replay.working, ctx.dialect, knobs) {
                if !replay.adopt(result, original_len) {
                    return false;
                }
            }
        }
        let Some(summary) = replay.last_summary.clone() else {
            return false;
        };
        let key = ctx.chain[replay.orig_cut - 1].clone();
        ctx.base_cut = replay.orig_cut;
        ctx.base_head = replay.head_len;
        ctx.chain_steps = replay.steps;
        ctx.substituted = Some((replay.working, replay.sizes));
        ctx.modified = true;
        ctx.compacted = true;
        ctx.refresh_estimate();
        self.store().put(
            key,
            Entry {
                head_len: ctx.base_head,
                summary,
                cut: ctx.base_cut,
            },
        );
        true
    }

    /// Last rung: shrink the current summary to fit the budget, keeping its
    /// newest parts; degenerates to a header-only summary. Never touches the
    /// head or the kept tail. Reactive only, except under strict mode.
    fn truncate_summary(&self, ctx: &mut RequestCtx) -> bool {
        if !ctx.compacted || ctx.base_cut == 0 {
            return false;
        }
        let index = ctx.base_head;
        let text = summary_text(&ctx.messages()[index]);
        let Some(rest) = text.strip_prefix(SUMMARY_HEADER) else {
            return false;
        };
        let others: usize = ctx
            .sizes()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, size)| size)
            .sum();
        let budget = (ctx.threshold_tokens.saturating_mul(4) as i128
            - (ctx.fixed_chars + others) as i128
            - SUMMARY_HEADER.len() as i128
            - 64)
            .max(0) as usize;
        let parts: Vec<&str> = rest.trim().split(PART_SEPARATOR).collect();
        let mut kept = Vec::new();
        let mut used = 0usize;
        for part in parts.iter().rev() {
            let len = part.chars().count();
            if used + len > budget {
                break;
            }
            kept.push(*part);
            // Separator overhead, rounded up as in the reference implementation.
            used += len + 9;
        }
        kept.reverse();
        let new_text = if kept.is_empty() {
            SUMMARY_HEADER.to_string()
        } else {
            format!("{SUMMARY_HEADER}\n\n{}", kept.join(PART_SEPARATOR))
        };
        if new_text.chars().count() >= text.chars().count() {
            return false;
        }
        let summary = ctx.dialect.user_message(new_text);
        let size = billable_chars(&summary) + 2;
        let Some((messages, sizes)) = ctx.substituted.as_mut() else {
            return false;
        };
        messages[index] = summary.clone();
        sizes[index] = size;
        ctx.modified = true;
        ctx.refresh_estimate();
        let key = ctx.chain[ctx.base_cut - 1].clone();
        self.store().put(
            key,
            Entry {
                head_len: ctx.base_head,
                summary,
                cut: ctx.base_cut,
            },
        );
        true
    }
}

fn summary_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find_map(|block| block.get("text").and_then(Value::as_str))
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::*;
    use super::super::replay;
    use super::*;
    use serde_json::json;

    fn grow(messages: &[Value], start: usize, steps: usize, result_chars: usize) -> Vec<Value> {
        let mut out = messages.to_vec();
        for i in start..start + steps {
            let id = format!("tu_{i}");
            out.push(a_assistant(
                &format!("Step {i}"),
                Some((&id, "bash", json!({"command": format!("cmd {i}")}))),
            ));
            out.push(a_result(&id, &"R".repeat(result_chars)));
        }
        out
    }

    fn engine(threshold_tokens: u64, keep_recent: usize) -> Engine {
        Engine::new(CliffConfig {
            threshold_tokens,
            keep_recent,
            ..CliffConfig::default()
        })
    }

    fn fat_session(turns: usize, result_chars: usize) -> Vec<Value> {
        let mut messages = vec![a_user("the task: fix the bug")];
        for i in 0..turns {
            messages.push(json!({"role": "assistant", "content": [
                {"type": "text", "text": format!("thought {i}: {}", "y".repeat(2000))},
                {"type": "tool_use", "id": format!("t{i}"), "name": "bash", "input": {"command": format!("make {i}")}}
            ]}));
            messages.push(a_result(&format!("t{i}"), &"R".repeat(result_chars)));
        }
        messages
    }

    #[test]
    fn under_threshold_passes_through() {
        let engine = engine(1_000_000, 3);
        let ctx = engine
            .prepare(a_body(a_session(4, 3000)), Dialect::Anthropic)
            .unwrap();
        assert!(!ctx.modified && !ctx.compacted);
        assert_eq!(ctx.messages(), a_session(4, 3000).as_slice());
        assert_eq!(ctx.est_tokens_in, ctx.est_tokens_out);
    }

    #[test]
    fn missing_or_malformed_history_is_not_prepared() {
        let engine = engine(10, 1);
        let mut body = a_body(Vec::new());
        assert!(engine.prepare(body.clone(), Dialect::Anthropic).is_none());
        body.insert("messages".into(), json!(["not an object"]));
        assert!(engine.prepare(body, Dialect::Anthropic).is_none());
        assert!(engine
            .prepare(a_body(a_session(3, 10)), Dialect::Responses)
            .is_none());
    }

    #[test]
    fn compacts_stores_and_substitutes_on_the_next_request() {
        let engine = engine(2_000, 1);
        let messages = a_session(10, 3000);
        let first = engine
            .prepare(a_body(messages.clone()), Dialect::Anthropic)
            .unwrap();
        assert!(first.compacted && first.modified);
        assert_eq!(summaries(first.messages()).len(), 1);
        assert_eq!(engine.store_stats().0, 1);
        assert!(first.est_tokens_out < first.est_tokens_in);

        // The client keeps growing its original history; it never saw the summary.
        let grown = grow(&messages, 10, 1, 10);
        let second = engine
            .prepare(a_body(grown.clone()), Dialect::Anthropic)
            .unwrap();
        assert!(second.matched && second.modified);
        let out = second.messages();
        assert_eq!(out[0], grown[0]);
        assert!(summaries(&out[1..2]).len() == 1);
        assert_eq!(out[2..], grown[second.base_cut..]);
        // Stable prefix: the same summary bytes as the first request sent.
        assert_eq!(out[1], first.messages()[1]);
    }

    #[test]
    fn recompaction_is_flat_and_the_deepest_prefix_wins() {
        let engine = engine(2_000, 1);
        let messages = a_session(10, 3000);
        engine
            .prepare(a_body(messages.clone()), Dialect::Anthropic)
            .unwrap();
        let grown = grow(&messages, 10, 8, 2000);
        let ctx = engine
            .prepare(a_body(grown.clone()), Dialect::Anthropic)
            .unwrap();
        assert!(ctx.compacted);
        assert_eq!(summaries(ctx.messages()).len(), 1);
        assert_eq!(ctx.messages().last(), grown.last());
        assert_eq!(engine.store_stats().0, 2);
        let third = grow(&grown, 18, 1, 10);
        let ctx = engine.prepare(a_body(third), Dialect::Anthropic).unwrap();
        assert!(ctx.base_cut > messages.len());
        assert_eq!(summaries(ctx.messages()).len(), 1);
    }

    #[test]
    fn a_rewritten_history_does_not_match() {
        let engine = engine(2_000, 1);
        let messages = a_session(10, 3000);
        engine
            .prepare(a_body(messages.clone()), Dialect::Anthropic)
            .unwrap();
        let mut mutated = messages.clone();
        mutated[3] = a_user("history rewritten by the client");
        let relaxed = Engine::with_store(
            CliffConfig {
                threshold_tokens: 1_000_000,
                ..CliffConfig::default()
            },
            engine.into_store(),
        );
        let ctx = relaxed
            .prepare(a_body(mutated), Dialect::Anthropic)
            .unwrap();
        assert!(!ctx.modified);
    }

    #[test]
    fn a_fresh_engine_replays_crossings_instead_of_accumulating() {
        let cfg = CliffConfig {
            threshold_tokens: 2_000,
            keep_recent: 1,
            ..CliffConfig::default()
        };
        let live = Engine::new(cfg.clone());
        let mut messages = a_session(2, 3000);
        let mut last = None;
        for i in 2..60 {
            last = live.prepare(a_body(messages.clone()), Dialect::Anthropic);
            messages = grow(&messages, i, 1, 2000);
        }
        let live_summary = summaries(last.unwrap().messages())[0].clone();

        let fresh = Engine::new(cfg.clone());
        let ctx = fresh.prepare(a_body(messages), Dialect::Anthropic).unwrap();
        assert!(ctx.compacted && ctx.chain_steps > 1);
        let fresh_summary = summaries(ctx.messages())[0].clone();
        assert!(!fresh_summary.contains("Step 2") && !fresh_summary.contains("Step 10"));
        assert!(fresh_summary.len() < 3 * live_summary.len().max(1_000));
        assert!(ctx.est_tokens_out <= cfg.threshold_tokens * 2);
    }

    #[test]
    fn reactive_compacts_regardless_of_threshold_and_terminates() {
        let engine = engine(1_000_000, 1);
        let mut ctx = engine
            .prepare(a_body(a_session(10, 3000)), Dialect::Anthropic)
            .unwrap();
        assert!(!ctx.modified);
        assert!(engine.reactive(&mut ctx));
        assert!(ctx.compacted);
        assert_eq!(summaries(ctx.messages()).len(), 1);
        let mut attempts = 0;
        while engine.reactive(&mut ctx) && attempts < 10 {
            attempts += 1;
        }
        assert!(attempts < 10);
        assert_eq!(ctx.rung, 3);
    }

    #[test]
    fn proactive_escalation_reaches_one_kept_turn_when_needed() {
        let engine = engine(4_000, 3);
        let ctx = engine
            .prepare(a_body(fat_session(8, 6000)), Dialect::Anthropic)
            .unwrap();
        assert!(ctx.compacted && ctx.rung >= 1);
        assert!(ctx.est_tokens_out <= 4_000);
        let assistants = ctx
            .messages()
            .iter()
            .filter(|m| m["role"] == "assistant")
            .count();
        assert_eq!(assistants, 1);
    }

    #[test]
    fn a_giant_live_turn_is_sent_soft_and_no_assistant_means_no_change() {
        let engine = engine(1_000, 3);
        let ctx = engine
            .prepare(a_body(fat_session(1, 40_000)), Dialect::Anthropic)
            .unwrap();
        assert!(ctx.over_budget);
        assert!(ctx.outgoing_body().is_object());
        let quiet = self::engine(100, 3);
        let ctx = quiet
            .prepare(
                a_body(vec![a_user(&"x".repeat(30_000))]),
                Dialect::Anthropic,
            )
            .unwrap();
        assert!(!ctx.compacted && !ctx.modified && ctx.rung == 0);
    }

    /// A summary that is mostly verbatim human text cannot shrink below the
    /// budget by recompaction; only summary truncation gets it there.
    fn human_heavy_session() -> Vec<Value> {
        vec![
            a_user("task"),
            a_assistant("a0", None),
            a_user(&format!("u0: {}", "U".repeat(12_000))),
            a_assistant("a1", None),
            a_user("u1"),
        ]
    }

    #[test]
    fn strict_mode_truncates_the_summary_before_giving_up() {
        let strict = Engine::new(CliffConfig {
            threshold_tokens: 2_000,
            keep_recent: 1,
            strict: true,
            ..CliffConfig::default()
        });
        let ctx = strict
            .prepare(a_body(human_heavy_session()), Dialect::Anthropic)
            .unwrap();
        assert!(ctx.compacted);
        assert_eq!(ctx.rung, 3);
        assert!(!ctx.over_budget);
        assert_eq!(summaries(ctx.messages()), vec![SUMMARY_HEADER.to_string()]);

        let soft = engine(2_000, 1);
        let ctx = soft
            .prepare(a_body(human_heavy_session()), Dialect::Anthropic)
            .unwrap();
        assert!(
            ctx.compacted && ctx.over_budget,
            "default mode sends over budget"
        );
        assert_eq!(ctx.rung, 0);
    }

    #[test]
    fn summary_truncation_keeps_the_newest_parts() {
        let mut messages = vec![a_user("task")];
        for i in 0..6 {
            messages.push(a_assistant(&format!("a{i}"), None));
            messages.push(a_user(&format!("u{i}: {}", "x".repeat(1_000))));
        }
        messages.push(a_assistant("a6", None));
        messages.push(a_user("u6"));
        let relaxed = engine(1_000_000, 1);
        let mut ctx = relaxed
            .prepare(a_body(messages), Dialect::Anthropic)
            .unwrap();
        assert!(relaxed.reactive(&mut ctx));
        let before = summaries(ctx.messages())[0].clone();
        assert!(before.contains("u0:") && before.contains("u5:"));

        ctx.threshold_tokens = 700;
        assert!(relaxed.truncate_summary(&mut ctx));
        let after = summaries(ctx.messages())[0].clone();
        assert!(after.len() < before.len());
        assert!(after.contains("u5:") && after.contains("a5"));
        assert!(!after.contains("u0:"));
    }

    #[test]
    fn a_head_near_the_threshold_raises_it_instead_of_recompacting_every_request() {
        // A history the client already compacted itself: a large verbatim
        // head, then ordinary steps.
        let mut messages = vec![a_user(&"h".repeat(9_000))];
        messages.extend(a_session(12, 3000).into_iter().skip(1));
        let engine = engine(2_000, 1);
        let first = engine
            .prepare(a_body(messages.clone()), Dialect::Anthropic)
            .unwrap();
        assert!(first.threshold_tokens > 2_000);
        assert!(first.compacted && !first.over_budget);
        // One more small step reuses the stored prefix instead of compacting again.
        let mut grown = messages.clone();
        grown.push(a_assistant("Done.", None));
        grown.push(a_user("thanks"));
        let second = engine.prepare(a_body(grown), Dialect::Anthropic).unwrap();
        assert!(second.matched && !second.compacted);
        assert_eq!(second.messages()[1], first.messages()[1]);
    }

    #[test]
    fn outgoing_body_keeps_other_fields_and_key_order() {
        let engine = engine(2_000, 1);
        let ctx = engine
            .prepare(a_body(a_session(10, 3000)), Dialect::Anthropic)
            .unwrap();
        let body = ctx.outgoing_body();
        let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["model", "max_tokens", "system", "messages"]);
        assert_eq!(body["system"], "You are a coding agent.");
    }

    #[test]
    fn chat_completions_compact_and_keep_tool_pairing() {
        let mut messages = vec![
            json!({"role": "system", "content": "you are an agent"}),
            json!({"role": "user", "content": "the task"}),
        ];
        for i in 0..10 {
            messages.push(json!({"role": "assistant", "content": format!("step {i}"),
                "tool_calls": [{"id": format!("call_{i}"), "type": "function",
                    "function": {"name": "bash", "arguments": format!("{{\"cmd\":\"s{i}\"}}")}}]}));
            messages.push(json!({"role": "tool", "tool_call_id": format!("call_{i}"),
                "content": "R".repeat(3000)}));
        }
        let Value::Object(body) = json!({
            "model": "gpt-x",
            "stream": true,
            "tools": [{"type": "function", "function": {"name": "bash"}}],
            "messages": messages.clone(),
        }) else {
            unreachable!()
        };
        let engine = engine(2_000, 1);
        let ctx = engine.prepare(body, Dialect::ChatCompletions).unwrap();
        assert!(ctx.compacted);
        assert!(replay::pairing_intact(
            ctx.messages(),
            Dialect::ChatCompletions
        ));
        let out = ctx.outgoing_body();
        assert_eq!(out["stream"], true);
        assert!(out["tools"].is_array());
        assert!(out["messages"].as_array().unwrap().len() < messages.len());
    }
}

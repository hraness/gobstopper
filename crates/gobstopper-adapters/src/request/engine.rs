//! Request pipeline: substitute the longest stored prefix, then compact the
//! outgoing request when it is over the threshold.
//!
//! Clients resend their original history on every request, so the store is
//! keyed by original-prefix chain hashes. When an entry matches, the
//! outgoing list is `messages[..base_head] + [summary] + messages[base_cut..]`,
//! and an index `k` in that list maps back to the original list as
//! `base_cut + (k - (base_head + 1))`.

use super::calibrate::{calibrated_threshold, MAX_RATIO_PERMILLE, MIN_RATIO_PERMILLE};
use super::{
    billable_chars, chain_hashes, compact_within, CliffConfig, CompactResult, Dialect, Entry,
    PrefixStore, CARRY_LABEL, MAX_KEEP_TAIL_PERCENT, PART_SEPARATOR, SUMMARY_HEADER,
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
    /// The verbatim floor: fixed fields plus the original head (messages
    /// before the first assistant message).
    floor_chars: usize,
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
    /// The threshold applied to this request: `base_threshold_tokens`, or
    /// the verbatim floor (fixed fields plus head) plus half of it when the
    /// head alone would keep every request over that value.
    pub threshold_tokens: u64,
    pub est_tokens_in: u64,
    pub est_tokens_out: u64,
    /// Threshold crossings replayed by the last compaction.
    pub chain_steps: usize,
    /// Estimated tokens of the fixed fields (system prompt, tools and
    /// other top-level fields); constant for one request.
    pub est_fixed_tokens: u64,
    /// Estimated tokens of the outgoing list, in three parts: the verbatim
    /// head before the summary, the summary, and the messages after it.
    /// Without a summary the head and summary are 0 and every message
    /// counts as tail. Sizes only; set with `est_tokens_out`.
    pub est_head_tokens: u64,
    pub est_summary_tokens: u64,
    pub est_tail_tokens: u64,
    /// The threshold selected for this request before the floor rule: the
    /// configured one for `Engine::prepare`, or the value passed to
    /// `Engine::prepare_at`. Stored entries record it, and only entries
    /// with the same value are substituted.
    pub base_threshold_tokens: u64,
    /// The estimate calibration applied to this request, in thousandths
    /// (1000 = none); see [`super::calibrate`].
    pub ratio_permille: u32,
    /// `base_threshold_tokens` divided by the calibration ratio, before the
    /// floor rule. Equal to `base_threshold_tokens` without calibration and
    /// never above it.
    pub calibrated_threshold_tokens: u64,
    /// The words the next compaction carries into its summary, oldest
    /// first: restored from a substituted entry and replaced by each
    /// compaction. Content, so it never leaves the request module: the
    /// shared test fixtures there compare it between chains.
    pub(super) carry: Vec<String>,
    /// Characters of the carried section in the outgoing summary, label
    /// included; 0 without a summary or without a section. A size only;
    /// set with `est_tokens_out`.
    pub carry_chars: usize,
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

    /// Room above the verbatim floor under the applied threshold.
    fn headroom_chars(&self) -> usize {
        (self.threshold_tokens.saturating_mul(4) as usize).saturating_sub(self.floor_chars)
    }

    /// The share of the headroom a step with `knobs` may fill with its
    /// summary and kept tail; 0 keeps exactly `keep_recent` turns.
    fn tail_budget_chars(&self, knobs: &CliffConfig) -> usize {
        let percent = knobs.keep_tail_percent.min(MAX_KEEP_TAIL_PERCENT);
        (self.headroom_chars() as u128 * u128::from(percent) / 100) as usize
    }

    fn refresh_estimate(&mut self) {
        let sizes = self.sizes();
        let est_out = self.tokens_of(sizes);
        let tokens = |part: &[usize]| (part.iter().sum::<usize>() / 4) as u64;
        let (head, summary, tail) = match (self.substituted.is_some(), sizes.get(self.base_head)) {
            (true, Some(summary)) => (
                tokens(sizes.get(..self.base_head).unwrap_or(&[])),
                (*summary / 4) as u64,
                tokens(sizes.get(self.base_head + 1..).unwrap_or(&[])),
            ),
            _ => (0, 0, tokens(sizes)),
        };
        let carry_chars = match self.substituted {
            Some(_) => self
                .messages()
                .get(self.base_head)
                .map_or(0, |summary| section_chars(summary_str(summary))),
            None => 0,
        };
        self.est_tokens_out = est_out;
        self.est_head_tokens = head;
        self.est_summary_tokens = summary;
        self.est_tail_tokens = tail;
        self.carry_chars = carry_chars;
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
    /// The carry the next step renders and extends.
    carry: Vec<String>,
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
        self.carry = result.carry;
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

    /// `(entries, characters)` held by the prefix store: summary
    /// characters plus carried characters.
    pub fn store_stats(&self) -> (usize, usize) {
        let store = self.store();
        (store.len(), store.bytes())
    }

    /// Prepare one request. `None` when the body has no non-empty history
    /// of message objects under the dialect's key: pass it through verbatim.
    pub fn prepare(&self, body: Map<String, Value>, dialect: Dialect) -> Option<RequestCtx> {
        self.prepare_at(body, dialect, self.cfg.threshold_tokens)
    }

    /// `prepare` with a threshold selected for this request in place of the
    /// configured one, such as a larger one for a request that declares a
    /// longer context window. Every other setting comes from the config.
    pub fn prepare_at(
        &self,
        body: Map<String, Value>,
        dialect: Dialect,
        threshold_tokens: u64,
    ) -> Option<RequestCtx> {
        self.prepare_calibrated(body, dialect, threshold_tokens, MIN_RATIO_PERMILLE)
    }

    /// `prepare_at` with the estimate scaled by `ratio_permille` thousandths
    /// before it is compared with the threshold: the request compacts when
    /// its estimate exceeds `threshold_tokens * 1000 / ratio_permille`. The
    /// ratio is clamped to 1000..=2000, so calibration can only compact
    /// earlier; at 1000 this is exactly `prepare_at`. Stored prefixes stay
    /// keyed by `threshold_tokens`, so a changing ratio keeps reusing them.
    pub fn prepare_calibrated(
        &self,
        mut body: Map<String, Value>,
        dialect: Dialect,
        threshold_tokens: u64,
        ratio_permille: u32,
    ) -> Option<RequestCtx> {
        let ratio_permille = ratio_permille.clamp(MIN_RATIO_PERMILLE, MAX_RATIO_PERMILLE);
        let calibrated = calibrated_threshold(threshold_tokens, ratio_permille);
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
            floor_chars: fixed_chars,
            base_cut: 0,
            base_head: 0,
            substituted: None,
            matched: false,
            modified: false,
            compacted: false,
            rung: 0,
            over_budget: false,
            threshold_tokens: calibrated,
            est_tokens_in: 0,
            est_tokens_out: 0,
            chain_steps: 0,
            est_fixed_tokens: (fixed_chars / 4) as u64,
            est_head_tokens: 0,
            est_summary_tokens: 0,
            est_tail_tokens: 0,
            base_threshold_tokens: threshold_tokens,
            ratio_permille,
            calibrated_threshold_tokens: calibrated,
            carry: Vec::new(),
            carry_chars: 0,
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
        ctx.floor_chars = fixed_chars + ctx.msg_chars[..head_end].iter().sum::<usize>();
        let floor = (ctx.floor_chars / 4) as u64;
        ctx.threshold_tokens = calibrated.max(floor.saturating_add(calibrated / 2));
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
        // The provider refused the size, so the first step keeps exactly
        // `keep_recent` turns instead of growing the tail.
        let first = CliffConfig {
            keep_tail_percent: 0,
            ..self.cfg.clone()
        };
        if !ctx.compacted && ctx.rung == 0 && self.compact_chain(ctx, true, Some(&first)) {
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
            keep_tail_percent: 0,
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
            if entry.base_threshold_tokens != ctx.base_threshold_tokens {
                // Computed under another threshold: a fresh prepare at this
                // one would not produce it. Try a shallower prefix.
                continue;
            }
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
            ctx.carry = entry.carry;
            ctx.matched = true;
            ctx.modified = true;
            return;
        }
    }

    /// Compact by replaying threshold crossings over the history: messages
    /// are fed in order and compacted whenever the running size crosses the
    /// threshold, each step dropping the previous summary and passing its
    /// carried words to the next. A long history arriving at once (fresh
    /// proxy, evicted store) therefore yields the same summary an
    /// incrementally built chain would, and the same carry while the carry
    /// budget is unchanged, which always holds when `carry_max_chars` is
    /// less than half the base threshold, rounded down (at the default,
    /// above 48,001 tokens).
    /// Otherwise, and after a lossy `truncate_summary`, the two carries
    /// agree again once newer words fill the budget. With `force`, a
    /// history that never crosses is compacted once at the end.
    fn compact_chain(
        &self,
        ctx: &mut RequestCtx,
        force: bool,
        knobs: Option<&CliffConfig>,
    ) -> bool {
        let knobs = knobs.unwrap_or(&self.cfg);
        let threshold_chars = ctx.threshold_tokens.saturating_mul(4) as usize;
        // Constant for the request, so every replayed crossing uses it.
        let budget = ctx.tail_budget_chars(knobs);
        let carry_budget = self.carry_budget(ctx);
        let mut replay = if ctx.base_cut > 0 {
            Replay {
                working: ctx.messages()[..=ctx.base_head].to_vec(),
                sizes: ctx.sizes()[..=ctx.base_head].to_vec(),
                head_len: ctx.base_head,
                orig_cut: ctx.base_cut,
                have_summary: true,
                last_summary: None,
                steps: 0,
                carry: ctx.carry.clone(),
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
                carry: Vec::new(),
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
                let Some(result) = compact_within(
                    &replay.working,
                    ctx.dialect,
                    knobs,
                    budget,
                    &replay.carry,
                    carry_budget,
                ) else {
                    continue;
                };
                if !replay.adopt(result, original_len) {
                    return false;
                }
                chars = ctx.fixed_chars + replay.sizes.iter().sum::<usize>();
            }
        }
        if replay.steps == 0 && force {
            if let Some(result) = compact_within(
                &replay.working,
                ctx.dialect,
                knobs,
                budget,
                &replay.carry,
                carry_budget,
            ) {
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
        ctx.carry = replay.carry;
        ctx.modified = true;
        ctx.compacted = true;
        ctx.refresh_estimate();
        self.store().put(
            key,
            Entry {
                head_len: ctx.base_head,
                summary,
                cut: ctx.base_cut,
                base_threshold_tokens: ctx.base_threshold_tokens,
                carry: ctx.carry.clone(),
            },
        );
        true
    }

    /// Characters of carried words a summary may show for this request:
    /// the configured maximum, capped at a quarter of the headroom. It
    /// depends only on the config and the request, never on a step's
    /// knobs, so the base step, the reactive first step and every rung
    /// bound the carry alike and their chains reconverge.
    fn carry_budget(&self, ctx: &RequestCtx) -> usize {
        self.cfg.carry_max_chars.min(ctx.headroom_chars() / 4)
    }

    /// Last rung: shrink the current summary to fit the budget, keeping its
    /// newest parts; degenerates to a header-only summary. Never touches the
    /// head or the kept tail. The carried section is the oldest part, so it
    /// goes first, and dropping any part empties the carry. Reactive only,
    /// except under strict mode.
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
        let dropped = kept.len() < parts.len();
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
        if dropped {
            // The newest cycle's words sit in merged summary parts that map
            // back to no carried part, so the only carry that never re-adds
            // text this truncation removed is an empty one.
            ctx.carry.clear();
        }
        ctx.modified = true;
        ctx.refresh_estimate();
        let key = ctx.chain[ctx.base_cut - 1].clone();
        self.store().put(
            key,
            Entry {
                head_len: ctx.base_head,
                summary,
                cut: ctx.base_cut,
                base_threshold_tokens: ctx.base_threshold_tokens,
                carry: ctx.carry.clone(),
            },
        );
        true
    }
}

fn summary_text(message: &Value) -> String {
    summary_str(message).to_string()
}

fn summary_str(message: &Value) -> &str {
    match message.get("content") {
        Some(Value::String(text)) => text,
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find_map(|block| block.get("text").and_then(Value::as_str))
            .unwrap_or(""),
        _ => "",
    }
}

/// Characters of the carried section in a summary's text, label included:
/// the first part after the header when it starts with the label line, up
/// to the next part separator. Carried parts hold no `---` line and no
/// summary part of a new cycle starts with the label line, so this is
/// exact. 0 for any other text.
fn section_chars(text: &str) -> usize {
    let Some(rest) = text
        .strip_prefix(SUMMARY_HEADER)
        .and_then(|rest| rest.strip_prefix("\n\n"))
    else {
        return 0;
    };
    let label_line = rest
        .strip_prefix(CARRY_LABEL)
        .is_some_and(|after| after.starts_with('\n'));
    if !label_line {
        return 0;
    }
    rest.split(PART_SEPARATOR)
        .next()
        .map_or(0, |section| section.chars().count())
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
    fn size_fields_split_the_outgoing_estimate() {
        let tokens = |messages: &[Value]| {
            (messages
                .iter()
                .map(|m| billable_chars(m) + 2)
                .sum::<usize>()
                / 4) as u64
        };
        let roomy = engine(1_000_000, 3)
            .prepare(a_body(a_session(4, 3000)), Dialect::Anthropic)
            .unwrap();
        assert_eq!((roomy.est_head_tokens, roomy.est_summary_tokens), (0, 0));
        assert_eq!(roomy.est_tail_tokens, tokens(roomy.messages()));
        assert!(roomy.est_tokens_out - roomy.est_fixed_tokens - roomy.est_tail_tokens <= 1);

        let ctx = engine(2_000, 1)
            .prepare(a_body(a_session(10, 3000)), Dialect::Anthropic)
            .unwrap();
        assert!(ctx.compacted);
        let out = ctx.messages();
        let head = ctx.base_head;
        assert_eq!(ctx.est_head_tokens, tokens(&out[..head]));
        assert_eq!(ctx.est_summary_tokens, tokens(&out[head..=head]));
        assert_eq!(ctx.est_tail_tokens, tokens(&out[head + 1..]));
        let parts = ctx.est_fixed_tokens
            + ctx.est_head_tokens
            + ctx.est_summary_tokens
            + ctx.est_tail_tokens;
        assert!(parts <= ctx.est_tokens_out && ctx.est_tokens_out - parts <= 3);
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
        // Each pass dropped the previous summary instead of nesting it. The
        // old steps' words may still be carried, but their tool calls are
        // not.
        assert_eq!(fresh_summary.matches(SUMMARY_HEADER).count(), 1);
        assert!(!fresh_summary.contains("\"cmd 2\"") && !fresh_summary.contains("\"cmd 10\""));
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
        // The reference tail isolates the floor rule; the companion test
        // below covers the default tail budget.
        let mut messages = vec![a_user(&"h".repeat(9_000))];
        messages.extend(a_session(12, 3000).into_iter().skip(1));
        let engine = tail_engine(2_000, 1, 0);
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
    fn a_head_near_the_threshold_recompacts_at_most_once_more_at_the_default_tail() {
        let mut messages = vec![a_user(&"h".repeat(9_000))];
        messages.extend(a_session(12, 3000).into_iter().skip(1));
        let engine = engine(2_000, 1);
        assert!(engine.config().keep_tail_percent > 0);
        let first = engine
            .prepare(a_body(messages.clone()), Dialect::Anthropic)
            .unwrap();
        assert!(first.threshold_tokens > 2_000);
        assert!(first.compacted && !first.over_budget);
        // Small steps: at most one more compaction, then the stored prefix
        // is reused on every request.
        let mut grown = messages;
        let mut compacted = Vec::new();
        let mut stable = 0;
        for i in 0..10 {
            grown.push(a_assistant(&format!("Done {i}."), None));
            grown.push(a_user(&format!("thanks {i}")));
            let ctx = engine
                .prepare(a_body(grown.clone()), Dialect::Anthropic)
                .unwrap();
            assert!(!ctx.over_budget);
            if ctx.compacted {
                assert_eq!(stable, 0, "request {i} recompacted after reuse");
                compacted.push(i);
            } else {
                assert!(ctx.matched, "request {i} neither compacted nor reused");
                stable += 1;
            }
        }
        assert!(compacted.len() <= 1, "recompacted on {compacted:?}");
        assert!(stable >= 9);
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

    fn tail_engine(threshold_tokens: u64, keep_recent: usize, keep_tail_percent: u8) -> Engine {
        Engine::new(CliffConfig {
            threshold_tokens,
            keep_recent,
            keep_tail_percent,
            ..CliffConfig::default()
        })
    }

    fn kept_turns(ctx: &RequestCtx) -> usize {
        ctx.messages()
            .iter()
            .filter(|m| ctx.dialect.is_assistant(m))
            .count()
    }

    /// `(request, rung, kept turns)` for every compacting request of a
    /// session whose first cycle summarizes a 15,000-character human
    /// message, followed by small text turns.
    fn text_heavy_compactions(keep_tail_percent: u8) -> Vec<(usize, u8, usize)> {
        let engine = tail_engine(5_000, 3, keep_tail_percent);
        let mut messages = vec![
            a_user("task"),
            a_assistant("a0", None),
            a_user(&format!("u0: {}", "U".repeat(15_000))),
        ];
        let mut log = Vec::new();
        for i in 1..60 {
            messages.push(a_assistant(&format!("a{i} {}", "t".repeat(120)), None));
            messages.push(a_user(&format!("u{i} {}", "v".repeat(120))));
            let ctx = engine
                .prepare(a_body(messages.clone()), Dialect::Anthropic)
                .unwrap();
            if ctx.compacted {
                log.push((i, ctx.rung, kept_turns(&ctx)));
            }
        }
        log
    }

    #[test]
    fn a_large_prior_summary_still_compacts_on_the_base_step() {
        // The second cycle starts with a summary near the whole headroom and
        // small turns after it. The tail may grow by one turn but never
        // swallows the first assistant turn, so the base step still compacts
        // instead of falling to rung 1 with one kept turn.
        assert_eq!(text_heavy_compactions(0), [(14, 0, 3), (16, 0, 3)]);
        assert_eq!(text_heavy_compactions(50), [(14, 0, 3), (16, 0, 4)]);
    }

    #[test]
    fn the_reactive_first_step_keeps_exactly_keep_recent_turns() {
        let engine = tail_engine(1_000_000, 1, 60);
        let mut ctx = engine
            .prepare(a_body(a_session(10, 3000)), Dialect::Anthropic)
            .unwrap();
        assert!(!ctx.compacted);
        // The base step's budget would hold the whole history.
        assert!(ctx.tail_budget_chars(engine.config()) > 4 * ctx.est_tokens_in as usize);
        assert!(engine.reactive(&mut ctx));
        assert_eq!(ctx.rung, 0);
        assert_eq!(kept_turns(&ctx), 1);
    }

    #[test]
    fn rungs_keep_one_turn_whatever_the_tail_percent() {
        let engine = tail_engine(4_000, 3, 60);
        for rung in [1, 2] {
            let knobs = engine.rung_cfg(rung);
            assert_eq!((knobs.keep_recent, knobs.keep_tail_percent), (1, 0));
        }
    }

    #[test]
    fn the_tail_percent_is_capped_at_sixty() {
        let capped = tail_engine(3_000, 1, MAX_KEEP_TAIL_PERCENT);
        let over = tail_engine(3_000, 1, u8::MAX);
        let off = tail_engine(3_000, 1, 0);
        let mut messages = vec![a_user("the task")];
        let mut differs = false;
        for step in 0..40 {
            messages = grow(&messages, step, 1, 700 + 300 * (step % 3));
            let a = capped
                .prepare(a_body(messages.clone()), Dialect::Anthropic)
                .unwrap();
            let b = over
                .prepare(a_body(messages.clone()), Dialect::Anthropic)
                .unwrap();
            let c = off
                .prepare(a_body(messages.clone()), Dialect::Anthropic)
                .unwrap();
            assert_eq!(a.messages(), b.messages(), "step {step}");
            differs |= a.messages() != c.messages();
        }
        assert!(differs, "the tail budget changes the outgoing list");
    }

    /// Request bodies of a session that grows one tool step per request;
    /// `size(i)` is the length of step i's result.
    fn stepped_bodies(steps: usize, size: impl Fn(usize) -> usize) -> Vec<Map<String, Value>> {
        let mut messages = vec![a_user("the task")];
        (0..steps)
            .map(|i| {
                let id = format!("tu_{i}");
                messages.push(a_assistant(
                    &format!("Step {i} {}", "t".repeat(200)),
                    Some((&id, "bash", json!({"command": format!("cmd {i}")}))),
                ));
                messages.push(a_result(&id, &"R".repeat(size(i))));
                a_body(messages.clone())
            })
            .collect()
    }

    fn out_chars(ctx: &RequestCtx) -> usize {
        ctx.fixed_chars + ctx.sizes().iter().sum::<usize>()
    }

    #[test]
    fn a_live_chain_equals_a_fresh_prepare_at_a_tail_percent() {
        let steps = chain_against_fresh(
            &tail_cfg(12_000),
            Dialect::Anthropic,
            &stepped_bodies(100, mixed_results),
        );
        let unequal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].equal).collect();
        assert!(unequal.is_empty(), "requests {unequal:?} differ");
        assert!(steps.iter().filter(|s| s.compacted).count() >= 4);
        assert!(steps.iter().all(|s| s.rung == 0));
        // The budget kept more than `keep_recent` turns.
        assert!(steps.iter().any(|s| s.compacted && s.tail_turns > 3));
    }

    #[test]
    fn a_rung_one_burst_reconverges_with_a_fresh_prepare() {
        // Without a carry; `a_rung_one_burst_with_a_carry_reconverges_with_a_fresh_prepare`
        // covers the carried chain.
        let steps = chain_against_fresh(
            &uncarried_cfg(6_000),
            Dialect::Anthropic,
            &stepped_bodies(90, burst_results),
        );
        assert!(steps.iter().any(|s| s.rung == 1));
        // A rung-1 request stores a deeper cut than a fresh replay makes, so
        // the chains differ during a burst and agree again after it.
        assert!(reconvergences(&steps) >= 3);
        // After the last burst the chains agree for good.
        assert!(steps[60..].iter().all(|s| s.equal));
    }

    #[test]
    fn small_requests_after_a_compaction_reuse_the_prefix_until_the_margin_is_used() {
        // Without a carry, so the summary and tail fill at most the 40%
        // target. A carried section takes up to a quarter of the headroom
        // on top; `at_16000_tokens_a_full_carry_leaves_the_prefix_reused_until_the_margin_is_used`
        // covers that case.
        let engine = Engine::new(uncarried_cfg(10_000));
        let bodies = stepped_bodies(130, |_| 700);
        let step_chars = bodies
            .iter()
            .map(|body| {
                let messages = body["messages"].as_array().unwrap();
                messages[messages.len() - 2..]
                    .iter()
                    .map(|m| billable_chars(m) + 2)
                    .sum::<usize>()
            })
            .max()
            .unwrap();
        let mut compactions = Vec::new();
        let mut margin = 0;
        for (i, body) in bodies.into_iter().enumerate() {
            let ctx = engine.prepare(body, Dialect::Anthropic).unwrap();
            if ctx.compacted {
                assert_eq!(ctx.rung, 0, "request {i}");
                // Summary and kept tail fit the target; only the rest of
                // the crossing step rides above it.
                let bound = ctx.floor_chars + ctx.tail_budget_chars(engine.config());
                assert!(out_chars(&ctx) <= bound + step_chars, "request {i}");
                margin = ctx.threshold_tokens as usize * 4 - bound;
                compactions.push(i);
            } else if !compactions.is_empty() {
                assert!(ctx.matched && !ctx.over_budget, "request {i}");
            }
        }
        assert!(compactions.len() >= 3);
        // (100 - p)% of the headroom stays free after each compaction.
        assert!(margin / step_chars >= 15, "{margin} / {step_chars}");
        for pair in compactions.windows(2) {
            assert!(pair[1] - pair[0] >= margin / step_chars - 1, "{pair:?}");
        }
    }

    #[test]
    fn a_minimum_tail_over_the_target_is_kept_as_the_reference_keeps_it() {
        // Without a carry: at 6,000 tokens a carried section would push
        // three 6,000-character turns over the threshold, and the request
        // would escalate to rung 1 instead of keeping the minimum tail.
        let uncarried = |keep_tail_percent| {
            Engine::new(CliffConfig {
                keep_tail_percent,
                ..uncarried_cfg(6_000)
            })
        };
        let tail = uncarried(40);
        let reference = uncarried(0);
        let mut compactions = 0;
        for (i, body) in stepped_bodies(40, |_| 6_000).into_iter().enumerate() {
            let a = tail.prepare(body.clone(), Dialect::Anthropic).unwrap();
            let b = reference.prepare(body, Dialect::Anthropic).unwrap();
            assert_eq!(a.messages(), b.messages(), "request {i}");
            if a.compacted {
                compactions += 1;
                assert_eq!((a.rung, kept_turns(&a)), (0, 3), "request {i}");
                let target = a.floor_chars + a.tail_budget_chars(tail.config());
                assert!(out_chars(&a) > target, "request {i}");
            }
        }
        assert!(compactions >= 2);
    }

    #[test]
    fn prepare_is_prepare_at_the_configured_threshold() {
        let configured = tail_engine(4_000, 3, 40);
        let selected = tail_engine(4_000, 3, 40);
        let mut compactions = 0;
        for (i, body) in stepped_bodies(60, mixed_results).into_iter().enumerate() {
            let a = configured
                .prepare(body.clone(), Dialect::Anthropic)
                .unwrap();
            let b = selected
                .prepare_at(body, Dialect::Anthropic, 4_000)
                .unwrap();
            assert_eq!(a.messages(), b.messages(), "request {i}");
            assert_eq!(
                (a.base_cut, a.base_head, a.matched, a.compacted, a.rung),
                (b.base_cut, b.base_head, b.matched, b.compacted, b.rung),
                "request {i}"
            );
            assert_eq!(
                (a.threshold_tokens, a.est_tokens_out),
                (b.threshold_tokens, b.est_tokens_out),
                "request {i}"
            );
            assert_eq!(
                (a.base_threshold_tokens, b.base_threshold_tokens),
                (4_000, 4_000)
            );
            compactions += usize::from(a.compacted);
        }
        assert!(compactions >= 2);
    }

    #[test]
    fn the_floor_rule_raises_the_selected_threshold() {
        let mut messages = vec![a_user(&"h".repeat(9_000))];
        messages.extend(a_session(16, 3000).into_iter().skip(1));
        let engine = tail_engine(2_000, 1, 0);
        let floor = |ctx: &RequestCtx| (ctx.floor_chars / 4) as u64;

        let configured = engine
            .prepare(a_body(messages.clone()), Dialect::Anthropic)
            .unwrap();
        assert_eq!(configured.base_threshold_tokens, 2_000);
        assert_eq!(configured.threshold_tokens, floor(&configured) + 1_000);

        // A smaller selected threshold is raised by half of itself, not by
        // half of the configured one.
        let small = engine
            .prepare_at(a_body(messages.clone()), Dialect::Anthropic, 1_000)
            .unwrap();
        assert_eq!(small.base_threshold_tokens, 1_000);
        assert_eq!(small.threshold_tokens, floor(&small) + 500);
        assert!(small.compacted && !small.over_budget);

        // A selected threshold with room above the head is applied as is,
        // though the configured one would have been raised.
        let large = engine
            .prepare_at(a_body(messages), Dialect::Anthropic, 8_000)
            .unwrap();
        assert!(floor(&large) + 4_000 < 8_000);
        assert_eq!(
            (large.base_threshold_tokens, large.threshold_tokens),
            (8_000, 8_000)
        );
        assert!(large.compacted && !large.over_budget);
        assert!(large.est_tokens_out <= 8_000);
    }

    #[test]
    fn a_stored_entry_from_another_threshold_is_never_substituted() {
        let bodies = stepped_bodies(40, mixed_results);
        let shared = tail_engine(2_000, 1, 40);

        // A 6,000-token request stores an entry for its regime.
        let wide = shared
            .prepare_at(bodies[30].clone(), Dialect::Anthropic, 6_000)
            .unwrap();
        assert!(wide.compacted && !wide.over_budget);
        assert_eq!(wide.rung, 0, "the stored entry comes from the base step");
        let wide_cut = wide.base_cut;

        // The next request at the configured threshold finds only that
        // entry: it is skipped, and the recompute equals a fresh prepare.
        let base = shared
            .prepare(bodies[31].clone(), Dialect::Anthropic)
            .unwrap();
        let fresh = tail_engine(2_000, 1, 40)
            .prepare(bodies[31].clone(), Dialect::Anthropic)
            .unwrap();
        assert!(!base.matched && base.compacted);
        assert_eq!(base.messages(), fresh.messages());
        assert_eq!(
            (base.base_cut, base.base_head),
            (fresh.base_cut, fresh.base_head)
        );
        assert!(base.base_cut > wide_cut, "the base entry is deeper");

        // Back at 6,000: the deeper base entry is skipped and the walk
        // continues to the shallower entry of the same regime, which the
        // small growth leaves under the threshold.
        let again = shared
            .prepare_at(bodies[32].clone(), Dialect::Anthropic, 6_000)
            .unwrap();
        assert!(again.matched && !again.compacted);
        assert_eq!(again.base_cut, wide_cut);
        assert_eq!(
            again.messages()[again.base_head],
            wide.messages()[wide.base_head]
        );
        let fresh_wide = tail_engine(2_000, 1, 40)
            .prepare_at(bodies[32].clone(), Dialect::Anthropic, 6_000)
            .unwrap();
        assert!(!fresh_wide.matched);
        assert_eq!(fresh_wide.base_threshold_tokens, 6_000);
        assert_eq!(again.messages(), fresh_wide.messages());
        assert_eq!(
            (again.base_cut, again.base_head),
            (fresh_wide.base_cut, fresh_wide.base_head)
        );
    }

    // C3: carried words across compactions.

    use super::super::CARRY_PART_MAX_CHARS;

    /// `stepped_bodies` with conversation: every fifth step is a text-only
    /// reply followed by a human instruction, so a carry holds both.
    fn talk_bodies(steps: usize, size: impl Fn(usize) -> usize) -> Vec<Map<String, Value>> {
        let mut messages = vec![a_user("the task")];
        (0..steps)
            .map(|i| {
                if i % 5 == 4 {
                    messages.push(a_assistant(&format!("Reply {i}: part {i} is done."), None));
                    messages.push(a_user(&format!(
                        "Instruction {i}: now take part {}.",
                        i + 1
                    )));
                } else {
                    let id = format!("tu_{i}");
                    messages.push(a_assistant(
                        &format!("Step {i} {}", "t".repeat(200)),
                        Some((&id, "bash", json!({"command": format!("cmd {i}")}))),
                    ));
                    messages.push(a_result(&id, &"R".repeat(size(i))));
                }
                a_body(messages.clone())
            })
            .collect()
    }

    fn carry_engine(threshold_tokens: u64, carry_max_chars: usize) -> Engine {
        Engine::new(CliffConfig {
            carry_max_chars,
            ..tail_cfg(threshold_tokens)
        })
    }

    /// What the bound charges for `parts`: characters plus 2 each.
    fn carry_cost(parts: &[String]) -> usize {
        parts.iter().map(|part| part.chars().count() + 2).sum()
    }

    #[test]
    fn a_later_compaction_shows_the_words_of_the_earlier_cycles() {
        let bodies = talk_bodies(60, mixed_results);
        let on = carry_engine(6_000, 24_000);
        let off = carry_engine(6_000, 0);
        let mut first_carry: Option<Vec<String>> = None;
        let mut later = 0;
        for (i, body) in bodies.iter().enumerate() {
            let a = on.prepare(body.clone(), Dialect::Anthropic).unwrap();
            if a.compacted {
                let text = summaries(a.messages())[0].clone();
                match &first_carry {
                    None => {
                        // Nothing to carry yet: the summary is the one
                        // without carrying, and the carry starts here.
                        let b = off.prepare(body.clone(), Dialect::Anthropic).unwrap();
                        assert_eq!(a.messages(), b.messages(), "request {i}");
                        assert!(!text.contains(CARRY_LABEL), "request {i}");
                        let carry = a.carry.clone();
                        assert!(carry.iter().any(|p| p.starts_with("assistant: Step 0 ")));
                        assert!(carry.iter().any(|p| p.starts_with("user: Instruction 4:")));
                        assert!(carry_cost(&carry) <= on.carry_budget(&a));
                        first_carry = Some(carry);
                    }
                    Some(carry) if later == 0 => {
                        // The first cycle's summary is gone, its words are not.
                        let section = format!("{CARRY_LABEL}\n{}", carry.join("\n\n"));
                        let expected = format!("{SUMMARY_HEADER}\n\n{section}{PART_SEPARATOR}");
                        assert!(text.starts_with(&expected), "request {i}");
                        assert!(text.contains("Step 0 "), "request {i}");
                        later += 1;
                    }
                    Some(_) => later += 1,
                }
            }
        }
        assert!(later >= 2);

        // Carrying off: after the first compaction the first cycle is gone.
        let fresh = carry_engine(6_000, 0);
        let texts: Vec<String> = bodies
            .iter()
            .map(|body| fresh.prepare(body.clone(), Dialect::Anthropic).unwrap())
            .filter(|ctx| ctx.compacted)
            .map(|ctx| summaries(ctx.messages())[0].clone())
            .collect();
        assert!(texts.len() >= 3);
        assert!(texts[0].contains("Step 0 "));
        assert!(texts[1..].iter().all(|text| !text.contains("Step 0 ")));
        assert!(texts.iter().all(|text| !text.contains(CARRY_LABEL)));
    }

    #[test]
    fn a_substituted_entry_restores_its_carry() {
        let engine = carry_engine(6_000, 24_000);
        let mut last: Option<Vec<String>> = None;
        let mut restored = 0;
        for (i, body) in talk_bodies(40, mixed_results).into_iter().enumerate() {
            let ctx = engine.prepare(body, Dialect::Anthropic).unwrap();
            if ctx.compacted {
                let entry = engine.store().get(&ctx.chain[ctx.base_cut - 1]).unwrap();
                assert_eq!(entry.carry, ctx.carry, "request {i}");
                assert!(!ctx.carry.is_empty(), "request {i}");
                last = Some(ctx.carry.clone());
            } else if let Some(carry) = &last {
                assert!(ctx.matched, "request {i}");
                assert_eq!(&ctx.carry, carry, "request {i}");
                restored += 1;
            } else {
                assert!(ctx.carry.is_empty(), "request {i}");
            }
        }
        assert!(restored >= 5);
    }

    #[test]
    fn the_carry_budget_comes_from_the_config_and_never_from_a_steps_knobs() {
        let body = talk_bodies(40, mixed_results).pop().unwrap();
        let engine = carry_engine(6_000, 400);
        let base = engine.prepare(body.clone(), Dialect::Anthropic).unwrap();
        assert!(base.compacted);
        assert_eq!(engine.carry_budget(&base), 400, "the maximum binds");
        assert!(!base.carry.is_empty() && carry_cost(&base.carry) <= 400);

        // Rungs 1-2 and the reactive first step, with knobs that ask for
        // other budgets, bound the carry with the request's budget.
        let first = CliffConfig {
            keep_tail_percent: 0,
            ..engine.config().clone()
        };
        for knobs in [engine.rung_cfg(1), engine.rung_cfg(2), first] {
            let run = |carry_max_chars| {
                let mut ctx = base.clone();
                let knobs = CliffConfig {
                    carry_max_chars,
                    ..knobs.clone()
                };
                assert!(engine.compact_chain(&mut ctx, true, Some(&knobs)));
                (ctx.messages().to_vec(), ctx.carry)
            };
            let (none, carry) = run(0);
            assert_eq!((none, carry.clone()), run(1_000_000));
            assert!(!carry.is_empty() && carry_cost(&carry) <= 400);
            assert!(summaries(&run(0).0)[0].contains(CARRY_LABEL));
        }

        // A quarter of the headroom caps a large maximum.
        let capped = carry_engine(2_000, 1_000_000);
        let ctx = capped.prepare(body, Dialect::Anthropic).unwrap();
        assert!(ctx.compacted);
        let budget = capped.carry_budget(&ctx);
        assert_eq!(budget, ctx.headroom_chars() / 4);
        assert!(budget < 4_000);
        assert!(!ctx.carry.is_empty() && carry_cost(&ctx.carry) <= budget);
    }

    #[test]
    fn a_lossy_truncation_empties_the_carry_and_a_no_op_keeps_it() {
        let engine = carry_engine(6_000, 24_000);
        let mut ctx = talk_bodies(60, mixed_results)
            .into_iter()
            .map(|body| engine.prepare(body, Dialect::Anthropic).unwrap())
            .filter(|ctx| ctx.compacted && summaries(ctx.messages())[0].contains(CARRY_LABEL))
            .last()
            .unwrap();
        let key = ctx.chain[ctx.base_cut - 1].clone();
        let carry = ctx.carry.clone();
        assert!(!carry.is_empty());

        // The summary already fits: nothing is dropped and nothing changes.
        let threshold = ctx.threshold_tokens;
        ctx.threshold_tokens = 10_000_000;
        assert!(!engine.truncate_summary(&mut ctx));
        assert_eq!(ctx.carry, carry);
        assert_eq!(engine.store().get(&key).map(|e| e.carry), Some(carry));
        ctx.threshold_tokens = threshold;

        // A budget that fits every part but the oldest drops exactly the
        // carried section, and with it the carry.
        let before = summaries(ctx.messages())[0].clone();
        let parts: Vec<&str> = before
            .strip_prefix(SUMMARY_HEADER)
            .unwrap()
            .trim()
            .split(PART_SEPARATOR)
            .collect();
        assert!(parts[0].starts_with(CARRY_LABEL) && parts.len() > 1);
        let index = ctx.base_head;
        let others: usize = ctx
            .sizes()
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, size)| size)
            .sum();
        let keep: usize = parts[1..].iter().map(|p| p.chars().count() + 9).sum();
        let chars = ctx.fixed_chars + others + SUMMARY_HEADER.len() + 64 + keep;
        ctx.threshold_tokens = (chars / 4 + 1) as u64;
        assert!(engine.truncate_summary(&mut ctx));
        let after = summaries(ctx.messages())[0].clone();
        let expected = format!("{SUMMARY_HEADER}\n\n{}", parts[1..].join(PART_SEPARATOR));
        assert_eq!(after, expected);
        assert!(ctx.carry.is_empty());
        assert_eq!(engine.store().get(&key).map(|e| e.carry), Some(Vec::new()));
    }

    #[test]
    fn a_long_chain_never_carries_a_summary_and_keeps_the_newest_words() {
        let engine = carry_engine(4_000, 24_000);
        let mut sections = 0;
        let mut oldest_dropped = false;
        for (i, body) in talk_bodies(100, mixed_results).into_iter().enumerate() {
            let ctx = engine.prepare(body, Dialect::Anthropic).unwrap();
            assert!(
                ctx.carry.iter().all(|part| !part.contains(SUMMARY_HEADER)),
                "request {i}"
            );
            assert!(
                carry_cost(&ctx.carry) <= engine.carry_budget(&ctx),
                "request {i}"
            );
            if let Some(text) = summaries(ctx.messages()).first() {
                assert_eq!(text.matches(SUMMARY_HEADER).count(), 1, "request {i}");
                assert!(text.matches(CARRY_LABEL).count() <= 1, "request {i}");
                sections += usize::from(text.contains(CARRY_LABEL));
            }
            oldest_dropped |=
                !ctx.carry.is_empty() && ctx.carry.iter().all(|part| !part.contains("Step 0 "));
        }
        assert!(sections > 10);
        assert!(oldest_dropped, "a full carry drops its oldest words first");
    }

    #[test]
    fn a_hostile_carry_through_the_reactive_path_terminates() {
        let engine = carry_engine(1_000_000, 24_000);
        // Oversized words with separators and unclosed reminder tags.
        let mut messages = vec![a_user("task")];
        for i in 0..12 {
            messages.push(a_assistant(
                &format!(
                    "{i} {}\n\n---\n\n<system-reminder> never closed",
                    "a".repeat(5_000)
                ),
                None,
            ));
            messages.push(a_user(&format!(
                "<system-reminder>The user sent a new message while you were working:\n\
                 q{i}\n\n---\n\n</system-reminder>{}\n---\n<system-reminder>open",
                "u".repeat(4_500)
            )));
        }
        let mut ctx = engine
            .prepare(a_body(messages), Dialect::Anthropic)
            .unwrap();
        assert!(engine.reactive(&mut ctx));
        assert!(!ctx.carry.is_empty());
        // The closed spans are stripped and never carried as queued
        // messages; an unclosed tag after other text stays, as typed.
        assert!(ctx.carry.iter().all(|part| {
            part.chars().count() <= CARRY_PART_MAX_CHARS
                && !part.contains(PART_SEPARATOR)
                && !part.contains("</system-reminder>")
                && !part.starts_with("user: q")
        }));

        // A carry no extraction produces: one huge part, 10,000 tiny ones,
        // a part holding a separator and the header, and a tag run.
        let mut hostile = vec!["z".repeat(100_000)];
        hostile.extend((0..10_000).map(|i| format!("user: {i}")));
        hostile.push(format!("assistant: x{PART_SEPARATOR}{SUMMARY_HEADER}"));
        hostile.push("<system-reminder>".repeat(100));
        hostile.extend((0..3).map(|i| format!("user: last {i}")));
        ctx.carry = hostile;
        let mut attempts = 0;
        while engine.reactive(&mut ctx) && attempts < 10 {
            attempts += 1;
        }
        assert!(attempts < 10);
        assert_eq!(ctx.rung, 3);
        assert!(carry_cost(&ctx.carry) <= engine.carry_budget(&ctx));
    }

    #[test]
    fn section_chars_reads_only_a_leading_carried_section() {
        let label = CARRY_LABEL.chars().count();
        let with = |body: &str| format!("{SUMMARY_HEADER}\n\n{body}");
        assert_eq!(section_chars(SUMMARY_HEADER), 0);
        assert_eq!(section_chars(&with("user: a")), 0);
        assert_eq!(
            section_chars(&with(&format!("{CARRY_LABEL}\nuser: é"))),
            label + 8
        );
        let two = format!("{CARRY_LABEL}\nuser: a\n\nassistant: b{PART_SEPARATOR}user: c");
        assert_eq!(section_chars(&with(&two)), label + 1 + 7 + 2 + 12);
        // The label must be the first part and a line of its own.
        let later = format!("user: a{PART_SEPARATOR}{CARRY_LABEL}\nuser: b");
        assert_eq!(section_chars(&with(&later)), 0);
        let glued = format!("{CARRY_LABEL} user: a");
        assert_eq!(section_chars(&with(&glued)), 0);
        assert_eq!(section_chars(&format!("{CARRY_LABEL}\nuser: a")), 0);
    }

    /// Characters of a section showing `parts`: the label line, then the
    /// parts joined by blank lines. 0 for no parts.
    fn shown_chars(parts: &[String]) -> usize {
        match parts.len() {
            0 => 0,
            n => {
                CARRY_LABEL.chars().count()
                    + 1
                    + parts.iter().map(|p| p.chars().count()).sum::<usize>()
                    + 2 * (n - 1)
            }
        }
    }

    #[test]
    fn carry_chars_is_the_section_after_compaction_substitution_and_truncation() {
        let engine = carry_engine(6_000, 24_000);
        let off = carry_engine(6_000, 0);
        // What the summary being sent shows: the carry before the request
        // that compacted it (one crossing per request here).
        let mut before: Vec<String> = Vec::new();
        let mut shown: Vec<String> = Vec::new();
        let (mut compacted, mut substituted) = (0, 0);
        let mut last = None;
        for (i, body) in talk_bodies(60, mixed_results).into_iter().enumerate() {
            let ctx = engine.prepare(body.clone(), Dialect::Anthropic).unwrap();
            if ctx.compacted {
                assert_eq!((ctx.chain_steps, ctx.rung), (1, 0), "request {i}");
                shown = before.clone();
            }
            let expected = if ctx.base_cut == 0 {
                0
            } else {
                shown_chars(&shown)
            };
            assert_eq!(ctx.carry_chars, expected, "request {i}");
            compacted += usize::from(ctx.compacted && expected > 0);
            substituted += usize::from(!ctx.compacted && ctx.matched && expected > 0);
            before = ctx.carry.clone();
            let plain = off.prepare(body, Dialect::Anthropic).unwrap();
            assert_eq!(plain.carry_chars, 0, "request {i}");
            if ctx.compacted && expected > 0 {
                last = Some(ctx);
            }
        }
        assert!(compacted >= 2 && substituted >= 5);

        // Rung 3 (a compacting request's last rung) drops the section
        // first: no section, no carried characters.
        let mut ctx = last.unwrap();
        assert!(ctx.carry_chars > 0);
        ctx.threshold_tokens = 1;
        assert!(engine.truncate_summary(&mut ctx));
        assert_eq!(ctx.carry_chars, 0);
        assert!(!summaries(ctx.messages())[0].contains(CARRY_LABEL));
    }

    // C3-7: determinism and prefix reuse with a nonempty carry.

    #[test]
    fn a_live_chain_with_a_carry_equals_a_fresh_prepare() {
        let steps = chain_against_fresh(
            &carry_cfg(12_000),
            Dialect::Anthropic,
            &talk_bodies(100, mixed_results),
        );
        let unequal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].equal).collect();
        assert!(unequal.is_empty(), "requests {unequal:?} differ");
        assert!(steps.iter().filter(|s| s.compacted).count() >= 4);
        assert!(steps.iter().all(|s| s.rung == 0));
        // Nonempty carries and carried sections were compared.
        assert!(carried(&steps) >= 20, "{}", carried(&steps));
    }

    /// `stepped_bodies` with Claude Code's captured and binary-derived shapes
    /// mixed in: a mid-turn message as a system message after a tool result
    /// (layout 1), text typed after an interrupt (layout 2), a coordinator
    /// system message (layout 7) and feedback typed when rejecting a call.
    fn shaped_bodies(steps: usize, size: impl Fn(usize) -> usize) -> Vec<Map<String, Value>> {
        let system =
            |text: String| json!({"role": "system", "content": [{"type": "text", "text": text}]});
        let mut messages = vec![a_user("the task")];
        (0..steps)
            .map(|i| {
                let id = format!("tu_{i}");
                messages.push(a_assistant(
                    &format!("Step {i} {}", "t".repeat(200)),
                    Some((&id, "bash", json!({"command": format!("cmd {i}")}))),
                ));
                let result = a_result(&id, &"R".repeat(size(i)));
                match i % 5 {
                    1 => messages.extend([
                        result,
                        system(format!(
                            "The user sent a new message while you were working:\n\
                             Mid-turn {i}: check part {i}."
                        )),
                    ]),
                    2 => messages.push(json!({"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": id, "is_error": true,
                         "content": "The user doesn't want to proceed with this tool use."},
                        {"type": "text", "text": "[Request interrupted by user for tool use]\n"},
                        {"type": "text", "text": format!("Typed {i}: skip part {i}.")}
                    ]})),
                    3 => messages.extend([
                        result,
                        system(format!(
                            "The coordinator sent a message while you were working:\n\
                             Coordinator {i}: rebase part {i}."
                        )),
                    ]),
                    4 => messages.push(json!({"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": id, "is_error": true,
                         "content": format!("The user doesn't want to proceed with this tool use. \
                            The tool use was rejected (eg. if it was a file edit, the new_string \
                            was NOT written to the file). To tell you how to proceed, the user \
                            said:\nRejected {i}: use part {i}.")}
                    ]})),
                    _ => messages.push(result),
                }
                a_body(messages.clone())
            })
            .collect()
    }

    #[test]
    fn a_live_chain_with_captured_shapes_equals_a_fresh_prepare() {
        let bodies = shaped_bodies(100, mixed_results);
        let steps = chain_against_fresh(&carry_cfg(12_000), Dialect::Anthropic, &bodies);
        let unequal: Vec<usize> = (0..steps.len()).filter(|&i| !steps[i].equal).collect();
        assert!(unequal.is_empty(), "requests {unequal:?} differ");
        assert!(steps.iter().filter(|s| s.compacted).count() >= 4);
        assert!(carried(&steps) >= 20, "{}", carried(&steps));
        // The live carry holds every shape's words.
        let engine = Engine::new(carry_cfg(12_000));
        let mut carry = Vec::new();
        for body in &bodies {
            carry = engine
                .prepare(body.clone(), Dialect::Anthropic)
                .unwrap()
                .carry;
        }
        for shape in [
            "user: Mid-turn",
            "user: Typed",
            "user: Coordinator",
            "user: Rejected",
        ] {
            assert!(carry.iter().any(|part| part.starts_with(shape)), "{shape}");
        }
        // Extraction splits at every turn start: the parts of a history are
        // the parts of its halves.
        let messages = bodies.last().unwrap()["messages"].as_array().unwrap();
        let whole = super::super::anthropic::carry_parts(messages);
        for start in (1..messages.len()).filter(|&k| messages[k]["role"] == "assistant") {
            let mut halves = super::super::anthropic::carry_parts(&messages[..start]);
            halves.extend(super::super::anthropic::carry_parts(&messages[start..]));
            assert_eq!(halves, whole, "split at {start}");
        }
    }

    #[test]
    fn a_rung_one_burst_with_a_carry_reconverges_with_a_fresh_prepare() {
        let steps = chain_against_fresh(
            &carry_cfg(6_000),
            Dialect::Anthropic,
            &talk_bodies(90, burst_results),
        );
        assert!(steps.iter().any(|s| s.rung == 1));
        // The first burst's rung-1 cut makes the chains differ; they agree
        // again with a nonempty carry, so the carries reconverged too. Later
        // bursts here happen not to split them.
        assert!(reconvergences(&steps) >= 1);
        let again = (1..steps.len())
            .find(|&i| !steps[i - 1].equal && steps[i].equal)
            .unwrap();
        assert!(steps[again].carry_parts > 0, "request {again}");
        // After the last burst the chains agree for good, carry included.
        assert!(steps[60..].iter().all(|s| s.equal));
        assert!(carried(&steps[60..]) >= 10, "{}", carried(&steps[60..]));
    }

    #[test]
    fn at_16000_tokens_a_full_carry_leaves_the_prefix_reused_until_the_margin_is_used() {
        let engine = carry_engine(16_000, 24_000);
        let bodies = talk_bodies(260, |_| 700);
        let step_chars = bodies
            .iter()
            .map(|body| {
                let messages = body["messages"].as_array().unwrap();
                messages[messages.len() - 2..]
                    .iter()
                    .map(|m| billable_chars(m) + 2)
                    .sum::<usize>()
            })
            .max()
            .unwrap();
        // The carry the summary being sent shows (one crossing per request).
        let mut before: Vec<String> = Vec::new();
        let mut compactions: Vec<(usize, usize)> = Vec::new();
        let mut full = 0;
        for (i, body) in bodies.into_iter().enumerate() {
            let ctx = engine.prepare(body, Dialect::Anthropic).unwrap();
            if ctx.compacted {
                assert_eq!((ctx.chain_steps, ctx.rung), (1, 0), "request {i}");
                let budget = engine.carry_budget(&ctx);
                // A quarter of the headroom binds below 48,000 tokens.
                assert!((8_000..24_000).contains(&budget), "request {i}: {budget}");
                // Full: no further step reply would fit, and the oldest
                // cycle's words are gone.
                if carry_cost(&before) + 250 > budget {
                    assert!(carry_cost(&before) <= budget, "request {i}");
                    assert!(!summaries(ctx.messages())[0].contains("Step 0 "));
                    full += 1;
                }
                // What stays free for the requests after it: at least half
                // of the headroom, since the carry takes at most a quarter.
                let free = (ctx.threshold_tokens as usize * 4).saturating_sub(out_chars(&ctx));
                assert!(free >= ctx.headroom_chars() / 2, "request {i}: {free}");
                compactions.push((i, free));
            } else if !compactions.is_empty() {
                // The request after a compaction and the ones after it
                // reuse the stored prefix and stay under the threshold.
                assert!(ctx.matched && !ctx.over_budget, "request {i}");
            }
            before = ctx.carry.clone();
        }
        assert!(full >= 2, "{full} compactions with a full carry");
        for pair in compactions.windows(2) {
            let ((a, free), (b, _)) = (pair[0], pair[1]);
            assert!(b - a >= free / step_chars - 1, "{pair:?} step {step_chars}");
        }
    }

    #[test]
    fn a_ratio_of_one_is_exactly_prepare_at() {
        for ratio in [0, 1, 999, 1000] {
            let plain = tail_engine(4_000, 3, 40);
            let calibrated = tail_engine(4_000, 3, 40);
            let mut compactions = 0;
            for (i, body) in stepped_bodies(60, mixed_results).into_iter().enumerate() {
                let a = plain
                    .prepare_at(body.clone(), Dialect::Anthropic, 4_000)
                    .unwrap();
                let b = calibrated
                    .prepare_calibrated(body, Dialect::Anthropic, 4_000, ratio)
                    .unwrap();
                assert_eq!(a.outgoing_body(), b.outgoing_body(), "request {i}");
                assert_eq!(
                    (a.base_cut, a.base_head, a.matched, a.compacted, a.rung),
                    (b.base_cut, b.base_head, b.matched, b.compacted, b.rung),
                    "request {i}"
                );
                assert_eq!(
                    (a.threshold_tokens, a.est_tokens_in, a.est_tokens_out),
                    (b.threshold_tokens, b.est_tokens_in, b.est_tokens_out),
                    "request {i}"
                );
                assert_eq!(
                    (b.ratio_permille, b.calibrated_threshold_tokens),
                    (1000, 4_000)
                );
                compactions += usize::from(a.compacted);
            }
            assert!(compactions >= 2);
            assert_eq!(plain.store_stats(), calibrated.store_stats());
        }
    }

    #[test]
    fn a_calibrated_request_compacts_earlier_and_never_later() {
        let bodies = stepped_bodies(60, mixed_results);
        let first_compaction = |ratio: u32| {
            let engine = tail_engine(4_000, 3, 40);
            bodies.iter().position(|body| {
                engine
                    .prepare_calibrated(body.clone(), Dialect::Anthropic, 4_000, ratio)
                    .unwrap()
                    .compacted
            })
        };
        let plain = first_compaction(1000).unwrap();
        let calibrated = first_compaction(1250).unwrap();
        let halved = first_compaction(2000).unwrap();
        assert!(calibrated < plain, "{calibrated} < {plain}");
        assert!(halved <= calibrated);
        // Above 2.0 the ratio is clamped.
        assert_eq!(first_compaction(u32::MAX), Some(halved));

        let engine = tail_engine(4_000, 3, 40);
        for body in &bodies {
            let ctx = engine
                .prepare_calibrated(body.clone(), Dialect::Anthropic, 4_000, 1250)
                .unwrap();
            assert_eq!(ctx.ratio_permille, 1250);
            assert_eq!(ctx.calibrated_threshold_tokens, 3_200);
            assert_eq!(ctx.base_threshold_tokens, 4_000);
            assert!(ctx.threshold_tokens >= 3_200);
            if !ctx.over_budget {
                assert!(ctx.est_tokens_out <= ctx.threshold_tokens);
            }
        }
    }

    #[test]
    fn a_changed_ratio_keeps_reusing_the_stored_prefix() {
        let engine = tail_engine(4_000, 3, 40);
        let bodies = stepped_bodies(60, mixed_results);
        let at = bodies
            .iter()
            .position(|body| {
                engine
                    .prepare_calibrated(body.clone(), Dialect::Anthropic, 4_000, 1100)
                    .unwrap()
                    .compacted
            })
            .unwrap();
        let next = engine
            .prepare_calibrated(bodies[at + 1].clone(), Dialect::Anthropic, 4_000, 1150)
            .unwrap();
        assert!(next.matched, "the entry is keyed by the base threshold");
    }
}

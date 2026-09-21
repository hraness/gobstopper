//! `session-observations-v1` report: join session discovery with
//! compaction telemetry into the AI Charts session-report shape so
//! aicharts (or any consumer of that profile) can ingest gobstopper's
//! measurements for dashboards.
//!
//! # Target schema (from aicharts `lib/usage/sessions.ts`)
//!
//! The canonical envelope is:
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "profile": "session-observations-v1",
//!   "sessions": [ {
//!     "provider": "codex" | "claude_code" | "devin",
//!     "sessionId": "<32 lowercase hex>",
//!     "conversationId": "<32 lowercase hex> | null",
//!     "window": { "startMs": <epoch ms>, "endMs": <epoch ms> },
//!     "source": "history" | "instrumented",
//!     "usage": [ {
//!       "id": "<32 lowercase hex>", "atMs": <epoch ms within window>,
//!       "model": "<allowlisted slug> | null",
//!       "modelBasis": "response" | "request" | "unknown",
//!       "inputTokens": 0, "cacheReadTokens": 0, "cacheWriteTokens": 0,
//!       "outputTokens": 0, "reasoningTokens": null
//!     } ],
//!     "spans": []
//!   } ]
//! }
//! ```
//!
//! aicharts' `parseSessionReport` accepts exactly these keys — no more,
//! no fewer — bounds sessions to 2,000 and records to 50,000, requires
//! `endMs - startMs <= 366 days`, requires history-sourced sessions to
//! carry no spans, and deduplicates on `provider:sessionId` /
//! `provider:usage:id`.
//!
//! # Deliberate deviations from the strict schema
//!
//! - **`gobstopper` extension key.** Each session carries one extra
//!   namespaced object holding compaction stats and raw counters the
//!   strict schema has no slot for. Every aicharts-mandated field is
//!   still emitted with a strictly valid value, so stripping
//!   `sessions[].gobstopper` (e.g. `jq 'del(.sessions[].gobstopper)'`)
//!   yields a report `parseSessionReport` accepts.
//! - **Real session ids, not keyed ids.** aicharts derives 128-bit
//!   keyed (HMAC) pseudonymous ids with a private occurrence key.
//!   gobstopper holds no such key, so `sessionId` carries the *native*
//!   provider id canonicalized to 32 lowercase hex (a UUID with dashes
//!   stripped is already the 128-bit id); non-UUID ids fall back to a
//!   deterministic FNV-1a digest — stable for joins, not a secrecy
//!   boundary. Consumers that need pseudonymity must re-key downstream,
//!   exactly as aicharts does with its occurrence key. The untouched
//!   native id is repeated under `gobstopper.sessionIdNative` whenever
//!   it is safe to print (never a filesystem path).
//! - **One aggregate usage record per session.** gobstopper's cheap
//!   scan yields lifetime counters, not per-request occurrences. The
//!   single record is timestamped at last activity. Token semantics:
//!   gobstopper `lifetime_input_tokens` includes the cached portion on
//!   both providers, so `inputTokens` = lifetime input minus
//!   `cacheReadTokens` (= `lifetime_cached_tokens`) to keep aicharts'
//!   `accountedTokens` sum faithful. Claude cache-*write* tokens cannot
//!   be separated from uncached input in the aggregate and are folded
//!   into `inputTokens`; `outputTokens`/`reasoningTokens` are not
//!   tracked and report 0/null. `model` is always null (`unknown`
//!   basis): gobstopper retains no model labels.
//! - **Window bounds.** `endMs` is last activity (file mtime age at
//!   scan; if underivable, the latest event ts; if neither, 0 and no
//!   usage record is emitted — `atMs` must stay inside the window).
//!   `startMs` extends back over the session's earliest telemetry
//!   event, clamped so the span never exceeds the schema's 366-day
//!   bound.
//!
//! Compaction events join sessions on the native (un-normalized)
//! provider id plus provider — the same key `apply`/`watch` wrote.
//! Events whose session is not among `sessions` are not represented.
//! Sessions are emitted in discovery order (newest first), deduplicated
//! on the emitted `(provider, sessionId)` pair, and capped at the
//! schema's 2,000-session bound; each carries at most one usage record
//! so the 50,000-record bound cannot be hit.
//!
//! Numeric and identifier fields only — never transcript content,
//! prompts, cwd, or filesystem paths.

#![allow(dead_code)]

use gobstopper_adapters::detect::Discovered;
use gobstopper_core::events::CompactionEvent;
use gobstopper_core::Provider;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// aicharts report profile tag.
pub const PROFILE: &str = "session-observations-v1";
/// aicharts envelope version.
pub const SCHEMA_VERSION: u64 = 1;

/// Parser bounds from the aicharts session contract.
const MAX_SESSIONS: usize = 2_000;
const MAX_WINDOW_MS: u64 = 366 * 86_400_000;

/// FNV-1a 64-bit. Deterministic across runs and platforms — used only
/// to mint opaque ids where the provider id is not a 128-bit value.
fn fnv1a64(seed: u64, data: &[u8]) -> u64 {
    let mut h = seed;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Domain-separated 128-bit digest as 32 lowercase hex chars.
/// Guaranteed nonzero so it satisfies the aicharts `id()` predicate.
fn digest128(domain: &str, data: &[u8]) -> String {
    let hi = fnv1a64(fnv1a64(0xcbf2_9ce4_8422_2325, domain.as_bytes()), data);
    let lo = fnv1a64(fnv1a64(0x9e37_79b9_7f4a_7c15, data), domain.as_bytes());
    let s = format!("{hi:016x}{lo:016x}");
    if s.bytes().all(|c| c == b'0') {
        // Astronomically unlikely; keep the all-zero id reserved.
        return format!("{}1", &s[..31]);
    }
    s
}

/// 32 lowercase hex chars, not all zeros — the aicharts `id` shape.
fn as_hex32(s: &str) -> Option<String> {
    (s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()) && s.bytes().any(|b| b != b'0'))
        .then(|| s.to_string())
}

/// Canonicalize a native session id to the schema's 32-hex form.
/// Native ids are UUIDs (both providers); a bare or dashed UUID strips
/// to its own real 128-bit value. Fallback stems like Codex
/// `rollout-<ts>-<uuid>` still contain the UUID as a hex/dash run.
/// Anything else becomes a stable FNV digest — deterministic so event
/// joins stay consistent, documented above as non-keyed.
fn normalize_session_id(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    if let Some(id) = as_hex32(&lowered) {
        return id;
    }
    // Scan maximal hex/dash runs; the first that strips to exactly 32
    // hex digits is the embedded UUID.
    let mut run = String::new();
    for c in lowered.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_hexdigit() || c == '-' {
            run.push(c);
            continue;
        }
        let stripped: String = run.chars().filter(|c| *c != '-').collect();
        if let Some(id) = as_hex32(&stripped) {
            return id;
        }
        run.clear();
    }
    digest128("gobstopper/session-id", raw.as_bytes())
}

/// An id that is (or could be mistaken for) a filesystem path must
/// never be echoed into a report. `find_session` can put the query
/// string itself into `session_id` when meta is missing, and the query
/// may be a path.
fn looks_like_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\')
}

/// Joined telemetry for one session. `applied` stats count only
/// `outcome == "applied"`; `min_ts`/`max_ts` span every event outcome
/// because any recorded event is still an observation of the session.
#[derive(Default)]
struct Agg {
    min_ts: u64,
    max_ts: u64,
    applied: u64,
    native_hook_applied: u64,
    est_reclaimed: u64,
    last_applied_ts: u64,
    last_strategy: Option<String>,
}

fn aggregate<'a>(events: &'a [CompactionEvent]) -> HashMap<(Provider, &'a str), Agg> {
    let mut map: HashMap<(Provider, &'a str), Agg> = HashMap::new();
    for e in events {
        let agg = map.entry((e.provider, e.session_id.as_str())).or_default();
        if agg.min_ts == 0 || e.ts < agg.min_ts {
            agg.min_ts = e.ts;
        }
        agg.max_ts = agg.max_ts.max(e.ts);
        if e.outcome == "applied" {
            agg.applied += 1;
            if e.strategy == "native" && e.action == "provider_compact" {
                agg.native_hook_applied += 1;
            }
            agg.est_reclaimed = agg.est_reclaimed.saturating_add(e.est_reclaimed_tokens);
            if e.ts >= agg.last_applied_ts {
                agg.last_applied_ts = e.ts;
                agg.last_strategy = Some(e.strategy.clone());
            }
        }
    }
    map
}

/// Build a session-observations-v1 report from detected sessions +
/// the gobstopper events log. Numeric and identifier fields only —
/// never transcript content, prompts, or freeform strings.
pub fn build_report(sessions: &[Discovered], events: &[CompactionEvent]) -> Value {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let aggs = aggregate(events);
    let mut seen: HashSet<(Provider, String)> = HashSet::new();
    let mut out = Vec::new();

    for d in sessions {
        let handle = &d.handle;
        let session_id = normalize_session_id(&handle.session_id);
        // The strict parser rejects duplicate provider:sessionId keys;
        // keep the first (discovery order is newest-first).
        if !seen.insert((handle.provider, session_id.clone())) {
            continue;
        }
        if out.len() >= MAX_SESSIONS {
            break;
        }
        let agg = aggs.get(&(handle.provider, handle.session_id.as_str()));

        // Last activity: file mtime age at scan time. `u64::MAX` is the
        // detect sentinel for "mtime unreadable" — not a real age.
        let last_activity_ms = (handle.age_secs != u64::MAX)
            .then(|| now_ms.saturating_sub(handle.age_secs.saturating_mul(1_000)));
        let event_min_ms = agg.filter(|a| a.min_ts > 0).map(|a| a.min_ts * 1_000);
        let event_max_ms = agg.filter(|a| a.max_ts > 0).map(|a| a.max_ts * 1_000);
        let end_ms = last_activity_ms.or(event_max_ms).unwrap_or(0);
        let mut start_ms = last_activity_ms.or(event_min_ms).unwrap_or(0);
        if let Some(min) = event_min_ms {
            start_ms = start_ms.min(min);
        }
        // Schema bounds: endMs >= startMs, and the span <= 366 days.
        // The derivation above already orders them; the min() keeps the
        // invariant explicit if the rules ever change.
        start_ms = start_ms.min(end_ms);
        if end_ms.saturating_sub(start_ms) > MAX_WINDOW_MS {
            start_ms = end_ms - MAX_WINDOW_MS;
        }

        // One aggregate usage record, timestamped at last activity.
        // Only emitted when a real timestamp exists: `atMs` must lie
        // inside the window, and a fabricated epoch would lie.
        let usage = if end_ms > 0 {
            vec![json!({
                "id": digest128("gobstopper/usage", session_id.as_bytes()),
                "atMs": end_ms,
                "model": Value::Null,
                "modelBasis": "unknown",
                "inputTokens": d
                    .usage
                    .lifetime_input_tokens
                    .saturating_sub(d.usage.lifetime_cached_tokens),
                "cacheReadTokens": d.usage.lifetime_cached_tokens,
                "cacheWriteTokens": 0,
                "outputTokens": 0,
                "reasoningTokens": Value::Null,
            })]
        } else {
            Vec::new()
        };

        let applied = agg.map(|a| a.applied).unwrap_or(0);
        let est_reclaimed = agg.map(|a| a.est_reclaimed).unwrap_or(0);
        let last_applied_ms = agg
            .filter(|a| a.last_applied_ts > 0)
            .map(|a| a.last_applied_ts * 1_000);
        let last_strategy = agg.and_then(|a| a.last_strategy.clone());
        let native = if looks_like_path(&handle.session_id) {
            Value::Null
        } else {
            Value::from(handle.session_id.as_str())
        };

        out.push(json!({
            "provider": handle.provider.as_str(),
            "sessionId": session_id,
            "conversationId": Value::Null,
            "window": { "startMs": start_ms, "endMs": end_ms },
            "source": "history",
            "usage": usage,
            "spans": [],
            "gobstopper": {
                "sessionIdNative": native,
                "contextTokens": d.usage.context_tokens,
                "lifetimeInputTokens": d.usage.lifetime_input_tokens,
                "lifetimeCachedTokens": d.usage.lifetime_cached_tokens,
                "modelContextWindow": d.usage.model_context_window,
                "lastActivityMs": last_activity_ms,
                "compactions": {
                    "applied": applied,
                    "nativeHookApplied": agg.map(|a| a.native_hook_applied).unwrap_or(0),
                    "estReclaimedTokens": est_reclaimed,
                    "lastAppliedMs": last_applied_ms,
                    "lastStrategy": last_strategy,
                },
            },
        }));
    }

    json!({
        "schemaVersion": SCHEMA_VERSION,
        "profile": PROFILE,
        "sessions": out,
    })
}

/// `events --cohort` readout: per-provider, per-rollout-arm aggregation
/// of compaction telemetry so the A/B effect is readable. The cohort is
/// recomputed from the session id — the same deterministic bucket
/// `hooks` and `watch` gate on — rather than trusting the strategy tag,
/// so untagged `auto`/`watch` events still attribute to the right arm.
pub fn cohort_summary(cfg: &crate::config::Config, events: &[CompactionEvent]) -> Value {
    #[derive(Default)]
    struct Acc {
        sessions: HashSet<String>,
        events: u64,
        /// prompt-policy decision shown to the user (over trigger).
        advisories_shown: u64,
        /// prompt-policy decision suppressed by the rollout gate while
        /// the session was over trigger — the withheld numerator.
        advisories_suppressed: u64,
        /// prompt-policy decision with no advisory emitted: under
        /// trigger, unresolved context, or a repeat suppressed by the
        /// re-show throttle.
        silent_decisions: u64,
        /// `watch-apply:control` — an in-place apply withheld by cohort.
        watch_suppressed: u64,
        applies: u64,
        reclaimed_tokens: u64,
        /// error_code == unresolved_context; not a policy decision.
        unresolved_context: u64,
        first_ts: u64,
        last_ts: u64,
    }
    impl Acc {
        fn observe(&mut self, e: &CompactionEvent, cohort: &'static str) {
            self.sessions.insert(e.session_id.clone());
            self.events += 1;
            if self.first_ts == 0 || e.ts < self.first_ts {
                self.first_ts = e.ts;
            }
            self.last_ts = self.last_ts.max(e.ts);
            if e.error_code.as_deref() == Some("unresolved_context") {
                self.unresolved_context += 1;
            }
            if e.strategy.starts_with("prompt-policy") {
                match e.outcome.as_str() {
                    "planned" => self.advisories_shown += 1,
                    // Unresolved context is not a decision at all —
                    // counted above, excluded from both arms here.
                    "skipped" if e.error_code.as_deref() == Some("unresolved_context") => {}
                    "skipped" => {
                        if cohort == "control" && e.context_tokens_before >= e.trigger_tokens {
                            self.advisories_suppressed += 1;
                        } else {
                            self.silent_decisions += 1;
                        }
                    }
                    _ => {}
                }
            }
            if e.strategy == "watch-apply:control" {
                self.watch_suppressed += 1;
            }
            if e.outcome == "applied" {
                self.applies += 1;
                self.reclaimed_tokens += e.est_reclaimed_tokens;
            }
        }
        fn json(&self) -> Value {
            json!({
                "sessions": self.sessions.len(),
                "events": self.events,
                "advisories_shown": self.advisories_shown,
                "advisories_suppressed": self.advisories_suppressed,
                "silent_decisions": self.silent_decisions,
                "watch_suppressed": self.watch_suppressed,
                "applies": self.applies,
                "reclaimed_tokens": self.reclaimed_tokens,
                "unresolved_context": self.unresolved_context,
                "first_ts": self.first_ts,
                "last_ts": self.last_ts,
            })
        }
    }
    #[derive(Default)]
    struct ProviderAcc {
        treatment: Acc,
        control: Acc,
        ungated: Acc,
    }
    let mut providers: HashMap<&'static str, ProviderAcc> = HashMap::new();
    for e in events {
        let pa = providers.entry(e.provider.as_str()).or_default();
        let cohort = match crate::hooks::rollout_cohort(cfg, e.provider.as_str(), &e.session_id) {
            Some(true) => "treatment",
            Some(false) => "control",
            None => "ungated",
        };
        match cohort {
            "treatment" => pa.treatment.observe(e, cohort),
            "control" => pa.control.observe(e, cohort),
            _ => pa.ungated.observe(e, cohort),
        }
    }
    let mut out = serde_json::Map::new();
    for (pname, pa) in providers {
        out.insert(
            pname.to_string(),
            json!({
                "rollout_pct": cfg.rollout.get(pname).copied(),
                "cohorts": {
                    "treatment": pa.treatment.json(),
                    "control": pa.control.json(),
                    "ungated": pa.ungated.json(),
                },
            }),
        );
    }
    json!({ "schema": "gobstopper-cohort-readout-v1", "providers": out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gobstopper_core::{SessionHandle, UsageSample};
    use std::path::PathBuf;

    const UUID_A: &str = "3f6b1a2c-9d4e-4f5a-8b6c-7d8e9f0a1b2c";
    const UUID_B: &str = "aa11bb22-cc33-4d44-8e55-ff6677889900";

    fn discovered(provider: Provider, session_id: &str, age_secs: u64) -> Discovered {
        Discovered {
            handle: SessionHandle {
                provider,
                session_id: session_id.to_string(),
                path: PathBuf::from("/private/tmp/secret/rollout-x.jsonl"),
                cwd: Some(PathBuf::from("/private/tmp/secret/worktree")),
                age_secs,
            },
            usage: UsageSample {
                context_tokens: 42_000,
                lifetime_input_tokens: 100_000,
                lifetime_cached_tokens: 30_000,
                model_context_window: Some(1_000_000),
            },
        }
    }

    fn event(
        provider: Provider,
        session_id: &str,
        ts: u64,
        outcome: &str,
        strategy: &str,
        reclaimed: u64,
    ) -> CompactionEvent {
        CompactionEvent {
            schema: CompactionEvent::SCHEMA.to_string(),
            ts,
            provider,
            session_id: session_id.to_string(),
            strategy: strategy.to_string(),
            action: "transcript_compact".to_string(),
            outcome: outcome.to_string(),
            trigger_tokens: 250_000,
            context_tokens_before: 260_000,
            context_tokens_after: 260_000 - reclaimed,
            est_reclaimed_tokens: reclaimed,
            items_covered: 10,
            duration_ms: 5,
            error_code: None,
        }
    }

    #[test]
    fn cohort_summary_attributes_events_to_deterministic_arms() {
        let mut cfg = crate::config::Config::default();
        cfg.rollout.insert("claude_code".to_string(), 100);
        let events = vec![
            event(Provider::ClaudeCode, UUID_A, 100, "applied", "auto", 5_000),
            event(
                Provider::ClaudeCode,
                UUID_A,
                110,
                "planned",
                "prompt-policy:treatment",
                0,
            ),
        ];
        let summary = cohort_summary(&cfg, &events);
        let t = &summary["providers"]["claude_code"]["cohorts"]["treatment"];
        assert_eq!(t["applies"], 1);
        assert_eq!(t["reclaimed_tokens"], 5_000);
        assert_eq!(t["advisories_shown"], 1);
        assert_eq!(t["sessions"], 1);
        assert_eq!(summary["providers"]["claude_code"]["rollout_pct"], 100);
        assert_eq!(
            summary["providers"]["claude_code"]["cohorts"]["control"]["events"],
            0
        );

        // 0% rollout: everyone is control; a prompt-policy skip recorded
        // while over trigger is a genuinely suppressed advisory.
        cfg.rollout.insert("claude_code".to_string(), 0);
        let mut suppressed = event(
            Provider::ClaudeCode,
            UUID_A,
            120,
            "skipped",
            "prompt-policy:control",
            0,
        );
        suppressed.context_tokens_before = 300_000; // >= trigger 250_000
        suppressed.context_tokens_after = 300_000;
        let summary = cohort_summary(&cfg, &[suppressed]);
        let c = &summary["providers"]["claude_code"]["cohorts"]["control"];
        assert_eq!(c["advisories_suppressed"], 1);
        assert_eq!(c["advisories_shown"], 0);

        // Unresolved-context skips are excluded from decision counters.
        let mut unresolved = event(
            Provider::ClaudeCode,
            UUID_A,
            130,
            "skipped",
            "prompt-policy:control",
            0,
        );
        unresolved.context_tokens_before = 0;
        unresolved.context_tokens_after = 0;
        unresolved.error_code = Some("unresolved_context".to_string());
        let summary = cohort_summary(&cfg, &[unresolved]);
        let c = &summary["providers"]["claude_code"]["cohorts"]["control"];
        assert_eq!(c["unresolved_context"], 1);
        assert_eq!(c["advisories_suppressed"], 0);
        assert_eq!(c["silent_decisions"], 0);

        // Providers with no rollout entry aggregate under "ungated".
        let events = vec![event(Provider::Codex, UUID_A, 1, "applied", "auto", 7)];
        let summary = cohort_summary(&cfg, &events);
        let u = &summary["providers"]["codex"]["cohorts"]["ungated"];
        assert_eq!(u["applies"], 1);
        assert_eq!(u["reclaimed_tokens"], 7);
    }

    #[test]
    fn envelope_and_session_fields_match_aicharts_shape() {
        // Event times relative to now keep the window under the 366-day
        // clamp no matter when the test runs.
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let sessions = vec![
            discovered(Provider::Codex, UUID_A, 3_600),
            discovered(Provider::ClaudeCode, UUID_B, 86_400),
        ];
        let events = vec![
            event(
                Provider::Codex,
                UUID_A,
                now_s - 7_200,
                "applied",
                "elide",
                50_000,
            ),
            event(
                Provider::Codex,
                UUID_A,
                now_s - 3_700,
                "failed",
                "sawtooth",
                0,
            ),
            event(
                Provider::Codex,
                UUID_A,
                now_s - 1_800,
                "applied",
                "sawtooth",
                75_000,
            ),
            event(
                Provider::ClaudeCode,
                "unrelated-session",
                9,
                "applied",
                "auto",
                1,
            ),
        ];
        let report = build_report(&sessions, &events);

        assert_eq!(report["schemaVersion"], 1);
        assert_eq!(report["profile"], "session-observations-v1");
        let list = report["sessions"].as_array().unwrap();
        assert_eq!(list.len(), 2);

        let s = &list[0];
        assert_eq!(s["provider"], "codex");
        // Native UUID canonicalized: same 128-bit id, dashes stripped.
        assert_eq!(s["sessionId"], "3f6b1a2c9d4e4f5a8b6c7d8e9f0a1b2c");
        assert!(s["conversationId"].is_null());
        assert_eq!(s["source"], "history");
        assert_eq!(s["spans"].as_array().unwrap().len(), 0);
        assert_eq!(s["window"]["endMs"], s["gobstopper"]["lastActivityMs"]);
        // Earliest telemetry event extends the window start.
        assert_eq!(s["window"]["startMs"], (now_s - 7_200) * 1_000);
        assert!(s["window"]["startMs"].as_u64() <= s["window"]["endMs"].as_u64());

        let u = &s["usage"].as_array().unwrap()[0].clone();
        let id = u["id"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(id.bytes().any(|b| b != b'0'));
        assert_eq!(u["atMs"], s["window"]["endMs"]);
        assert!(u["model"].is_null());
        assert_eq!(u["modelBasis"], "unknown");
        // Lifetime input 100k splits into 70k uncached + 30k cache reads.
        assert_eq!(u["inputTokens"], 70_000);
        assert_eq!(u["cacheReadTokens"], 30_000);
        assert_eq!(u["cacheWriteTokens"], 0);
        assert_eq!(u["outputTokens"], 0);
        assert!(u["reasoningTokens"].is_null());

        let g = &s["gobstopper"];
        assert_eq!(g["sessionIdNative"], UUID_A);
        assert_eq!(g["contextTokens"], 42_000);
        assert_eq!(g["lifetimeInputTokens"], 100_000);
        assert_eq!(g["lifetimeCachedTokens"], 30_000);
        assert_eq!(g["modelContextWindow"], 1_000_000);
        // Two applied of three events; failed one is counted in neither
        // the applied count nor the reclaimed sum, but still bounds the
        // window. Last applied wins strategy + ts.
        assert_eq!(g["compactions"]["applied"], 2);
        assert_eq!(g["compactions"]["estReclaimedTokens"], 125_000);
        assert_eq!(g["compactions"]["lastAppliedMs"], (now_s - 1_800) * 1_000);
        assert_eq!(g["compactions"]["lastStrategy"], "sawtooth");

        // Second session: no matching events (the unrelated one missed).
        let g2 = &list[1]["gobstopper"];
        assert_eq!(g2["compactions"]["applied"], 0);
        assert_eq!(g2["compactions"]["estReclaimedTokens"], 0);
        assert!(g2["compactions"]["lastAppliedMs"].is_null());
        assert!(g2["compactions"]["lastStrategy"].is_null());
        // No events: window is the single last-activity instant.
        assert_eq!(list[1]["window"]["startMs"], list[1]["window"]["endMs"]);
    }

    #[test]
    fn native_hook_count_excludes_other_applied_or_unfinished_events() {
        let sessions = vec![discovered(Provider::Codex, UUID_A, 0)];
        let mut native = event(Provider::Codex, UUID_A, 1, "applied", "native", 0);
        native.action = "provider_compact".into();
        let mut planned = native.clone();
        planned.outcome = "planned".into();
        let mut skipped = native.clone();
        skipped.outcome = "skipped".into();
        let other = event(Provider::Codex, UUID_A, 1, "applied", "elide", 100);
        let report = build_report(&sessions, &[native, planned, skipped, other]);
        let counts = &report["sessions"][0]["gobstopper"]["compactions"];
        assert_eq!(counts["applied"], 2);
        assert_eq!(counts["nativeHookApplied"], 1);
        assert_eq!(counts["estReclaimedTokens"], 100);
    }

    #[test]
    fn non_uuid_ids_get_stable_hex_digests() {
        let sessions = vec![discovered(Provider::Codex, "rollout-no-uuid-here", 0)];
        let a = build_report(&sessions, &[]);
        let b = build_report(&sessions, &[]);
        let id = a["sessions"][0]["sessionId"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(id, b["sessions"][0]["sessionId"].as_str().unwrap());
        // The unusable native id still rides along for event joins.
        assert_eq!(
            a["sessions"][0]["gobstopper"]["sessionIdNative"],
            "rollout-no-uuid-here"
        );
    }

    #[test]
    fn duplicate_ids_emit_once() {
        let sessions = vec![
            discovered(Provider::Codex, UUID_A, 10),
            discovered(Provider::Codex, UUID_A, 20),
        ];
        let report = build_report(&sessions, &[]);
        assert_eq!(report["sessions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn no_absolute_paths_or_freeform_strings_leak() {
        // A path-like session id is the worst case: it must not be
        // echoed, and its digest keeps the sessionId shape.
        let mut d = discovered(Provider::ClaudeCode, "/private/tmp/secret/evil.jsonl", 5);
        d.handle.cwd = Some(PathBuf::from("/private/tmp/secret/worktree"));
        let report = build_report(&[d], &[]);
        let text = serde_json::to_string(&report).unwrap();
        assert!(!text.contains("/private"));
        assert!(!text.contains("evil.jsonl"));
        assert!(!text.contains("worktree"));
        assert!(!text.contains("path"));
        let s = &report["sessions"][0];
        let id = s["sessionId"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(s["gobstopper"]["sessionIdNative"].is_null());
    }

    #[test]
    fn underivable_timestamps_yield_zero_window_and_no_usage() {
        let sessions = vec![discovered(Provider::Codex, UUID_A, u64::MAX)];
        let report = build_report(&sessions, &[]);
        let s = &report["sessions"][0];
        assert_eq!(s["window"]["startMs"], 0);
        assert_eq!(s["window"]["endMs"], 0);
        assert_eq!(s["usage"].as_array().unwrap().len(), 0);
        assert!(s["gobstopper"]["lastActivityMs"].is_null());
        // Tokens still reach the extension block.
        assert_eq!(s["gobstopper"]["contextTokens"], 42_000);
    }

    #[test]
    fn event_only_timestamps_bound_the_window() {
        // mtime unreadable but telemetry saw the session: window comes
        // from event seconds, usage lands on the latest event.
        let sessions = vec![discovered(Provider::Codex, UUID_A, u64::MAX)];
        let events = vec![
            event(
                Provider::Codex,
                UUID_A,
                1_700_000_000,
                "applied",
                "elide",
                10,
            ),
            event(Provider::Codex, UUID_A, 1_700_050_000, "skipped", "auto", 0),
        ];
        let report = build_report(&sessions, &events);
        let s = &report["sessions"][0];
        assert_eq!(s["window"]["startMs"], 1_700_000_000_000u64);
        assert_eq!(s["window"]["endMs"], 1_700_050_000_000u64);
        assert_eq!(s["usage"][0]["atMs"], 1_700_050_000_000u64);
        assert_eq!(s["gobstopper"]["compactions"]["applied"], 1);
    }

    #[test]
    fn sessions_beyond_a_year_of_telemetry_are_clamped() {
        let sessions = vec![discovered(Provider::Codex, UUID_A, 0)];
        let events = vec![event(
            Provider::Codex,
            UUID_A,
            1_600_000_000, // ~Sept 2020, far before scan time
            "applied",
            "elide",
            1,
        )];
        let report = build_report(&sessions, &events);
        let s = &report["sessions"][0];
        let start = s["window"]["startMs"].as_u64().unwrap();
        let end = s["window"]["endMs"].as_u64().unwrap();
        assert!(end >= start);
        assert!(end - start <= MAX_WINDOW_MS);
    }
}

//! Source-bound session observations. Counts carry presence and source scope
//! in the gobstopper extension; partial discovery tails are never lifetime
//! totals. Positive provider context reports from retained same-source snapshots
//! can establish a recorded reduction, not billed savings or causal task benefit.
//! Exported session ids hash provider/store/session so equal native ids in
//! different stores cannot merge. The native id remains available when bounded.
//! The compatibility usage slots for output/cache-write are zero placeholders;
//! unmeasuredUsageFields names them explicitly. No prices or billing are inferred.

#![allow(dead_code)]

use gobstopper_adapters::detect::Discovered;
use gobstopper_core::events::{qualified_reductions, Cohort, CompactionEvent};
use gobstopper_core::model::LifetimeScope;
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
    qualified_reductions: u64,
    unqualified_applied: u64,
    last_applied_ts: u64,
    last_strategy: Option<String>,
}

fn aggregate<'a>(
    events: &'a [CompactionEvent],
    evidence_available: bool,
) -> HashMap<(Provider, &'a str, &'a str), Agg> {
    let mut map: HashMap<(Provider, &'a str, &'a str), Agg> = HashMap::new();
    let evidence = if evidence_available {
        qualified_reductions(events)
    } else {
        Default::default()
    };
    let qualified: HashMap<_, _> = evidence
        .reductions
        .iter()
        .map(|row| (row.first_index, row.reduction_tokens))
        .collect();
    for (index, e) in events.iter().enumerate() {
        let Some(identity) = e.source_identity_sha256.as_deref() else {
            continue;
        };
        let agg = map
            .entry((e.provider, e.session_id.as_str(), identity))
            .or_default();
        if agg.min_ts == 0 || e.ts < agg.min_ts {
            agg.min_ts = e.ts;
        }
        agg.max_ts = agg.max_ts.max(e.ts);
        if e.outcome == "applied" {
            agg.applied = agg.applied.saturating_add(1);
            if let Some(reduction) = qualified.get(&index) {
                agg.qualified_reductions = agg.qualified_reductions.saturating_add(1);
                agg.est_reclaimed = agg.est_reclaimed.saturating_add(*reduction);
            } else {
                agg.unqualified_applied = agg.unqualified_applied.saturating_add(1);
            }
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
pub fn build_report(
    sessions: &[Discovered],
    events: &[CompactionEvent],
    evidence_available: bool,
) -> Value {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let aggs = aggregate(events, evidence_available);
    let evidence_unavailable_reason = (!evidence_available).then_some("event_history_incomplete");
    let mut seen: HashSet<(Provider, String)> = HashSet::new();
    let mut out = Vec::new();
    let mut omitted = 0usize;

    for d in sessions {
        let handle = &d.handle;
        let canonical_identity = gobstopper_adapters::detect::source_identity(handle).ok();
        let source_identity = canonical_identity.clone().unwrap_or_else(|| {
            gobstopper_adapters::copy::sha256(
                &serde_json::to_vec(&(
                    "unavailable-source-path-v1",
                    handle.provider,
                    &handle.session_id,
                    handle.path.as_os_str().as_encoded_bytes(),
                ))
                .expect("serializable source identity"),
            )
        });
        // Store identity participates in the exported id so foreign stores with
        // the same native id remain separate even across successive reports.
        let session_id = source_identity[..32].to_string();
        // The strict parser rejects duplicate provider:sessionId keys;
        // keep the first (discovery order is newest-first).
        if !seen.insert((handle.provider, session_id.clone())) {
            continue;
        }
        if out.len() >= MAX_SESSIONS {
            omitted += 1;
            continue;
        }
        let agg = canonical_identity.as_ref().and_then(|identity| {
            aggs.get(&(
                handle.provider,
                handle.session_id.as_str(),
                identity.as_str(),
            ))
        });

        // Last activity: file mtime age at scan time. `u64::MAX` is the
        // detect sentinel for "mtime unreadable" — not a real age.
        let last_activity_ms = (handle.age_secs != u64::MAX)
            .then(|| now_ms.saturating_sub(handle.age_secs.saturating_mul(1_000)));
        let event_min_ms = agg
            .filter(|a| a.min_ts > 0)
            .map(|a| a.min_ts.saturating_mul(1_000).min(now_ms));
        let event_max_ms = agg
            .filter(|a| a.max_ts > 0)
            .map(|a| a.max_ts.saturating_mul(1_000).min(now_ms));
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
        let usage = if end_ms > 0 && d.usage.lifetime_scope == LifetimeScope::Full {
            vec![json!({
                "id": digest128("gobstopper/usage", session_id.as_bytes()),
                "atMs": end_ms,
                "model": Value::Null,
                "modelBasis": "unknown",
                "inputTokens": d
                    .usage
                    .lifetime_input_tokens
                    .saturating_sub(d.usage.lifetime_cached_tokens),
                "cacheReadTokens": d.usage.lifetime_cached_tokens.min(d.usage.lifetime_input_tokens),
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
            .map(|a| a.last_applied_ts.saturating_mul(1_000).min(now_ms));
        let last_strategy = agg.and_then(|a| a.last_strategy.clone());
        let native = if looks_like_path(&handle.session_id)
            || handle.session_id.len() > 256
            || !handle
                .session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            Value::Null
        } else {
            Value::from(handle.session_id.as_str())
        };
        // Whether a closed session can take the provider-native compaction
        // path under `auto_compact_closed`. Codex multi-agent v2 sub-agent
        // threads reject `thread/resume` outright — only the parent can
        // compact them — so they are reported as unavailable rather than
        // discovered as failures at apply time.
        let closed_session_compact = match handle.provider {
            Provider::Codex if gobstopper_adapters::codex::is_subagent_thread(&handle.path) => {
                "unavailable:sub-agent"
            }
            _ => "unqualified",
        };

        let compactions = json!({
            "evidenceAvailable": evidence_available,
            "evidenceUnavailableReason": evidence_unavailable_reason,
            "applied": applied,
            "nativeHookApplied": agg.map(|a| a.native_hook_applied).unwrap_or(0),
            "estReclaimedTokens": est_reclaimed,
            "recordedContextReductionTokens": agg.filter(|a| a.qualified_reductions > 0).map(|a| a.est_reclaimed),
            "qualifiedReductionEvents": agg.map(|a| a.qualified_reductions).unwrap_or(0),
            "unqualifiedAppliedEvents": agg.map(|a| a.unqualified_applied).unwrap_or(0),
            "reductionBasis": "positive_reported_context_same_source_retained_bytes_not_billing",
            "lastAppliedMs": last_applied_ms,
            "lastStrategy": last_strategy,
        });
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
                "sourceIdentitySha256": canonical_identity,
                "sessionIdBasis": if canonical_identity.is_some() { "canonical_provider_store_session_sha256_prefix" } else { "unavailable_source_path_hash_fallback" },
                "sampling": "bounded_discovered_sessions",
                "legacyEventsQualified": false,
                "contextTokens": d.usage.context_tokens,
                "contextState": d.usage.context_state,
                "reportedContextTokens": d.usage.reported_context(),
                "contextComponents": d.usage.context_components,
                "contextReason": d.usage.context_reason,
                "measuredComponentSubtotal": d.usage.measured_component_subtotal(),
                "componentSubtotalBasis": "known_numeric_components_not_complete_occupancy",
                "lifetimeScope": d.usage.lifetime_scope,
                "tokenUnits": "tokens",
                "usageBasis": "recorded_provider_accounting_not_billing",
                "unmeasuredUsageFields": ["cacheWriteTokens", "outputTokens", "reasoningTokens", "chargedTokens"],
                "lifetimeInputTokens": d.usage.lifetime_input_tokens,
                "lifetimeCachedTokens": d.usage.lifetime_cached_tokens,
                "modelContextWindow": d.usage.model_context_window,
                "lastActivityMs": last_activity_ms,
                "closedSessionCompact": closed_session_compact,
                "qualificationStatus": "unqualified",
                "nativeActivation": false,
                "compactions": compactions,
            },
        }));
    }

    json!({
        "schemaVersion": SCHEMA_VERSION,
        "profile": PROFILE,
        "sessions": out,
        "gobstopper": {
            "coverage": {"discovered": sessions.len(), "exported": out.len(),
                "omitted": omitted, "limit": MAX_SESSIONS, "truncated": omitted > 0},
            "evidence": {"available": evidence_available,
                "unavailableReason": evidence_unavailable_reason,
                "conflictingPairs": evidence_available.then(|| qualified_reductions(events).conflicting_pairs)},
        },
    })
}

/// `events --cohort` readout: per-provider, per-rollout-arm aggregation
/// of compaction telemetry. Arm identity is the recorded decision-time value;
/// old events cannot be relabeled using a changed current configuration.
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
        fn observe(&mut self, e: &CompactionEvent, cohort: &'static str, reduction: Option<u64>) {
            self.sessions.insert(
                e.source_identity_sha256
                    .clone()
                    .unwrap_or_else(|| format!("unqualified:{}", e.session_id)),
            );
            self.events = self.events.saturating_add(1);
            if self.first_ts == 0 || e.ts < self.first_ts {
                self.first_ts = e.ts;
            }
            self.last_ts = self.last_ts.max(e.ts);
            if e.error_code.as_deref() == Some("unresolved_context") {
                self.unresolved_context = self.unresolved_context.saturating_add(1);
            }
            if e.strategy.starts_with("prompt-policy") {
                match e.outcome.as_str() {
                    "planned" => self.advisories_shown = self.advisories_shown.saturating_add(1),
                    // Unresolved context is not a decision at all —
                    // counted above, excluded from both arms here.
                    "skipped" if e.error_code.as_deref() == Some("unresolved_context") => {}
                    "skipped" => {
                        if cohort == "control" && e.context_tokens_before >= e.trigger_tokens {
                            self.advisories_suppressed =
                                self.advisories_suppressed.saturating_add(1);
                        } else {
                            self.silent_decisions = self.silent_decisions.saturating_add(1);
                        }
                    }
                    _ => {}
                }
            }
            if matches!(
                e.strategy.as_str(),
                "watch-apply:control" | "watch-native:control"
            ) {
                self.watch_suppressed = self.watch_suppressed.saturating_add(1);
            }
            if e.outcome == "applied" {
                self.applies = self.applies.saturating_add(1);
                if let Some(reduction) = reduction {
                    self.reclaimed_tokens = self.reclaimed_tokens.saturating_add(reduction);
                }
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
        unknown: Acc,
    }
    let mut providers: HashMap<&'static str, ProviderAcc> = HashMap::new();
    let evidence = qualified_reductions(events);
    let qualified: HashMap<_, _> = evidence
        .reductions
        .iter()
        .map(|row| (row.first_index, row.reduction_tokens))
        .collect();
    for (index, e) in events.iter().enumerate() {
        let pa = providers.entry(e.provider.as_str()).or_default();
        let cohort = match e.decision_cohort {
            Some(Cohort::Treatment) => "treatment",
            Some(Cohort::Control) => "control",
            Some(Cohort::Ungated) => "ungated",
            None => "unknown",
        };
        let reduction = qualified.get(&index).copied();
        match cohort {
            "treatment" => pa.treatment.observe(e, cohort, reduction),
            "control" => pa.control.observe(e, cohort, reduction),
            "ungated" => pa.ungated.observe(e, cohort, reduction),
            _ => pa.unknown.observe(e, cohort, reduction),
        }
    }
    let mut out = serde_json::Map::new();
    for (pname, pa) in providers {
        out.insert(
            pname.to_string(),
            json!({
                "rollout_pct": cfg.rollout.get(pname).copied(),
                "rollout_pct_basis": "current_configuration_not_historical_assignment",
                "cohorts": {
                    "treatment": pa.treatment.json(),
                    "control": pa.control.json(),
                    "ungated": pa.ungated.json(),
                    "unknown": pa.unknown.json(),
                },
            }),
        );
    }
    json!({ "schema": "gobstopper-cohort-readout-v1", "providers": out,
        "assignment_basis": "recorded_decision_time_unknown_for_legacy",
        "conflicting_reduction_pairs": evidence.conflicting_pairs,
        "interpretation": "descriptive_selected_cohorts_not_causal_effect", "reclaimed_tokens_basis": "qualified_recorded_context_reduction_not_billing" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gobstopper_core::{SessionHandle, UsageSample};
    use std::fs;
    use std::path::PathBuf;

    const UUID_A: &str = "3f6b1a2c-9d4e-4f5a-8b6c-7d8e9f0a1b2c";
    const UUID_B: &str = "aa11bb22-cc33-4d44-8e55-ff6677889900";

    fn discovered(provider: Provider, session_id: &str, age_secs: u64) -> Discovered {
        static SOURCE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let path = SOURCE
            .get_or_init(|| {
                let path = std::env::temp_dir()
                    .join(format!("gobstopper-report-unit-{}", std::process::id()));
                fs::write(&path, b"synthetic report identity fixture").unwrap();
                path.canonicalize().unwrap()
            })
            .clone();
        Discovered {
            handle: SessionHandle {
                provider,
                session_id: session_id.to_string(),
                path,
                cwd: Some(PathBuf::from("/private/tmp/secret/worktree")),
                age_secs,
            },
            usage: UsageSample {
                context_tokens: 42_000,
                lifetime_input_tokens: 100_000,
                lifetime_cached_tokens: 30_000,
                model_context_window: Some(1_000_000),
                context_state: gobstopper_core::model::ContextState::Reported,
                lifetime_scope: LifetimeScope::Full,
                ..UsageSample::default()
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
        use gobstopper_core::events::TokenObservation;
        use gobstopper_core::model::ContextState;
        let mut event = CompactionEvent::new(
            provider,
            session_id,
            strategy,
            "transcript_compact",
            outcome,
            250_000,
            260_000,
            260_000u64.saturating_sub(reclaimed),
            10,
            5,
            None,
        );
        event.ts = ts;
        event.decision_cohort = Some(Cohort::Ungated);
        let source = discovered(provider, session_id, 0).handle;
        let identity = gobstopper_adapters::copy::sha256(
            &serde_json::to_vec(&(provider, session_id, &source.path)).unwrap(),
        );
        event.source_identity_sha256 = Some(identity.clone());
        let before_manifest = gobstopper_adapters::copy::sha256(
            &serde_json::to_vec(&(ts, reclaimed, "before")).unwrap(),
        );
        let after_manifest = gobstopper_adapters::copy::sha256(
            &serde_json::to_vec(&(ts, reclaimed, "after")).unwrap(),
        );
        event.snapshot_before_sha256 = Some(before_manifest.clone());
        event.snapshot_after_sha256 = Some(after_manifest.clone());
        let observation = |manifest: &str, context| TokenObservation {
            source_sha256: gobstopper_adapters::copy::sha256(manifest.as_bytes()),
            source_identity_sha256: identity.clone(),
            snapshot_manifest_sha256: Some(manifest.into()),
            context_state: ContextState::Reported,
            context_tokens: Some(context),
            estimated_context_tokens: 0,
            lifetime_scope: LifetimeScope::Absent,
            lifetime_input_tokens: None,
            lifetime_cached_tokens: None,
        };
        event.before_observation = Some(observation(&before_manifest, 260_000));
        event.after_observation = Some(observation(
            &after_manifest,
            260_000u64.saturating_sub(reclaimed),
        ));
        event
    }

    #[test]
    fn source_identity_separates_foreign_same_id_and_partial_totals() {
        let first = discovered(Provider::Codex, UUID_A, 0);
        let mut foreign = discovered(Provider::Codex, UUID_A, 0);
        foreign.handle.path = "/foreign/source.jsonl".into();
        foreign.usage.lifetime_scope = LifetimeScope::Partial;
        let report = build_report(
            &[first, foreign],
            &[event(
                Provider::Codex,
                UUID_A,
                u64::MAX,
                "applied",
                "auto",
                20,
            )],
            true,
        );
        let sessions = report["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_ne!(sessions[0]["sessionId"], sessions[1]["sessionId"]);
        assert_eq!(
            sessions[0]["gobstopper"]["compactions"]["recordedContextReductionTokens"],
            20
        );
        assert!(
            sessions[1]["gobstopper"]["compactions"]["recordedContextReductionTokens"].is_null()
        );
        assert!(sessions[1]["usage"].as_array().unwrap().is_empty());
        assert!(
            sessions[0]["gobstopper"]["compactions"]["lastAppliedMs"]
                .as_u64()
                .unwrap()
                < u64::MAX
        );
    }

    #[test]
    fn legacy_estimates_and_plans_are_never_recorded_savings() {
        let session = discovered(Provider::Codex, UUID_A, 0);
        let mut legacy = event(Provider::Codex, UUID_A, 1, "applied", "auto", 50_000);
        legacy.before_observation = None;
        legacy.after_observation = None;
        let report = build_report(&[session], &[legacy], true);
        let compactions = &report["sessions"][0]["gobstopper"]["compactions"];
        assert_eq!(compactions["applied"], 1);
        assert_eq!(compactions["estReclaimedTokens"], 0);
        assert_eq!(compactions["unqualifiedAppliedEvents"], 1);
        assert!(compactions["recordedContextReductionTokens"].is_null());
    }

    #[test]
    fn repeated_evidence_pair_does_not_multiply_recorded_reduction() {
        let session = discovered(Provider::Codex, UUID_A, 0);
        let event = event(Provider::Codex, UUID_A, 1, "applied", "auto", 20);
        let events = [event.clone(), event];
        let report = build_report(&[session], &events, true);
        let compactions = &report["sessions"][0]["gobstopper"]["compactions"];
        assert_eq!(compactions["recordedContextReductionTokens"], 20);
        assert_eq!(compactions["qualifiedReductionEvents"], 1);
        let cohorts = cohort_summary(&crate::config::Config::default(), &events);
        assert_eq!(
            cohorts["providers"]["codex"]["cohorts"]["ungated"]["reclaimed_tokens"],
            20
        );
    }

    #[test]
    fn contradictory_pairs_stay_excluded_and_legacy_cohort_stays_unknown() {
        let source = discovered(Provider::Codex, UUID_A, 0);
        let mut a = event(Provider::Codex, UUID_A, 1, "applied", "auto", 20);
        a.decision_cohort = None;
        let mut b = a.clone();
        b.after_observation.as_mut().unwrap().source_sha256 = "f".repeat(64);
        for events in [
            vec![a.clone(), b.clone(), a.clone()],
            vec![b.clone(), a.clone(), b.clone()],
        ] {
            let report = build_report(std::slice::from_ref(&source), &events, true);
            assert!(report["sessions"][0]["gobstopper"]["compactions"]
                ["recordedContextReductionTokens"]
                .is_null());
            assert_eq!(report["gobstopper"]["evidence"]["conflictingPairs"], 1);
            let mut config = crate::config::Config::default();
            config.rollout.insert("codex".into(), 100);
            let cohorts = cohort_summary(&config, &events);
            assert_eq!(
                cohorts["providers"]["codex"]["cohorts"]["unknown"]["events"],
                3
            );
            assert_eq!(
                cohorts["providers"]["codex"]["cohorts"]["treatment"]["events"],
                0
            );
        }
    }

    #[test]
    fn cohort_summary_uses_recorded_arms_independent_of_current_configuration() {
        let mut cfg = crate::config::Config::default();
        cfg.rollout.insert("claude_code".to_string(), 100);
        let mut events = vec![
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
        for event in &mut events {
            event.decision_cohort = Some(Cohort::Treatment);
            event.rollout_percent = Some(100);
        }
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

        cfg.rollout.insert("claude_code".to_string(), 0);
        let changed = cohort_summary(&cfg, &events);
        assert_eq!(
            changed["providers"]["claude_code"]["cohorts"],
            summary["providers"]["claude_code"]["cohorts"]
        );

        // 0% rollout: newly recorded events are control; a prompt-policy skip recorded
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
        suppressed.decision_cohort = Some(Cohort::Control);
        suppressed.rollout_percent = Some(0);
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
        unresolved.decision_cohort = Some(Cohort::Control);
        unresolved.rollout_percent = Some(0);
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
    fn native_control_decisions_are_counted_as_suppressed_watch_work() {
        let mut blocked = event(
            Provider::Devin,
            UUID_A,
            100,
            "skipped",
            "watch-native:control",
            0,
        );
        blocked.decision_cohort = Some(Cohort::Control);
        blocked.rollout_percent = Some(0);
        let result = cohort_summary(&crate::config::Config::default(), &[blocked]);
        assert_eq!(
            result["providers"]["devin"]["cohorts"]["control"]["watch_suppressed"],
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_source_paths_have_distinct_unqualified_fallback_ids() {
        use std::os::unix::ffi::OsStringExt;
        let root =
            std::env::temp_dir().join(format!("gobstopper-report-non-utf8-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let mut sessions = Vec::new();
        for suffix in [0xfe, 0xff] {
            let mut d = discovered(Provider::Codex, UUID_A, 0);
            d.handle.path = root.join(std::ffi::OsString::from_vec(vec![b's', suffix]));
            // Missing non-UTF8 paths exercise the serialization fallback on
            // platforms that cannot create such filesystem names (e.g. APFS).
            assert!(gobstopper_adapters::detect::source_identity(&d.handle).is_err());
            sessions.push(d);
        }
        let report = build_report(
            &sessions,
            &[event(Provider::Codex, UUID_A, 1, "applied", "auto", 7)],
            true,
        );
        let rows = report["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0]["sessionId"], rows[1]["sessionId"]);
        for row in rows {
            assert!(row["gobstopper"]["sourceIdentitySha256"].is_null());
            assert_eq!(
                row["gobstopper"]["sessionIdBasis"],
                "unavailable_source_path_hash_fallback"
            );
            assert_eq!(row["gobstopper"]["compactions"]["applied"], 0);
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
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
        let report = build_report(&sessions, &events, true);

        assert_eq!(report["schemaVersion"], 1);
        assert_eq!(report["profile"], "session-observations-v1");
        let list = report["sessions"].as_array().unwrap();
        assert_eq!(list.len(), 2);

        let s = &list[0];
        assert_eq!(s["provider"], "codex");
        assert_eq!(
            s["sessionId"],
            &s["gobstopper"]["sourceIdentitySha256"].as_str().unwrap()[..32]
        );
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
        assert_eq!(g["closedSessionCompact"], "unqualified");
        assert_eq!(g["nativeActivation"], false);
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
        let report = build_report(&sessions, &[native, planned, skipped, other], true);
        let counts = &report["sessions"][0]["gobstopper"]["compactions"];
        assert_eq!(counts["applied"], 2);
        assert_eq!(counts["nativeHookApplied"], 0); // a hook label is not a causal operation receipt
        assert_eq!(counts["estReclaimedTokens"], 100);
    }

    #[test]
    fn closed_session_compact_marks_subagent_threads_unavailable() {
        let dir = std::env::temp_dir().join(format!("gob-report-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("rollout-sub.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"source\":{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"p\"}}}}}\n",
        )
        .unwrap();
        let mut d = discovered(Provider::Codex, UUID_A, 0);
        d.handle.path = path.clone();
        let report = build_report(&[d], &[], true);
        assert_eq!(
            report["sessions"][0]["gobstopper"]["closedSessionCompact"],
            "unavailable:sub-agent"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_uuid_ids_get_stable_hex_digests() {
        let sessions = vec![discovered(Provider::Codex, "rollout-no-uuid-here", 0)];
        let a = build_report(&sessions, &[], true);
        let b = build_report(&sessions, &[], true);
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
        let report = build_report(&sessions, &[], true);
        assert_eq!(report["sessions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn no_absolute_paths_or_freeform_strings_leak() {
        // A path-like session id is the worst case: it must not be
        // echoed, and its digest keeps the sessionId shape.
        let mut d = discovered(Provider::ClaudeCode, "/private/tmp/secret/evil.jsonl", 5);
        d.handle.cwd = Some(PathBuf::from("/private/tmp/secret/worktree"));
        let report = build_report(&[d], &[], true);
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
        let report = build_report(&sessions, &[], true);
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
        let report = build_report(&sessions, &events, true);
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
        let report = build_report(&sessions, &events, true);
        let s = &report["sessions"][0];
        let start = s["window"]["startMs"].as_u64().unwrap();
        let end = s["window"]["endMs"].as_u64().unwrap();
        assert!(end >= start);
        assert!(end - start <= MAX_WINDOW_MS);
    }
}

//! Pure lenses over validated observations. Missing evidence stays visible.
use super::schema::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct Request {
    pub source: Source,
    pub identity: Identity,
    pub started_at_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub outcome: Option<Outcome>,
    pub duration_ms: Option<u64>,
    pub first_output_ms: Option<u64>,
    pub usage: Option<Usage>,
    pub generation: Option<Generation>,
    /// A missing terminal is incomplete evidence, including after a process crash.
    pub incomplete: bool,
}

pub fn requests(events: &[Envelope]) -> Vec<Request> {
    let mut rows = BTreeMap::<(OpaqueId, OpaqueId), Request>::new();
    for item in events {
        if !matches!(
            item.event,
            Event::RequestStarted { .. } | Event::RequestFinished { .. }
        ) {
            continue;
        }
        let Some(attempt) = &item.identity.attempt_id else {
            continue;
        };
        let row = rows
            .entry((item.source.id.clone(), attempt.clone()))
            .or_insert_with(|| Request {
                source: item.source.clone(),
                identity: item.identity.clone(),
                started_at_ms: None,
                finished_at_ms: None,
                provider: None,
                model: None,
                outcome: None,
                duration_ms: None,
                first_output_ms: None,
                usage: None,
                generation: None,
                incomplete: true,
            });
        match &item.event {
            Event::RequestStarted { provider, model } => {
                row.started_at_ms = Some(item.observed_at_ms);
                row.provider = Some(provider.clone());
                row.model = model.clone();
            }
            Event::RequestFinished {
                outcome,
                duration_ms,
                first_output_ms,
                usage,
                generation,
                ..
            } => {
                row.finished_at_ms = Some(item.observed_at_ms);
                row.outcome = Some(*outcome);
                row.duration_ms = *duration_ms;
                row.first_output_ms = *first_output_ms;
                row.usage = usage.clone();
                row.generation = *generation;
                row.incomplete = false;
            }
            _ => {}
        }
    }
    rows.into_values().collect()
}

#[derive(Debug, Serialize)]
pub struct Session {
    pub source_id: OpaqueId,
    pub session_id: OpaqueId,
    pub first_observed_ms: u64,
    pub last_observed_ms: u64,
    pub request_attempts: u64,
    pub incomplete_attempts: u64,
    pub tool_invocations: u64,
    pub context_decisions: u64,
}

#[derive(Debug, Serialize)]
pub struct Sessions {
    pub sessions: Vec<Session>,
    pub attempts_without_session: u64,
}

pub fn sessions(events: &[Envelope]) -> Sessions {
    let mut rows = BTreeMap::<(OpaqueId, OpaqueId), Session>::new();
    let mut tools = BTreeMap::<(OpaqueId, OpaqueId), BTreeSet<OpaqueId>>::new();
    for item in events {
        let Some(session) = &item.identity.session_id else {
            continue;
        };
        let key = (item.source.id.clone(), session.clone());
        let row = rows.entry(key.clone()).or_insert_with(|| Session {
            source_id: key.0.clone(),
            session_id: key.1.clone(),
            first_observed_ms: item.observed_at_ms,
            last_observed_ms: item.observed_at_ms,
            request_attempts: 0,
            incomplete_attempts: 0,
            tool_invocations: 0,
            context_decisions: 0,
        });
        row.first_observed_ms = row.first_observed_ms.min(item.observed_at_ms);
        row.last_observed_ms = row.last_observed_ms.max(item.observed_at_ms);
        if matches!(item.event, Event::ContextDecision { .. }) {
            row.context_decisions += 1;
        }
        if let Some(tool) = &item.identity.tool_id {
            tools.entry(key).or_default().insert(tool.clone());
        }
    }
    let mut unassigned = 0;
    for request in requests(events) {
        if let Some(session) = &request.identity.session_id {
            if let Some(row) = rows.get_mut(&(request.source.id.clone(), session.clone())) {
                row.request_attempts += 1;
                row.incomplete_attempts += u64::from(request.incomplete);
            }
        } else {
            unassigned += 1;
        }
    }
    for (key, row) in &mut rows {
        row.tool_invocations = tools.get(key).map(|v| v.len() as u64).unwrap_or(0);
    }
    Sessions {
        sessions: rows.into_values().collect(),
        attempts_without_session: unassigned,
    }
}

#[derive(Debug, Serialize)]
pub struct Tool {
    pub source_id: OpaqueId,
    pub session_id: Option<OpaqueId>,
    pub request_id: Option<OpaqueId>,
    pub attempt_id: Option<OpaqueId>,
    pub tool_id: OpaqueId,
    pub name: Option<String>,
    pub requested: bool,
    pub dispatched: bool,
    pub terminal: bool,
    pub outcome: Option<Outcome>,
}
pub fn tools(events: &[Envelope]) -> Vec<Tool> {
    let mut rows = BTreeMap::<(OpaqueId, OpaqueId), Tool>::new();
    for item in events {
        let Event::ToolObserved {
            stage,
            outcome,
            tool_name,
        } = &item.event
        else {
            continue;
        };
        let Some(tool) = &item.identity.tool_id else {
            continue;
        };
        let key = (item.source.id.clone(), tool.clone());
        let row = rows.entry(key.clone()).or_insert_with(|| Tool {
            source_id: key.0,
            session_id: item.identity.session_id.clone(),
            request_id: item.identity.request_id.clone(),
            attempt_id: item.identity.attempt_id.clone(),
            tool_id: key.1,
            name: tool_name.clone(),
            requested: false,
            dispatched: false,
            terminal: false,
            outcome: None,
        });
        if row.name.is_none() {
            row.name = tool_name.clone();
        }
        match stage {
            ToolStage::Requested => row.requested = true,
            ToolStage::Dispatched => row.dispatched = true,
            ToolStage::Terminal => {
                row.terminal = true;
                row.outcome = Some(*outcome);
            }
        }
    }
    rows.into_values().collect()
}

#[derive(Debug, Serialize)]
pub struct Measure {
    pub id: &'static str,
    pub unit: &'static str,
    pub value: Option<f64>,
    /// Decimal strings keep exact numerators portable beyond JSON's 53-bit range.
    pub numerator: String,
    pub denominator: String,
    pub measured: u64,
    pub missing: u64,
    pub status: &'static str,
    pub unavailable_reason: Option<&'static str>,
}

fn mean(
    id: &'static str,
    unit: &'static str,
    values: impl Iterator<Item = Option<u64>>,
) -> Measure {
    let mut sum = 0u128;
    let mut measured = 0u64;
    let mut missing = 0u64;
    for value in values {
        if let Some(value) = value {
            sum += value as u128;
            measured += 1;
        } else {
            missing += 1;
        }
    }
    Measure {
        id,
        unit,
        value: (measured > 0).then(|| sum as f64 / measured as f64),
        numerator: sum.to_string(),
        denominator: measured.to_string(),
        measured,
        missing,
        status: if measured == 0 {
            "unavailable"
        } else if missing > 0 {
            "partial"
        } else {
            "available"
        },
        unavailable_reason: (measured == 0).then_some("no_measured_observations"),
    }
}

#[derive(Debug, Serialize)]
pub struct Cohort {
    pub source_kind: SourceKind,
    pub source_profile: String,
    pub attempts: u64,
    pub incomplete_attempts: u64,
    pub unknown_outcomes: u64,
    pub reported_input_tokens: String,
    pub reported_output_tokens: String,
    pub estimated_input_tokens: String,
    pub estimated_output_tokens: String,
    pub requests_with_input: u64,
    pub requests_with_output: u64,
    pub applied_compactions: u64,
    pub shadow_compactions: u64,
    pub legacy_observations: u64,
    pub context_tokens_removed_estimate: String,
    pub measures: Vec<Measure>,
}
#[derive(Debug, Serialize)]
pub struct Metrics {
    pub schema_version: u32,
    pub profile: &'static str,
    pub cohorts: Vec<Cohort>,
    pub cohort_rule: &'static str,
}

pub fn metrics(events: &[Envelope]) -> Metrics {
    let all = requests(events);
    let mut cohorts = Vec::new();
    let mut cohort_keys = Vec::new();
    for kind in [
        SourceKind::LiveProxy,
        SourceKind::NativeTranscript,
        SourceKind::LegacyStats,
    ] {
        let profiles: BTreeSet<_> = events
            .iter()
            .filter(|e| e.source.kind == kind)
            .map(|e| e.source.profile.clone())
            .collect();
        cohort_keys.extend(profiles.into_iter().map(|profile| (kind, profile)));
    }
    for (kind, profile) in cohort_keys {
        let rows: Vec<_> = all
            .iter()
            .filter(|r| r.source.kind == kind && r.source.profile == profile)
            .collect();
        let mut input = 0u128;
        let mut output = 0u128;
        let mut est_input = 0u128;
        let mut est_output = 0u128;
        let mut with_input = 0;
        let mut with_output = 0;
        for row in &rows {
            if let Some(usage) = &row.usage {
                if let Some(q) = usage.input_tokens {
                    with_input += 1;
                    match q.basis {
                        Basis::Reported => input += q.value as u128,
                        Basis::Estimated => est_input += q.value as u128,
                    }
                }
                if let Some(q) = usage.output_tokens {
                    with_output += 1;
                    match q.basis {
                        Basis::Reported => output += q.value as u128,
                        Basis::Estimated => est_output += q.value as u128,
                    }
                }
            }
        }
        let mut applied = 0;
        let mut shadow = 0;
        let mut removed = 0u128;
        let mut legacy = 0;
        for item in events
            .iter()
            .filter(|e| e.source.kind == kind && e.source.profile == profile)
        {
            match item.event {
                Event::ContextDecision {
                    estimated_before_tokens,
                    estimated_after_tokens,
                    compacted: true,
                    shadow: is_shadow,
                    ..
                } => {
                    if is_shadow {
                        shadow += 1;
                    } else {
                        applied += 1;
                        removed +=
                            estimated_before_tokens.saturating_sub(estimated_after_tokens) as u128;
                    }
                }
                Event::LegacyContext { .. } => legacy += 1,
                _ => {}
            }
        }
        let mut measures = vec![
            mean(
                "request_duration_mean",
                "ms",
                rows.iter().map(|r| r.duration_ms),
            ),
            mean(
                "first_output_latency_mean",
                "ms",
                rows.iter().map(|r| r.first_output_ms),
            ),
            mean(
                "reported_input_tokens_mean",
                "tokens",
                rows.iter().map(|r| {
                    r.usage
                        .as_ref()
                        .and_then(|u| u.input_tokens)
                        .filter(|q| q.basis == Basis::Reported)
                        .map(|q| q.value)
                }),
            ),
            mean(
                "reported_output_tokens_mean",
                "tokens",
                rows.iter().map(|r| {
                    r.usage
                        .as_ref()
                        .and_then(|u| u.output_tokens)
                        .filter(|q| q.basis == Basis::Reported)
                        .map(|q| q.value)
                }),
            ),
        ];
        let mut generation_tokens = 0u128;
        let mut generation_ms = 0u128;
        let mut generation_count = 0u64;
        for span in rows
            .iter()
            .filter_map(|r| r.generation)
            .filter(|g| g.duration_ms > 0)
        {
            generation_count += 1;
            generation_tokens += span.output_tokens as u128;
            generation_ms += span.duration_ms as u128;
        }
        measures.push(Measure {
            id: "generation_output_tokens_per_second",
            unit: "tokens/s",
            value: (generation_ms > 0)
                .then(|| 1000.0 * generation_tokens as f64 / generation_ms as f64),
            numerator: generation_tokens.to_string(),
            denominator: generation_ms.to_string(),
            measured: generation_count,
            missing: rows.len() as u64 - generation_count,
            status: if generation_count == 0 {
                "unavailable"
            } else if generation_count < rows.len() as u64 {
                "partial"
            } else {
                "available"
            },
            unavailable_reason: (generation_count == 0).then_some("no_matching_generation_spans"),
        });
        let mut request_tokens = 0u128;
        let mut request_ms = 0u128;
        let mut request_count = 0u64;
        for row in &rows {
            if let (Some(tokens), Some(ms)) = (
                row.usage
                    .as_ref()
                    .and_then(|u| u.output_tokens)
                    .filter(|q| q.basis == Basis::Reported),
                row.duration_ms.filter(|ms| *ms > 0),
            ) {
                request_tokens += tokens.value as u128;
                request_ms += ms as u128;
                request_count += 1;
            }
        }
        measures.push(Measure {
            id: "request_output_tokens_per_second",
            unit: "tokens/s",
            value: (request_ms > 0).then(|| 1000.0 * request_tokens as f64 / request_ms as f64),
            numerator: request_tokens.to_string(),
            denominator: request_ms.to_string(),
            measured: request_count,
            missing: rows.len() as u64 - request_count,
            status: if request_count == 0 {
                "unavailable"
            } else if request_count < rows.len() as u64 {
                "partial"
            } else {
                "available"
            },
            unavailable_reason: (request_count == 0)
                .then_some("no_matching_request_tokens_and_durations"),
        });
        cohorts.push(Cohort {
            source_kind: kind,
            source_profile: profile,
            attempts: rows.len() as u64,
            incomplete_attempts: rows.iter().filter(|r| r.incomplete).count() as u64,
            unknown_outcomes: rows
                .iter()
                .filter(|r| r.outcome.is_none_or(|o| o == Outcome::Unknown))
                .count() as u64,
            reported_input_tokens: input.to_string(),
            reported_output_tokens: output.to_string(),
            estimated_input_tokens: est_input.to_string(),
            estimated_output_tokens: est_output.to_string(),
            requests_with_input: with_input,
            requests_with_output: with_output,
            applied_compactions: applied,
            shadow_compactions: shadow,
            legacy_observations: legacy,
            context_tokens_removed_estimate: removed.to_string(),
            measures,
        });
    }
    Metrics {schema_version:1,profile:"observed-request-metrics-v1",cohorts,cohort_rule:"source kinds and parser profiles remain separate; retries are distinct attempts; unmeasured values are absent"}
}

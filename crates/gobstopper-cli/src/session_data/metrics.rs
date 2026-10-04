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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<FinishReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<RequestTimings>,
    /// A missing terminal is incomplete evidence, including after a process crash.
    pub incomplete: bool,
}

impl Request {
    pub(super) fn new(item: &Envelope) -> Self {
        Self {
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
            reason: None,
            timings: None,
            incomplete: true,
        }
    }

    pub(super) fn observe(&mut self, item: &Envelope) {
        match &item.event {
            Event::RequestStarted { provider, model } => {
                self.started_at_ms = Some(item.observed_at_ms);
                self.provider = Some(provider.clone());
                self.model = model.clone();
            }
            Event::RequestFinished {
                outcome,
                duration_ms,
                first_output_ms,
                usage,
                generation,
                reason,
                timings,
                ..
            } => {
                self.finished_at_ms = Some(item.observed_at_ms);
                self.outcome = Some(*outcome);
                self.duration_ms = *duration_ms;
                self.first_output_ms = *first_output_ms;
                self.usage = usage.clone();
                self.generation = *generation;
                self.reason = *reason;
                self.timings = *timings;
                self.incomplete = false;
            }
            _ => {}
        }
    }
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
        rows.entry((item.source.id.clone(), attempt.clone()))
            .or_insert_with(|| Request::new(item))
            .observe(item);
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

#[derive(Default)]
struct Mean {
    sum: u128,
    measured: u64,
    missing: u64,
}

impl Mean {
    fn observe(&mut self, value: Option<u64>) {
        if let Some(value) = value {
            self.sum += value as u128;
            self.measured += 1;
        } else {
            self.missing += 1;
        }
    }

    fn measure(&self, id: &'static str, unit: &'static str) -> Measure {
        Measure {
            id,
            unit,
            value: (self.measured > 0).then(|| self.sum as f64 / self.measured as f64),
            numerator: self.sum.to_string(),
            denominator: self.measured.to_string(),
            measured: self.measured,
            missing: self.missing,
            status: coverage(self.measured, self.missing),
            unavailable_reason: (self.measured == 0).then_some("no_measured_observations"),
        }
    }
}

fn coverage(measured: u64, missing: u64) -> &'static str {
    if measured == 0 {
        "unavailable"
    } else if missing > 0 {
        "partial"
    } else {
        "available"
    }
}

#[derive(Default)]
struct Rate {
    tokens: u128,
    ms: u128,
    measured: u64,
}

impl Rate {
    fn observe(&mut self, tokens: u64, ms: u64) {
        self.tokens += tokens as u128;
        self.ms += ms as u128;
        self.measured += 1;
    }

    fn measure(&self, id: &'static str, attempts: u64, reason: &'static str) -> Measure {
        Measure {
            id,
            unit: "tokens/s",
            value: (self.ms > 0).then(|| 1000.0 * self.tokens as f64 / self.ms as f64),
            numerator: self.tokens.to_string(),
            denominator: self.ms.to_string(),
            measured: self.measured,
            missing: attempts - self.measured,
            status: coverage(self.measured, attempts - self.measured),
            unavailable_reason: (self.measured == 0).then_some(reason),
        }
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

const OUTCOMES: [(Outcome, &str); 7] = [
    (Outcome::Success, "request_outcome_success_fraction"),
    (Outcome::Error, "request_outcome_error_fraction"),
    (Outcome::Refused, "request_outcome_refused_fraction"),
    (Outcome::Cancelled, "request_outcome_cancelled_fraction"),
    (Outcome::Timeout, "request_outcome_timeout_fraction"),
    (Outcome::Interrupted, "request_outcome_interrupted_fraction"),
    (Outcome::Unknown, "request_outcome_unknown_fraction"),
];
const REASONS: [(FinishReason, &str); 12] = [
    (FinishReason::Completed, "request_finish_completed_fraction"),
    (
        FinishReason::ClientDisconnected,
        "request_finish_client_disconnected_fraction",
    ),
    (
        FinishReason::ClientWriteFailed,
        "request_finish_client_write_failed_fraction",
    ),
    (
        FinishReason::UpstreamUnavailable,
        "request_finish_upstream_unavailable_fraction",
    ),
    (
        FinishReason::UpstreamReadFailed,
        "request_finish_upstream_read_failed_fraction",
    ),
    (
        FinishReason::UpstreamTimeout,
        "request_finish_upstream_timeout_fraction",
    ),
    (
        FinishReason::UpstreamTruncated,
        "request_finish_upstream_truncated_fraction",
    ),
    (
        FinishReason::MissingTerminal,
        "request_finish_missing_terminal_fraction",
    ),
    (
        FinishReason::ParserUncertain,
        "request_finish_parser_uncertain_fraction",
    ),
    (
        FinishReason::ProviderRefused,
        "request_finish_provider_refused_fraction",
    ),
    (
        FinishReason::ProviderError,
        "request_finish_provider_error_fraction",
    ),
    (
        FinishReason::ProxyInterrupted,
        "request_finish_proxy_interrupted_fraction",
    ),
];

fn fraction(
    id: &'static str,
    count: u64,
    measured: u64,
    attempts: u64,
    reason: &'static str,
) -> Measure {
    Measure {
        id,
        unit: "fraction",
        value: (measured > 0).then(|| count as f64 / measured as f64),
        numerator: count.to_string(),
        denominator: measured.to_string(),
        measured,
        missing: attempts - measured,
        status: coverage(measured, attempts - measured),
        unavailable_reason: (measured == 0).then_some(reason),
    }
}

#[derive(Default)]
struct Aggregate {
    attempts: u64,
    incomplete: u64,
    unknown: u64,
    input: u128,
    output: u128,
    estimated_input: u128,
    estimated_output: u128,
    with_input: u64,
    with_output: u64,
    applied: u64,
    shadow: u64,
    removed: u128,
    legacy: u64,
    duration: Mean,
    first_output: Mean,
    reported_input: Mean,
    reported_output: Mean,
    generation_rate: Rate,
    request_rate: Rate,
    outcomes: [u64; 7],
    reasons: [u64; 12],
    preparation: Mean,
    upstream_headers: Mean,
    transform: Mean,
}

impl Aggregate {
    fn event(&mut self, item: &Envelope) {
        match item.event {
            Event::ContextDecision {
                estimated_before_tokens,
                estimated_after_tokens,
                compacted: true,
                shadow,
                ..
            } => {
                if shadow {
                    self.shadow += 1;
                } else {
                    self.applied += 1;
                    self.removed +=
                        estimated_before_tokens.saturating_sub(estimated_after_tokens) as u128;
                }
            }
            Event::LegacyContext { .. } => self.legacy += 1,
            _ => {}
        }
    }

    fn request(&mut self, row: &Request) {
        self.attempts += 1;
        self.incomplete += u64::from(row.incomplete);
        self.unknown += u64::from(row.outcome.is_none_or(|o| o == Outcome::Unknown));
        if let Some(outcome) = row.outcome {
            self.outcomes[match outcome {
                Outcome::Success => 0,
                Outcome::Error => 1,
                Outcome::Refused => 2,
                Outcome::Cancelled => 3,
                Outcome::Timeout => 4,
                Outcome::Interrupted => 5,
                Outcome::Unknown => 6,
            }] += 1;
        }
        if let Some(reason) = row.reason {
            self.reasons[match reason {
                FinishReason::Completed => 0,
                FinishReason::ClientDisconnected => 1,
                FinishReason::ClientWriteFailed => 2,
                FinishReason::UpstreamUnavailable => 3,
                FinishReason::UpstreamReadFailed => 4,
                FinishReason::UpstreamTimeout => 5,
                FinishReason::UpstreamTruncated => 6,
                FinishReason::MissingTerminal => 7,
                FinishReason::ParserUncertain => 8,
                FinishReason::ProviderRefused => 9,
                FinishReason::ProviderError => 10,
                FinishReason::ProxyInterrupted => 11,
            }] += 1;
        }
        self.preparation
            .observe(row.timings.map(|t| t.preparation_ms));
        self.upstream_headers
            .observe(row.timings.map(|t| t.upstream_headers_ms));
        self.transform
            .observe(row.timings.and_then(|t| t.transform_ms));
        let input = row.usage.as_ref().and_then(|u| u.input_tokens);
        let output = row.usage.as_ref().and_then(|u| u.output_tokens);
        if let Some(q) = input {
            self.with_input += 1;
            match q.basis {
                Basis::Reported => self.input += q.value as u128,
                Basis::Estimated => self.estimated_input += q.value as u128,
            }
        }
        if let Some(q) = output {
            self.with_output += 1;
            match q.basis {
                Basis::Reported => self.output += q.value as u128,
                Basis::Estimated => self.estimated_output += q.value as u128,
            }
        }
        let reported =
            |q: Option<Quantity>| q.filter(|q| q.basis == Basis::Reported).map(|q| q.value);
        self.duration.observe(row.duration_ms);
        self.first_output.observe(row.first_output_ms);
        self.reported_input.observe(reported(input));
        self.reported_output.observe(reported(output));
        if let Some(span) = row.generation.filter(|g| g.duration_ms > 0) {
            self.generation_rate
                .observe(span.output_tokens, span.duration_ms);
        }
        if let (Some(tokens), Some(ms)) = (reported(output), row.duration_ms.filter(|ms| *ms > 0)) {
            self.request_rate.observe(tokens, ms);
        }
    }

    fn finish(self, source_kind: SourceKind, source_profile: String) -> Cohort {
        let mut measures = vec![
            self.duration.measure("request_duration_mean", "ms"),
            self.first_output.measure("first_output_latency_mean", "ms"),
            self.reported_input
                .measure("reported_input_tokens_mean", "tokens"),
            self.reported_output
                .measure("reported_output_tokens_mean", "tokens"),
            self.generation_rate.measure(
                "generation_output_tokens_per_second",
                self.attempts,
                "no_matching_generation_spans",
            ),
            self.request_rate.measure(
                "request_output_tokens_per_second",
                self.attempts,
                "no_matching_request_tokens_and_durations",
            ),
            self.preparation.measure("request_preparation_mean", "ms"),
            self.upstream_headers
                .measure("upstream_headers_latency_mean", "ms"),
            self.transform.measure("request_transform_mean", "ms"),
        ];
        let outcomes = self.outcomes.iter().sum();
        let reasons = self.reasons.iter().sum();
        measures.extend(OUTCOMES.iter().enumerate().map(|(index, (_, id))| {
            fraction(
                id,
                self.outcomes[index],
                outcomes,
                self.attempts,
                "no_observed_request_outcomes",
            )
        }));
        measures.extend(REASONS.iter().enumerate().map(|(index, (_, id))| {
            fraction(
                id,
                self.reasons[index],
                reasons,
                self.attempts,
                "no_observed_finish_reasons",
            )
        }));
        Cohort {
            source_kind,
            source_profile,
            attempts: self.attempts,
            incomplete_attempts: self.incomplete,
            unknown_outcomes: self.unknown,
            reported_input_tokens: self.input.to_string(),
            reported_output_tokens: self.output.to_string(),
            estimated_input_tokens: self.estimated_input.to_string(),
            estimated_output_tokens: self.estimated_output.to_string(),
            requests_with_input: self.with_input,
            requests_with_output: self.with_output,
            applied_compactions: self.applied,
            shadow_compactions: self.shadow,
            legacy_observations: self.legacy,
            context_tokens_removed_estimate: self.removed.to_string(),
            measures,
        }
    }
}

const KINDS: [SourceKind; 3] = [
    SourceKind::LiveProxy,
    SourceKind::NativeTranscript,
    SourceKind::LegacyStats,
];
const PROFILES: [&str; 6] = [
    "claude-metadata-v1",
    "codex-metadata-v1",
    "gobstopper-events-v1",
    "gobstopper-proxy-v1",
    "gobstopper-proxy-v2",
    "gobstopper-stats-v0",
];

#[derive(Default)]
pub(super) struct BoundedAccumulator {
    cohorts: [Option<Aggregate>; KINDS.len() * PROFILES.len()],
}

impl BoundedAccumulator {
    fn cohort(&mut self, source: &Source) -> anyhow::Result<&mut Aggregate> {
        let kind = match source.kind {
            SourceKind::LiveProxy => 0,
            SourceKind::NativeTranscript => 1,
            SourceKind::LegacyStats => 2,
        };
        let profile = PROFILES
            .iter()
            .position(|p| *p == source.profile)
            .ok_or_else(|| anyhow::anyhow!("data_metric_profile_unsupported"))?;
        Ok(self.cohorts[kind * PROFILES.len() + profile].get_or_insert_with(Aggregate::default))
    }

    pub(super) fn event(&mut self, item: &Envelope) -> anyhow::Result<()> {
        self.cohort(&item.source)?.event(item);
        Ok(())
    }

    pub(super) fn request(&mut self, row: &Request) -> anyhow::Result<()> {
        self.cohort(&row.source)?.request(row);
        Ok(())
    }

    pub(super) fn finish(self) -> Metrics {
        report(
            self.cohorts
                .into_iter()
                .enumerate()
                .filter_map(|(index, aggregate)| {
                    aggregate.map(|aggregate| {
                        aggregate.finish(
                            KINDS[index / PROFILES.len()],
                            PROFILES[index % PROFILES.len()].into(),
                        )
                    })
                })
                .collect(),
        )
    }
}

fn report(cohorts: Vec<Cohort>) -> Metrics {
    Metrics {
        schema_version: 1,
        profile: "observed-request-metrics-v1",
        cohorts,
        cohort_rule: "source kinds and parser profiles remain separate; retries are distinct attempts; unmeasured values are absent",
    }
}

#[derive(Default)]
struct Accumulator {
    cohorts: BTreeMap<(u8, String), (SourceKind, Aggregate)>,
}

impl Accumulator {
    fn cohort(&mut self, source: &Source) -> &mut Aggregate {
        let kind = match source.kind {
            SourceKind::LiveProxy => 0,
            SourceKind::NativeTranscript => 1,
            SourceKind::LegacyStats => 2,
        };
        &mut self
            .cohorts
            .entry((kind, source.profile.clone()))
            .or_insert_with(|| (source.kind, Aggregate::default()))
            .1
    }

    pub(super) fn event(&mut self, item: &Envelope) {
        self.cohort(&item.source).event(item);
    }

    pub(super) fn request(&mut self, row: &Request) {
        self.cohort(&row.source).request(row);
    }

    fn finish(self) -> Metrics {
        report(
            self.cohorts
                .into_iter()
                .map(|((_, profile), (kind, aggregate))| aggregate.finish(kind, profile))
                .collect(),
        )
    }
}

#[allow(dead_code)]
pub fn metrics(events: &[Envelope]) -> Metrics {
    let mut accumulator = Accumulator::default();
    for item in events {
        accumulator.event(item);
    }
    for row in requests(events) {
        accumulator.request(&row);
    }
    accumulator.finish()
}

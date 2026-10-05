//! Content-free observations. Each version describes evidence, never inferred success.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const EVENT_VERSION: u32 = 1;
pub const MAX_EVENT_BYTES: usize = 16 * 1024;
pub const MAX_COUNTER: u64 = 1_000_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueId(pub String);

impl OpaqueId {
    pub fn random() -> Result<Self> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("data_random_unavailable"))?;
        Ok(Self(hex(&bytes)))
    }
    pub fn validate(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            && self.0.bytes().any(|c| c != b'0')
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    LiveProxy,
    NativeTranscript,
    LegacyStats,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: SourceKind,
    pub id: OpaqueId,
    /// Owned parser profile, not a file path or user-entered label.
    pub profile: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub runtime_id: Option<OpaqueId>,
    pub session_id: Option<OpaqueId>,
    pub request_id: Option<OpaqueId>,
    pub attempt_id: Option<OpaqueId>,
    pub tool_id: Option<OpaqueId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Reported,
    Estimated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quantity {
    pub value: u64,
    pub basis: Basis,
}

impl Quantity {
    pub fn reported(value: u64) -> Self {
        Self {
            value,
            basis: Basis::Reported,
        }
    }
}

/// Input includes all cache categories. Output includes its reasoning subset.
/// Missing counters stay None; zero is used only for an observed numeric zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: Option<Quantity>,
    pub output_tokens: Option<Quantity>,
    pub cache_read_tokens: Option<Quantity>,
    pub cache_write_tokens: Option<Quantity>,
    pub reasoning_tokens: Option<Quantity>,
}

impl Usage {
    pub fn validate(&self) -> bool {
        let within = [
            self.input_tokens,
            self.output_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
            self.reasoning_tokens,
        ]
        .into_iter()
        .flatten()
        .all(|q| q.value <= MAX_COUNTER);
        let subset = |child: Option<Quantity>, parent: Option<Quantity>| match (child, parent) {
            (Some(c), Some(p)) if c.basis == p.basis => c.value <= p.value,
            _ => true,
        };
        let cache_sum = match (
            self.input_tokens,
            self.cache_read_tokens,
            self.cache_write_tokens,
        ) {
            (Some(input), Some(read), Some(write))
                if input.basis == read.basis && input.basis == write.basis =>
            {
                read.value
                    .checked_add(write.value)
                    .is_some_and(|n| n <= input.value)
            }
            _ => true,
        };
        within
            && subset(self.reasoning_tokens, self.output_tokens)
            && subset(self.cache_read_tokens, self.input_tokens)
            && subset(self.cache_write_tokens, self.input_tokens)
            && cache_sum
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Error,
    Refused,
    Cancelled,
    Timeout,
    Interrupted,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Completed,
    ClientDisconnected,
    ClientWriteFailed,
    UpstreamUnavailable,
    UpstreamReadFailed,
    UpstreamTimeout,
    UpstreamTruncated,
    MissingTerminal,
    ParserUncertain,
    ProviderRefused,
    ProviderError,
    ProxyInterrupted,
}

impl FinishReason {
    pub fn outcome(self) -> Outcome {
        match self {
            Self::Completed => Outcome::Success,
            Self::ClientDisconnected => Outcome::Cancelled,
            Self::ClientWriteFailed
            | Self::UpstreamReadFailed
            | Self::UpstreamTruncated
            | Self::MissingTerminal
            | Self::ProxyInterrupted => Outcome::Interrupted,
            Self::UpstreamTimeout => Outcome::Timeout,
            Self::ParserUncertain => Outcome::Unknown,
            Self::ProviderRefused => Outcome::Refused,
            Self::ProviderError | Self::UpstreamUnavailable => Outcome::Error,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ClientDisconnected => "client_disconnected",
            Self::ClientWriteFailed => "client_write_failed",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::UpstreamReadFailed => "upstream_read_failed",
            Self::UpstreamTimeout => "upstream_timeout",
            Self::UpstreamTruncated => "upstream_truncated",
            Self::MissingTerminal => "missing_terminal",
            Self::ParserUncertain => "parser_uncertain",
            Self::ProviderRefused => "provider_refused",
            Self::ProviderError => "provider_error",
            Self::ProxyInterrupted => "proxy_interrupted",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestTimings {
    pub preparation_ms: u64,
    pub upstream_headers_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_ms: Option<u64>,
}

impl RequestTimings {
    fn validate(&self, total_ms: Option<u64>) -> bool {
        total_ms.is_some_and(|total| {
            self.preparation_ms
                .checked_add(self.upstream_headers_ms)
                .is_some_and(|sum| sum <= total)
                && self.transform_ms.is_none_or(|n| n <= self.preparation_ms)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStage {
    Requested,
    Dispatched,
    Terminal,
}

/// An independently measured output-token count and exactly matching generation
/// interval. A request wall duration is not a generation interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub output_tokens: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyObservation {
    pub requested_input_tokens: Option<u64>,
    pub input_capacity_tokens: Option<u64>,
    pub policy_generation: Option<u64>,
    pub limiting_reason: Option<String>,
    pub scope_id: Option<OpaqueId>,
    pub evidence_bytes: Option<u64>,
    pub evicted_evidence_count: Option<u64>,
    pub rescued: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    RequestStarted {
        provider: String,
        model: Option<String>,
    },
    RequestFinished {
        outcome: Outcome,
        http_status: Option<u16>,
        duration_ms: Option<u64>,
        first_output_ms: Option<u64>,
        usage: Option<Usage>,
        generation: Option<Generation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<FinishReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timings: Option<RequestTimings>,
    },
    ContextDecision {
        estimated_before_tokens: u64,
        estimated_after_tokens: u64,
        threshold_tokens: u64,
        compacted: bool,
        shadow: bool,
        policy: Option<PolicyObservation>,
    },
    ToolObserved {
        stage: ToolStage,
        outcome: Outcome,
        tool_name: Option<String>,
    },
    LegacyContext {
        estimated_before_tokens: u64,
        estimated_after_tokens: u64,
        compacted: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema_version: u32,
    pub event_id: OpaqueId,
    pub source: Source,
    pub observed_at_ms: u64,
    pub identity: Identity,
    pub event: Event,
}

pub fn safe_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-/".contains(&b))
        && !value.contains("..")
        && !value.starts_with('/')
}

impl Envelope {
    pub fn new(source: Source, identity: Identity, event: Event) -> Result<Self> {
        let result = Self {
            schema_version: EVENT_VERSION,
            event_id: OpaqueId::random()?,
            source,
            observed_at_ms: now_ms(),
            identity,
            event,
        };
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != EVENT_VERSION {
            bail!("data_event_version_unsupported");
        }
        if !self.event_id.validate()
            || !self.source.id.validate()
            || !matches!(
                self.source.profile.as_str(),
                "gobstopper-proxy-v1"
                    | "gobstopper-proxy-v2"
                    | "gobstopper-stats-v0"
                    | "gobstopper-events-v1"
                    | "claude-metadata-v1"
                    | "codex-metadata-v1"
            )
            || self.observed_at_ms > 8_640_000_000_000_000
            || [
                &self.identity.runtime_id,
                &self.identity.session_id,
                &self.identity.request_id,
                &self.identity.attempt_id,
                &self.identity.tool_id,
            ]
            .into_iter()
            .flatten()
            .any(|id| !id.validate())
        {
            bail!("data_invalid_identity");
        }
        let request = self.identity.request_id.is_some() && self.identity.attempt_id.is_some();
        let valid = match &self.event {
            Event::RequestStarted { provider, model } => {
                request
                    && matches!(
                        provider.as_str(),
                        "anthropic" | "responses" | "chat_completions" | "codex" | "claude_code"
                    )
                    && model.as_deref().is_none_or(safe_label)
            }
            Event::RequestFinished {
                http_status,
                duration_ms,
                first_output_ms,
                usage,
                generation,
                timings,
                reason,
                outcome,
            } => {
                request
                    && reason.is_none_or(|r| {
                        r.outcome() == *outcome
                            && (r != FinishReason::Completed || http_status.is_none_or(|s| s < 400))
                    })
                    && http_status.is_none_or(|s| (100..=599).contains(&s))
                    && duration_ms.is_none_or(|n| n <= 31 * 86_400_000)
                    && first_output_ms.is_none_or(|n| duration_ms.is_some_and(|d| n <= d))
                    && timings.as_ref().is_none_or(|t| t.validate(*duration_ms))
                    && usage.as_ref().is_none_or(Usage::validate)
                    && generation.is_none_or(|g| {
                        g.output_tokens <= MAX_COUNTER
                            && g.duration_ms > 0
                            && duration_ms.is_some_and(|d| g.duration_ms <= d)
                            && usage
                                .as_ref()
                                .and_then(|u| u.output_tokens)
                                .is_some_and(|q| {
                                    q.basis == Basis::Reported && g.output_tokens <= q.value
                                })
                    })
            }
            Event::ContextDecision {
                estimated_before_tokens,
                estimated_after_tokens,
                threshold_tokens,
                policy,
                ..
            } => {
                request
                    && [
                        *estimated_before_tokens,
                        *estimated_after_tokens,
                        *threshold_tokens,
                    ]
                    .into_iter()
                    .all(|n| n <= MAX_COUNTER)
                    && policy.as_ref().is_none_or(|p| {
                        [
                            p.requested_input_tokens,
                            p.input_capacity_tokens,
                            p.policy_generation,
                            p.evidence_bytes,
                            p.evicted_evidence_count,
                        ]
                        .into_iter()
                        .flatten()
                        .all(|n| n <= MAX_COUNTER)
                            && p.scope_id.as_ref().is_none_or(OpaqueId::validate)
                            && p.limiting_reason.as_deref().is_none_or(safe_label)
                    })
            }
            Event::ToolObserved {
                tool_name,
                stage,
                outcome,
            } => {
                self.identity.tool_id.is_some()
                    && (self.identity.session_id.is_some() || request)
                    && tool_name.as_deref().is_none_or(safe_label)
                    && (*stage == ToolStage::Terminal || *outcome == Outcome::Unknown)
            }
            Event::LegacyContext {
                estimated_before_tokens,
                estimated_after_tokens,
                ..
            } => {
                self.source.kind == SourceKind::LegacyStats
                    && *estimated_before_tokens <= MAX_COUNTER
                    && *estimated_after_tokens <= MAX_COUNTER
                    && self.identity == Identity::default()
            }
        };
        if !valid {
            bail!("data_invalid_observation");
        }
        if serde_json::to_vec(self)?.len() > MAX_EVENT_BYTES {
            bail!("data_event_limit");
        }
        Ok(())
    }
    pub fn kind(&self) -> &'static str {
        match self.event {
            Event::RequestStarted { .. } => "request_started",
            Event::RequestFinished { .. } => "request_finished",
            Event::ContextDecision { .. } => "context_decision",
            Event::ToolObserved { .. } => "tool_observed",
            Event::LegacyContext { .. } => "legacy_context",
        }
    }
    pub fn lifecycle_key(&self) -> Option<String> {
        match &self.event {
            Event::RequestStarted { .. } | Event::RequestFinished { .. } => self
                .identity
                .attempt_id
                .as_ref()
                .map(|id| format!("{}:{}:{}", self.source.id.0, id.0, self.kind())),
            Event::ToolObserved { stage, .. } => self.identity.tool_id.as_ref().map(|id| {
                format!(
                    "{}:{}:{}:{stage:?}",
                    self.source.id.0,
                    self.identity
                        .session_id
                        .as_ref()
                        .map(|s| s.0.as_str())
                        .unwrap_or(""),
                    id.0
                )
            }),
            _ => None,
        }
    }
}

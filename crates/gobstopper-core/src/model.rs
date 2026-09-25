use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Provider that owns the session transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// OpenAI Codex (`~/.codex/sessions/**/rollout-*.jsonl`).
    Codex,
    /// Anthropic Claude Code (`~/.claude/projects/*/*.jsonl`).
    ClaudeCode,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Codex => "codex",
            Provider::ClaudeCode => "claude_code",
        }
    }
}

/// A located session transcript on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHandle {
    pub provider: Provider,
    /// Provider session/thread id when recoverable, else the file stem.
    pub session_id: String,
    /// Canonical path to the transcript file.
    pub path: PathBuf,
    /// Working directory the session runs in, when recorded in the transcript.
    pub cwd: Option<PathBuf>,
    /// File modification age in seconds at scan time.
    pub age_secs: u64,
}

impl SessionHandle {
    /// A session written to within this window is probably still running.
    pub const HOT_SECS: u64 = 180;

    pub fn is_active(&self) -> bool {
        self.age_secs <= Self::HOT_SECS
    }
}

/// What an item is in the normalized transcript stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// System/developer instructions and environment preamble.
    System,
    /// User-authored message.
    User,
    /// Assistant-authored message.
    Assistant,
    /// Invocation of a tool/function by the assistant.
    ToolCall,
    /// Output returned by a tool/function.
    ToolResult,
    /// Model reasoning trace.
    Reasoning,
    /// Provider bookkeeping that does not reach the model context directly.
    Meta,
}

/// One normalized transcript entry. `line_index` is the adapter's rewrite
/// anchor: line-based stores elide in place so provider-internal linkage
/// (Claude `parentUuid` chains, Codex `ordinal` order) is never broken.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptItem {
    /// Index of the backing record within its store (JSONL line number).
    pub line_index: usize,
    pub kind: ItemKind,
    /// Approximate token contribution of this item.
    pub est_tokens: u64,
    /// Byte length of the elidable payload, if this item carries one.
    pub elidable_bytes: Option<u64>,
    #[serde(default = "default_elidable_parts")]
    pub elidable_parts: u32,
    /// Short sanitized label for plan output (never contains payload text).
    pub label: String,
    /// Optional short summary of the item's payload for digest generation.
    /// Never includes the full payload; at most a few hundred bytes.
    pub summary: Option<String>,
    /// Provider record uuid, when the format exposes one.
    pub uuid: Option<String>,
    /// Provider record parent uuid, when the format exposes one.
    pub parent_uuid: Option<String>,
    #[serde(default)]
    pub tool_use_ids: Vec<String>,
    #[serde(default)]
    pub payload_sha256: Option<String>,
}

const fn default_elidable_parts() -> u32 {
    1
}

/// Presence/provenance of the numeric context slot. A compaction reset clears
/// stale accounting; it is not evidence that the effective context is empty.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextState {
    #[default]
    Absent,
    Unknown,
    Reported,
    Reset,
}

/// Coverage of recorded input accounting, never a billing measurement. Full
/// means a provider cumulative report or the complete inspected source; Partial
/// means an additive tail/sample and cannot be exported as a session total.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeScope {
    #[default]
    Absent,
    Partial,
    Full,
}

/// A closed explanation for unavailable complete context accounting. These
/// values describe evidence, never provider content or a billing conclusion.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReason {
    MissingComponent,
    NullComponent,
    MalformedComponent,
    Overflow,
    InvalidAncestry,
    InvalidRecord,
    ReadLimit,
    SourceChanged,
    SourceUnavailable,
}

/// Metrics from one assistant message under the adapter's accounting dialect.
/// Unknown slots remain unknown; adapters retain their existing zero convention
/// for omitted optional counters, which does not extend to explicit null.
/// The sum of known slots is a measured-component subtotal,
/// not a claim about complete occupancy, a lower bound, or charged usage.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextComponents {
    pub input_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl ContextComponents {
    pub fn measured_subtotal(&self) -> Option<u64> {
        if self.input_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_creation_tokens.is_none()
            && self.output_tokens.is_none()
        {
            return None;
        }
        self.input_tokens
            .unwrap_or(0)
            .checked_add(self.cache_read_tokens.unwrap_or(0))?
            .checked_add(self.cache_creation_tokens.unwrap_or(0))?
            .checked_add(self.output_tokens.unwrap_or(0))
    }

    pub fn complete_total(&self) -> Option<u64> {
        self.input_tokens?
            .checked_add(self.cache_read_tokens?)?
            .checked_add(self.cache_creation_tokens?)?
            .checked_add(self.output_tokens?)
    }
}

/// Point-in-time token accounting extracted from provider records.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageSample {
    /// Estimated tokens the model currently carries in its context window.
    pub context_tokens: u64,
    /// Recorded input tokens in the scope below; not charged/billed usage.
    pub lifetime_input_tokens: u64,
    /// Portion of lifetime input served from prompt cache.
    pub lifetime_cached_tokens: u64,
    /// Provider-advertised context window for the active model, if known.
    pub model_context_window: Option<u64>,
    #[serde(default)]
    pub context_state: ContextState,
    #[serde(default)]
    pub lifetime_scope: LifetimeScope,
    /// Retained only for an incomplete component reading; never substitute this
    /// for `context_tokens` or use it to qualify a before/after reduction.
    #[serde(default)]
    pub context_components: Option<ContextComponents>,
    #[serde(default)]
    pub context_reason: Option<ContextReason>,
}

impl UsageSample {
    /// Absorb provider cumulative counters without double-counting or erasing
    /// absent fields. Explicit context zero is a reported zero; reset_context is
    /// a distinct provider compaction boundary. Cached lifetime input
    /// is normalized to its known input total; an unknown zero total grants no
    /// cached-token claim. Additive usage dialects must not use this operation.
    pub fn observe_cumulative_report(
        &mut self,
        context: Option<u64>,
        input: Option<u64>,
        cached: Option<u64>,
        window: Option<u64>,
    ) {
        if let Some(context) = context {
            self.context_tokens = context;
            self.context_state = ContextState::Reported;
            self.context_components = None;
            self.context_reason = None;
        }
        if let Some(input) = input {
            self.lifetime_input_tokens = input;
            self.lifetime_scope = LifetimeScope::Full;
        }
        if let Some(cached) = cached {
            self.lifetime_cached_tokens = cached;
        }
        self.lifetime_cached_tokens = self.lifetime_cached_tokens.min(self.lifetime_input_tokens);
        if let Some(window) = window.filter(|window| *window > 0) {
            self.model_context_window = Some(window);
        }
    }

    pub fn reset_context(&mut self) {
        self.context_tokens = 0;
        self.context_state = ContextState::Reset;
        self.context_components = None;
        self.context_reason = None;
    }

    pub fn invalidate_context(&mut self) {
        self.context_tokens = 0;
        self.context_state = ContextState::Unknown;
        self.context_components = None;
        self.context_reason = None;
    }

    pub fn invalidate_context_because(&mut self, reason: ContextReason) {
        self.invalidate_context();
        self.context_reason = Some(reason);
    }

    /// Only all present, representable components with no parser uncertainty
    /// establish complete accounting. A null or malformed slot is not zero.
    pub fn observe_context_components(
        &mut self,
        components: ContextComponents,
        reason: Option<ContextReason>,
    ) {
        if let Some(total) = components.complete_total().filter(|_| reason.is_none()) {
            self.context_tokens = total;
            self.context_state = ContextState::Reported;
            self.context_components = None;
            self.context_reason = None;
            return;
        }
        self.invalidate_context();
        self.context_components = Some(components);
        self.context_reason = Some(
            if components.measured_subtotal().is_none()
                && (components.input_tokens.is_some()
                    || components.cache_read_tokens.is_some()
                    || components.cache_creation_tokens.is_some()
                    || components.output_tokens.is_some())
            {
                ContextReason::Overflow
            } else {
                reason.unwrap_or(ContextReason::MissingComponent)
            },
        );
    }

    pub fn measured_component_subtotal(&self) -> Option<u64> {
        if self.context_state != ContextState::Unknown || self.context_reason.is_none() {
            return None;
        }
        self.context_components?.measured_subtotal()
    }

    pub fn reported_context(&self) -> Option<u64> {
        (self.context_state == ContextState::Reported
            && self.context_reason.is_none()
            && self.context_components.is_none())
        .then_some(self.context_tokens)
    }

    /// Best available context signal for ordering and gating watch work:
    /// provider-reported context, then a recorded preceding-token total,
    /// then a partial measured component subtotal. A hint is a bounded
    /// decision aid, not complete occupancy or a provider accounting claim.
    pub fn context_hint(&self) -> Option<u64> {
        self.reported_context()
            .or_else(|| {
                (self.context_state == ContextState::Unknown && self.context_tokens > 0)
                    .then_some(self.context_tokens)
            })
            .or_else(|| self.measured_component_subtotal())
    }
}

/// A parsed session transcript ready for strategy evaluation.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub session: SessionHandle,
    pub items: Vec<TranscriptItem>,
    pub usage: UsageSample,
}

impl TranscriptItem {
    /// Projection/lowering owns supported shape and branch authority. A live,
    /// positive payload is additionally required by every core edit planner.
    pub fn is_elidable(&self) -> bool {
        crate::admission::is_elidable(self.est_tokens, self.elidable_bytes)
    }

    pub fn estimated_elision_savings(&self) -> u64 {
        crate::estimate::elision_savings(self.est_tokens, self.elidable_bytes, self.elidable_parts)
    }
}

impl Transcript {
    pub fn estimated_context_tokens(&self) -> u64 {
        self.items.iter().fold(0u64, |total, item| {
            crate::estimate::add_tokens(total, item.est_tokens)
        })
    }

    /// Estimated context occupancy: prefer the provider's own accounting,
    /// fall back to summing item estimates.
    pub fn context_tokens(&self) -> u64 {
        self.usage
            .reported_context()
            .unwrap_or_else(|| self.estimated_context_tokens())
    }

    /// Estimated tokens recoverable by eliding every elidable item.
    pub fn elidable_tokens(&self) -> u64 {
        self.items
            .iter()
            .filter(|item| item.is_elidable())
            .filter_map(|item| item.elidable_bytes)
            .map(crate::estimate::estimate_token_bytes)
            .fold(0u64, crate::estimate::add_tokens)
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;

    #[test]
    fn absent_legacy_unknown_reported_zero_and_reset_are_distinct() {
        let mut usage: UsageSample = serde_json::from_str(r#"{"context_tokens":0,"lifetime_input_tokens":0,"lifetime_cached_tokens":0,"model_context_window":null}"#).unwrap();
        assert_eq!(usage.context_state, ContextState::Absent);
        assert_eq!(usage.lifetime_scope, LifetimeScope::Absent);
        assert_eq!(usage.reported_context(), None);
        usage.observe_cumulative_report(Some(0), Some(0), Some(0), None);
        assert_eq!(usage.reported_context(), Some(0));
        assert_eq!(usage.lifetime_scope, LifetimeScope::Full);
        usage.reset_context();
        assert_eq!(usage.context_state, ContextState::Reset);
        assert_eq!(usage.reported_context(), None);
        usage.invalidate_context();
        assert_eq!(usage.context_state, ContextState::Unknown);
        assert_eq!(usage.lifetime_scope, LifetimeScope::Full);
    }

    #[test]
    fn cumulative_reset_missing_fields_and_inconsistent_cache_are_bounded() {
        let mut usage = UsageSample {
            context_tokens: u64::MAX,
            lifetime_input_tokens: 100,
            lifetime_cached_tokens: 90,
            model_context_window: Some(200_000),
            context_state: ContextState::Reported,
            lifetime_scope: LifetimeScope::Full,
            ..Default::default()
        };
        usage.observe_cumulative_report(None, None, None, Some(0));
        assert_eq!(usage.context_tokens, u64::MAX);
        assert_eq!(usage.model_context_window, Some(200_000));
        usage.observe_cumulative_report(Some(0), Some(10), Some(u64::MAX), None);
        assert_eq!(usage.context_tokens, 0);
        assert_eq!(usage.lifetime_input_tokens, 10);
        assert_eq!(usage.lifetime_cached_tokens, 10);
        let first = usage;
        usage.observe_cumulative_report(Some(0), Some(10), Some(u64::MAX), None);
        assert_eq!(usage, first);
        usage.observe_cumulative_report(None, Some(0), None, None);
        assert_eq!(usage.lifetime_cached_tokens, 0);
    }

    #[test]
    fn partial_components_never_claim_complete_context_and_do_not_survive_reset() {
        let components = ContextComponents {
            input_tokens: Some(100),
            cache_read_tokens: Some(900),
            cache_creation_tokens: None,
            output_tokens: Some(5),
        };
        let mut usage = UsageSample::default();
        usage.observe_context_components(components, Some(ContextReason::NullComponent));
        assert_eq!(usage.context_state, ContextState::Unknown);
        assert_eq!(usage.context_tokens, 0);
        assert_eq!(usage.reported_context(), None);
        assert_eq!(usage.measured_component_subtotal(), Some(1005));
        assert_eq!(usage.context_components, Some(components));
        assert_eq!(usage.context_reason, Some(ContextReason::NullComponent));
        let mut inconsistent = usage;
        inconsistent.context_state = ContextState::Reported;
        assert_eq!(inconsistent.reported_context(), None);
        inconsistent.context_reason = None;
        assert_eq!(inconsistent.reported_context(), None);
        usage.reset_context();
        assert_eq!(usage.context_components, None);
        assert_eq!(usage.context_reason, None);
        assert_eq!(usage.measured_component_subtotal(), None);
        usage.observe_context_components(components, None);
        assert_eq!(usage.context_reason, Some(ContextReason::MissingComponent));
        usage.invalidate_context();
        assert_eq!(usage.context_components, None);
        assert_eq!(usage.context_reason, None);
    }

    #[test]
    fn component_zero_absence_and_overflow_have_distinct_meanings() {
        let mut usage = UsageSample::default();
        usage.observe_context_components(ContextComponents::default(), None);
        assert_eq!(usage.reported_context(), None);
        assert_eq!(usage.measured_component_subtotal(), None);
        let zero = ContextComponents {
            input_tokens: Some(0),
            cache_read_tokens: Some(0),
            cache_creation_tokens: Some(0),
            output_tokens: Some(0),
        };
        usage.observe_context_components(zero, None);
        assert_eq!(usage.reported_context(), Some(0));
        assert_eq!(usage.context_components, None);
        assert_eq!(usage.context_reason, None);
        usage.observe_context_components(
            ContextComponents {
                input_tokens: Some(u64::MAX),
                output_tokens: Some(1),
                ..zero
            },
            None,
        );
        assert_eq!(usage.reported_context(), None);
        assert_eq!(usage.measured_component_subtotal(), None);
        assert_eq!(usage.context_reason, Some(ContextReason::Overflow));
        usage.observe_cumulative_report(Some(0), None, None, None);
        assert_eq!(usage.reported_context(), Some(0));
        assert_eq!(usage.context_components, None);
        assert_eq!(usage.context_reason, None);
    }

    #[test]
    fn context_hint_prefers_reported_then_preceding_then_component_subtotal() {
        let mut usage = UsageSample::default();
        assert_eq!(usage.context_hint(), None);
        // Partial components give a measured subtotal hint.
        usage.observe_context_components(
            ContextComponents {
                input_tokens: Some(100),
                cache_read_tokens: Some(900),
                cache_creation_tokens: None,
                output_tokens: Some(5),
            },
            Some(ContextReason::NullComponent),
        );
        assert_eq!(usage.context_hint(), Some(1005));
        // A recorded preceding-token total beats a partial subtotal.
        usage.context_tokens = 42_000;
        assert_eq!(usage.context_hint(), Some(42_000));
        // Reported context always wins.
        usage.observe_cumulative_report(Some(170_000), None, None, None);
        assert_eq!(usage.context_hint(), Some(170_000));
        // An invalidated sample has no hint at all.
        usage.invalidate_context();
        assert_eq!(usage.context_hint(), None);
        // A reset session reports a real zero, which is a real hint.
        usage.reset_context();
        assert_eq!(usage.context_hint(), None);
        usage.observe_cumulative_report(Some(0), None, None, None);
        assert_eq!(usage.context_hint(), Some(0));
    }

    #[test]
    fn full_byte_domain_savings_and_totals_saturate() {
        let item = TranscriptItem {
            line_index: usize::MAX,
            kind: ItemKind::ToolResult,
            est_tokens: u64::MAX,
            elidable_bytes: Some(u64::MAX),
            elidable_parts: u32::MAX,
            label: String::new(),
            summary: None,
            uuid: None,
            parent_uuid: None,
            tool_use_ids: Vec::new(),
            payload_sha256: None,
        };
        assert!(item.estimated_elision_savings() > 0);
        let transcript = Transcript {
            session: SessionHandle {
                provider: Provider::Codex,
                session_id: "test".into(),
                path: Default::default(),
                cwd: None,
                age_secs: u64::MAX,
            },
            items: vec![item; 8],
            usage: UsageSample::default(),
        };
        assert_eq!(transcript.estimated_context_tokens(), u64::MAX);
        assert_eq!(transcript.elidable_tokens(), u64::MAX);
        let mut tiny = transcript.items[0].clone();
        tiny.est_tokens = 2;
        assert_eq!(tiny.estimated_elision_savings(), 2);
        tiny.est_tokens = 0;
        assert!(!tiny.is_elidable());
        assert_eq!(tiny.estimated_elision_savings(), 0);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn cumulative_usage_preserves_absence_and_bounds_cache() {
        let mut usage = UsageSample {
            context_tokens: kani::any(),
            lifetime_input_tokens: kani::any(),
            lifetime_cached_tokens: kani::any(),
            model_context_window: kani::any(),
            context_state: match kani::any::<u8>() % 4 {
                0 => ContextState::Absent,
                1 => ContextState::Unknown,
                2 => ContextState::Reported,
                _ => ContextState::Reset,
            },
            lifetime_scope: match kani::any::<u8>() % 3 {
                0 => LifetimeScope::Absent,
                1 => LifetimeScope::Partial,
                _ => LifetimeScope::Full,
            },
            ..Default::default()
        };
        let before = usage;
        let context: Option<u64> = kani::any();
        let input: Option<u64> = kani::any();
        let cached: Option<u64> = kani::any();
        let window: Option<u64> = kani::any();
        usage.observe_cumulative_report(context, input, cached, window);
        let input_expected = input.unwrap_or(before.lifetime_input_tokens);
        let cached_reported = cached.unwrap_or(before.lifetime_cached_tokens);
        assert_eq!(
            usage.context_tokens,
            context.unwrap_or(before.context_tokens)
        );
        assert_eq!(usage.lifetime_input_tokens, input_expected);
        assert_eq!(
            usage.lifetime_cached_tokens,
            if cached_reported > input_expected {
                input_expected
            } else {
                cached_reported
            }
        );
        assert!(
            usage.lifetime_cached_tokens <= usage.lifetime_input_tokens,
            "cached cumulative usage is bounded by known input"
        );
        let expected_window = match window {
            Some(1..=u64::MAX) => window,
            _ => before.model_context_window,
        };
        assert_eq!(usage.model_context_window, expected_window);
        assert_eq!(
            usage.context_state,
            if context.is_some() {
                ContextState::Reported
            } else {
                before.context_state
            },
            "context report presence preserves or establishes provenance"
        );
        assert_eq!(
            usage.lifetime_scope,
            if input.is_some() {
                LifetimeScope::Full
            } else {
                before.lifetime_scope
            },
            "only cumulative input establishes full lifetime scope"
        );
        assert_eq!(
            usage.reported_context(),
            if usage.context_state == ContextState::Reported {
                Some(usage.context_tokens)
            } else {
                None
            },
            "only reported context is a measured value including zero"
        );
        let first = usage;
        usage.observe_cumulative_report(context, input, cached, window);
        assert_eq!(
            usage, first,
            "replaying a cumulative report does not double count"
        );
        kani::cover!(
            context == Some(0) && before.context_tokens == u64::MAX,
            "reported zero clears stale numeric context"
        );
        kani::cover!(
            input == Some(0) && cached == Some(u64::MAX),
            "inconsistent cached report clamped"
        );
        kani::cover!(
            context.is_none() && before.context_tokens == u64::MAX,
            "missing context preserves full width value"
        );
        kani::cover!(
            context.is_none() && before.context_state == ContextState::Absent,
            "absent context remains absent"
        );
        kani::cover!(
            context.is_none() && before.context_state == ContextState::Unknown,
            "unknown context remains unknown"
        );
        kani::cover!(
            context.is_none() && before.context_state == ContextState::Reported,
            "missing report preserves previously reported context"
        );
        kani::cover!(
            context == Some(0) && before.context_state == ContextState::Reset,
            "reported zero is distinct from an explicit reset"
        );
        kani::cover!(
            input == Some(0) && before.lifetime_scope == LifetimeScope::Partial,
            "cumulative zero establishes full lifetime accounting"
        );
        kani::cover!(
            input.is_none() && before.lifetime_scope == LifetimeScope::Absent,
            "absent cumulative input stays absent"
        );
        kani::cover!(
            input.is_none() && before.lifetime_scope == LifetimeScope::Full,
            "missing cumulative input preserves full scope"
        );
        let mut reset = before;
        reset.reset_context();
        assert_eq!(reset.context_tokens, 0);
        assert_eq!(reset.context_state, ContextState::Reset);
        assert_eq!(
            reset.reported_context(),
            None,
            "explicit reset is not measured empty context"
        );
        assert_eq!(reset.lifetime_input_tokens, before.lifetime_input_tokens);
        assert_eq!(reset.lifetime_cached_tokens, before.lifetime_cached_tokens);
        assert_eq!(reset.lifetime_scope, before.lifetime_scope);
        assert_eq!(reset.model_context_window, before.model_context_window);
        let mut unknown = before;
        unknown.invalidate_context();
        assert_eq!(unknown.context_tokens, 0);
        assert_eq!(unknown.context_state, ContextState::Unknown);
        assert_eq!(
            unknown.reported_context(),
            None,
            "invalidated context is unavailable rather than measured empty"
        );
        assert_eq!(unknown.lifetime_input_tokens, before.lifetime_input_tokens);
        assert_eq!(
            unknown.lifetime_cached_tokens,
            before.lifetime_cached_tokens
        );
        assert_eq!(unknown.lifetime_scope, before.lifetime_scope);
        assert_eq!(unknown.model_context_window, before.model_context_window);

        // This exercises the production component normalizer over full-width
        // values, including unknown slots and arithmetic overflow. It is not a
        // proof of a provider's additive metric contract.
        let components = ContextComponents {
            input_tokens: kani::any(),
            cache_read_tokens: kani::any(),
            cache_creation_tokens: kani::any(),
            output_tokens: kani::any(),
        };
        let explicit_unknown: bool = kani::any();
        let mut partial = before;
        partial.observe_context_components(
            components,
            explicit_unknown.then_some(ContextReason::NullComponent),
        );
        let complete = components
            .input_tokens
            .zip(components.cache_read_tokens)
            .zip(components.cache_creation_tokens)
            .zip(components.output_tokens)
            .and_then(|(((input, read), creation), output)| {
                input
                    .checked_add(read)?
                    .checked_add(creation)?
                    .checked_add(output)
            });
        assert_eq!(
            partial.reported_context(),
            complete.filter(|_| !explicit_unknown),
            "partial component evidence is never complete context"
        );
        assert_eq!(partial.lifetime_input_tokens, before.lifetime_input_tokens);
        assert_eq!(
            partial.lifetime_cached_tokens,
            before.lifetime_cached_tokens
        );
        assert_eq!(partial.lifetime_scope, before.lifetime_scope);
        assert_eq!(partial.model_context_window, before.model_context_window);
        if partial.reported_context().is_none() {
            assert_eq!(partial.context_tokens, 0);
            assert!(partial.context_reason.is_some());
            assert_eq!(partial.context_components, Some(components));
        } else {
            assert_eq!(partial.context_reason, None);
            assert_eq!(partial.context_components, None);
        }
        kani::cover!(
            components.input_tokens == Some(7)
                && components.cache_creation_tokens.is_none()
                && partial.measured_component_subtotal() == Some(7),
            "known components survive unknown cache creation"
        );
        kani::cover!(
            components.input_tokens == Some(u64::MAX)
                && components.output_tokens == Some(1)
                && partial.context_reason == Some(ContextReason::Overflow),
            "component overflow is unavailable"
        );
        kani::cover!(
            partial.reported_context() == Some(0),
            "complete component zero remains measured zero"
        );
        partial.reset_context();
        assert_eq!(partial.measured_component_subtotal(), None);
        assert_eq!(partial.context_components, None);
        assert_eq!(partial.context_reason, None);
    }
}

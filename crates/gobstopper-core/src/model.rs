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
    /// Cognition Devin (`$DEVIN_DATA_DIR/sessions.db`, default
    /// `~/.local/share/devin/cli/sessions.db`). The store is SQLite;
    /// `SessionHandle::path` carries the database path and `session_id`
    /// selects the row set.
    Devin,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Codex => "codex",
            Provider::ClaudeCode => "claude_code",
            Provider::Devin => "devin",
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
    }

    pub fn invalidate_context(&mut self) {
        self.context_tokens = 0;
        self.context_state = ContextState::Unknown;
    }

    pub fn reported_context(&self) -> Option<u64> {
        (self.context_state == ContextState::Reported).then_some(self.context_tokens)
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
    }
}

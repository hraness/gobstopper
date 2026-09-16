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
    /// Short sanitized label for plan output (never contains payload text).
    pub label: String,
}

/// Point-in-time token accounting extracted from provider records.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UsageSample {
    /// Estimated tokens the model currently carries in its context window.
    pub context_tokens: u64,
    /// Cumulative input tokens charged to this session so far.
    pub lifetime_input_tokens: u64,
    /// Portion of lifetime input served from prompt cache.
    pub lifetime_cached_tokens: u64,
    /// Provider-advertised context window for the active model, if known.
    pub model_context_window: Option<u64>,
}

/// A parsed session transcript ready for strategy evaluation.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub session: SessionHandle,
    pub items: Vec<TranscriptItem>,
    pub usage: UsageSample,
}

impl Transcript {
    /// Estimated context occupancy: prefer the provider's own accounting,
    /// fall back to summing item estimates.
    pub fn context_tokens(&self) -> u64 {
        if self.usage.context_tokens > 0 {
            self.usage.context_tokens
        } else {
            self.items.iter().map(|i| i.est_tokens).sum()
        }
    }

    /// Estimated tokens recoverable by eliding every elidable item.
    pub fn elidable_tokens(&self) -> u64 {
        self.items
            .iter()
            .filter(|i| i.elidable_bytes.is_some())
            .map(|i| i.est_tokens)
            .sum()
    }
}

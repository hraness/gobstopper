//! Compaction telemetry: numeric-only records appended to a local JSONL
//! log (schema `gobstopper/compaction-events-v1`) for downstream
//! consumers such as aicharts and oompa.
//!
//! Hard rule: these records carry counts, durations, closed enum-like
//! strings, and identifiers only. Transcript content, file paths, and
//! raw provider messages are FORBIDDEN in these records.

use crate::Provider;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One compaction telemetry record — the unit written to `events.jsonl`.
///
/// Numeric-only by design; see the module docs for what must never
/// appear here. `action`, `outcome`, and `error_code` are closed vocab
/// strings, not freeform text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    /// Always `gobstopper/compaction-events-v1`.
    pub schema: String,
    /// Unix seconds when the event was recorded.
    pub ts: u64,
    /// Provider that owns the session.
    pub provider: crate::Provider,
    /// Session identifier — callers pass a real id locally or a
    /// keyed/pseudonymous digest for telemetry export.
    pub session_id: String,
    /// Strategy id that produced the plan (e.g. "auto", "elide").
    pub strategy: String,
    /// "provider_compact" | "transcript_compact" | "none"
    pub action: String,
    /// "applied" | "planned" | "failed" | "skipped"
    pub outcome: String,
    /// Effective trigger the decision was evaluated against.
    pub trigger_tokens: u64,
    /// Context estimate before the compaction.
    pub context_tokens_before: u64,
    /// Context estimate after the compaction.
    pub context_tokens_after: u64,
    /// `context_tokens_before - context_tokens_after`, saturating.
    pub est_reclaimed_tokens: u64,
    /// Transcript items covered by elision/digest.
    pub items_covered: u64,
    /// Wall time of the evaluate+apply pass.
    pub duration_ms: u64,
    /// Closed error code on failure (e.g. "io", "provider_rejected") —
    /// never a freeform message.
    pub error_code: Option<String>,
}

impl CompactionEvent {
    /// Schema tag written into every record.
    pub const SCHEMA: &'static str = "gobstopper/compaction-events-v1";

    /// Build an event for one compaction pass. `schema` and `ts` are
    /// filled automatically; `est_reclaimed_tokens` is derived from the
    /// before/after context estimates.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Provider,
        session_id: impl Into<String>,
        strategy: impl Into<String>,
        action: impl Into<String>,
        outcome: impl Into<String>,
        trigger_tokens: u64,
        context_tokens_before: u64,
        context_tokens_after: u64,
        items_covered: u64,
        duration_ms: u64,
        error_code: Option<String>,
    ) -> Self {
        Self {
            schema: Self::SCHEMA.to_string(),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            provider,
            session_id: session_id.into(),
            strategy: strategy.into(),
            action: action.into(),
            outcome: outcome.into(),
            trigger_tokens,
            context_tokens_before,
            context_tokens_after,
            est_reclaimed_tokens: context_tokens_before.saturating_sub(context_tokens_after),
            items_covered,
            duration_ms,
            error_code,
        }
    }
}

/// Append one event as a JSONL line, creating parent dirs as needed.
pub fn append_event(log_path: &Path, event: &CompactionEvent) -> std::io::Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    file.write_all(line.as_bytes())
}

/// Default telemetry log: `$XDG_DATA_HOME/gobstopper/events.jsonl`,
/// falling back to `~/.local/share/gobstopper/events.jsonl`.
pub fn default_log_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gobstopper").join("events.jsonl")
}

/// Read every event in the log. Blank and unparseable lines are skipped
/// so one torn write does not lose the whole history.
pub fn read_events(log_path: &Path) -> std::io::Result<Vec<CompactionEvent>> {
    let text = std::fs::read_to_string(log_path)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_json_round_trip() {
        let event = CompactionEvent::new(
            Provider::Codex,
            "sess-1",
            "elide",
            "transcript_compact",
            "applied",
            250_000,
            260_000,
            40_000,
            12,
            42,
            None,
        );
        assert_eq!(event.schema, CompactionEvent::SCHEMA);
        assert!(event.ts > 0);
        assert_eq!(event.est_reclaimed_tokens, 220_000);

        let json = serde_json::to_string(&event).unwrap();
        let back: CompactionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema, "gobstopper/compaction-events-v1");
        assert_eq!(back.provider, Provider::Codex);
        assert_eq!(back.session_id, "sess-1");
        assert_eq!(back.strategy, "elide");
        assert_eq!(back.action, "transcript_compact");
        assert_eq!(back.outcome, "applied");
        assert_eq!(back.trigger_tokens, 250_000);
        assert_eq!(back.context_tokens_before, 260_000);
        assert_eq!(back.context_tokens_after, 40_000);
        assert_eq!(back.est_reclaimed_tokens, 220_000);
        assert_eq!(back.items_covered, 12);
        assert_eq!(back.duration_ms, 42);
        assert_eq!(back.error_code, None);

        // Provider serializes snake_case like the rest of the model.
        assert!(json.contains("\"provider\":\"codex\""));

        let failed = CompactionEvent::new(
            Provider::ClaudeCode,
            "sess-2",
            "sawtooth",
            "provider_compact",
            "failed",
            175_000,
            180_000,
            180_000,
            0,
            5,
            Some("provider_rejected".to_string()),
        );
        let back: CompactionEvent =
            serde_json::from_str(&serde_json::to_string(&failed).unwrap()).unwrap();
        assert_eq!(back.error_code.as_deref(), Some("provider_rejected"));
        assert_eq!(back.provider, Provider::ClaudeCode);
        // Saturating: after > before yields zero reclaimed, not underflow.
        assert_eq!(back.est_reclaimed_tokens, 0);
    }

    #[test]
    fn append_and_read_events_jsonl() {
        let unique = format!(
            "gobstopper-events-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(unique);
        // Nested path proves append_event creates parent dirs.
        let log = dir.join("nested").join("events.jsonl");

        let e1 = CompactionEvent::new(
            Provider::Codex,
            "a",
            "elide",
            "transcript_compact",
            "applied",
            1_000,
            1_200,
            300,
            4,
            10,
            None,
        );
        let e2 = CompactionEvent::new(
            Provider::ClaudeCode,
            "b",
            "sawtooth",
            "provider_compact",
            "skipped",
            1_000,
            900,
            900,
            0,
            0,
            None,
        );
        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        // A torn write / foreign line is skipped, not fatal.
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"this is not json\n\n").unwrap();
        drop(f);

        let events = read_events(&log).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].session_id, "a");
        assert_eq!(events[1].session_id, "b");
        assert_eq!(events[1].outcome, "skipped");

        std::fs::remove_dir_all(&dir).ok();
    }
}

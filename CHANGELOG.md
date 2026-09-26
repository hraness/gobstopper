# Changelog

Each version section is the text of that version's release page: a summary paragraph, then one bullet per change. [`docs/release.md`](docs/release.md) describes how a release page is built from it.

## Unreleased

The request proxy keeps more of the agent's recent work after each compaction, and Claude Code requests that use a 1M-token window get their own threshold.

- `proxy serve`, `proxy run` and `proxy replay` keep a recent tail sized by `--keep-tail-percent` (default 40, from 0 to 60) of the room below the threshold, instead of only the last `--keep-recent` turns. `--keep-tail-percent 0` keeps the previous tail.
- Anthropic requests whose `anthropic-beta` header lists a `context-1m` token use `--threshold-1m`: 256,000 estimated tokens by default, or `--threshold` if higher. Setting it equal to `--threshold` turns the split off.
- Consecutive assistant messages count as one turn in every dialect, so a compaction no longer separates tool results from their calls.
- `proxy replay` reads subagent transcripts and reports head, summary and tail sizes per compaction, repeated reads, and the gap between compactions.
- `proxy status` shows `threshold_1m_tokens`, `keep_tail_percent` and `requests_1m` when the server reports them.
- Each row of the proxy's stats file records `threshold_tokens`, the threshold applied to that request. A request over the configured threshold that is sent unchanged now gets a log line saying why: a large verbatim head raised the threshold, or there was nothing to compact.
- `scripts/monitor.py --proxy PORT` runs `proxy status --json` on each pass and fails the pass when the proxy does not answer. It is off by default.

## v0.4.1 - 2026-09-25

`watch` and `report` now skip sessions that have been idle longer than a window you set, and the monitor script can follow every active session of a provider without a session list.

- `watch` and `report` share one discovery window, `[discovery] max_age_secs` in `~/.config/gobstopper/config.toml` (default 7 days, `0` for every session). Idle sessions older than the window are skipped at discovery instead of being read and dropped later.
- `watch --max-age SECS`, `report --max-age SECS` and `report --all` override the window for one run. `--active-only` still selects sessions active in the last 180 seconds.
- Locked Devin sessions count as live whatever their age, so running work stays covered.
- `scripts/monitor.py --provider codex` and `--provider claude_code` cover every active session of that provider, so new sessions are followed without a `--session` list.

# Contents

- `crates/gobstopper-core/` holds the normalized transcript model, the `Edit` IR, the `Strategy` trait, and all built-in strategies. No I/O.
- `crates/gobstopper-adapters/` holds session discovery and the Codex and Claude Code JSONL dialects (parse and in-place rewrite).
- `crates/gobstopper-cli/` holds the `gobstopper` binary and layered config/preset resolution.
- `docs/design.md` is the architecture and research record.

# Guidelines

- Strategies are pure: transcript in, plan out. Execution lives in adapters.
- Rewrites replace payload content in place; provider record order and linkage (`parentUuid`, `ordinal`) are never disturbed.
- Never emit transcript content (prompts, tool output, paths beyond what the provider record carries) into stdout, logs, or digests unless the user asked for that field.
- `detect`/`policy-check`/`plan --json` are the stable machine surfaces; keep their fields additive-only.
- The `auto` strategy must always prefer provider delegation for live sessions; file surgery is for idle transcripts.
- Keep dependency count small; prefer `std` + `serde_json` over new crates.

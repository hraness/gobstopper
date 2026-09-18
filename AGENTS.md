# Contents

- `crates/gobstopper-core/` holds the normalized transcript model, the `Edit` IR, the `Strategy` trait, all built-in strategies, and the `compaction-events-v1` telemetry schema. No I/O beyond event-log append.
- `crates/gobstopper-adapters/` holds session discovery, the Codex and Claude Code JSONL dialects (parse and in-place rewrite), the `verify` resume-validity checker, and the `vault` content-addressed snapshot store.
- `crates/gobstopper-cli/` holds the `gobstopper` binary, layered config/preset resolution, and the read-only `mcp` stdio server (`mcp.rs`) that exposes vault/recall/plan/verify as agent tools — it must never surface a mutating operation.
- `docs/design.md` is the architecture and research record; `docs/roadmap.md` is the phased plan; `docs/oompa-contract.md` is the oompa integration contract.

# Guidelines

- Strategies are pure: transcript in, plan out. Execution lives in adapters.
- Rewrites replace payload content in place; provider record order and linkage (`parentUuid`, `ordinal`) are never disturbed.
- Never emit transcript content (prompts, tool output, paths beyond what the provider record carries) into stdout, logs, or digests unless the user asked for that field.
- `detect`/`policy-check`/`plan --json`/`verify --json`/`vault --json` are the stable machine surfaces; keep their fields additive-only.
- Every mutating path (`apply`, `watch`) snapshots into the vault before writing and emits a `compaction-events-v1` record after; telemetry failures are non-fatal, snapshot failures abort the edit.
- The `auto` strategy must always prefer provider delegation for live sessions; file surgery is for idle transcripts.
- Keep dependency count small; prefer `std` + `serde_json` over new crates.

# Local development and install

For fast iteration, build and install the release binary once instead of running `cargo run` each time:

```bash
cargo build --release
cargo install --path crates/gobstopper-cli --locked
```

`~/.cargo/bin/gobstopper` is then on `$PATH` after a shell restart and `gobstopper --version` reflects the current checkout.

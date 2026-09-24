# Reproducible verification

The checks cover different boundaries: [production kernels](core/README.md),
[transcript algebra and finite Rust correspondence](transcript/README.md),
[vault publication](vault/README.md) and [native dispatch](watch/README.md).
The [bounded synthetic stress suite](stress/README.md) exercises named fault,
restart, parser and process fixtures with explicit resource and evidence limits.
The [assurance ledger](../docs/assurance/README.md) records their claims and exclusions.

`tools.lock.json` pins official Java, TLC, Kani and Lean release archives for
Linux x86_64 and macOS arm64. `python3 verify/tools.py <tool> --output <new-file>`
downloads one archive into an existing directory, verifies its SHA-256 and publishes
it without overwriting. It never extracts or executes the download. The selected
platform must have a reviewed pin; a new release is not adopted automatically.

Downloads have a 240-second monotonic copy deadline, a 20-second socket idle timeout
and a per-artifact size bound no larger than 1 GiB. `read1` returns after one underlying
read, allowing the total deadline to be checked even when a peer trickles bytes.
DNS resolution and one blocking read can add overhead; the CI job timeout is the
outer process deadline. HTTPS release redirects are allowed only to HTTPS, and the
expected archive hash remains authoritative. Parent directories are trusted.

CI runs all proof checkers with fresh output directories and retains successful and
failed logs/receipts for 30 days. Every proof runner records exact input/tool digests,
requires input stability and rejects incomplete output or unrelated mutant failures.
Checksums identify bytes; they do not prove that compilers, solvers, kernels, release
publishers or dependency registries are correct. `cargo --locked` binds the root
dependency graph; dependency build scripts can create separate workspaces outside
that lockfile. The [correspondence setup](transcript/README.md#gate-and-negative-controls)
records the current Hegel engine bootstrap and its remaining provenance limit.
CI separately installs the pinned toolchain and verified proof bundle.

The `Required` check joins six gates: quality and regression tests, MSRV 1.85.0,
TLA+ protocol models, Kani production kernels, Lean laws with finite Rust
correspondence, and bounded synthetic stress. They run for pull requests,
pushes to `main`, manual dispatch and a weekly Monday run at 07:19 UTC.
The site has its own `bun run check` gate. Passing these checks admits the
tested artifact; it does not establish live provider ownership, resume
compatibility, semantic retention or billing savings. The
[activation matrix](../docs/assurance/qualification.json) keeps those separate
qualification obligations explicit.

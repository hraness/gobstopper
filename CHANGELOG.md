# Changelog

Each version section is the text of that version's release page: a summary paragraph, then one bullet per change. [`docs/release.md`](docs/release.md) describes how a release page is built from it.

## v0.8.5 - 2026-10-01

A context scope can carry its own standing input threshold, so a long-lived agent runs wider than the proxy default without re-arming a reservation.

- `gobstopper context create --threshold <tokens>` sets a standing threshold for the scope. Scoped requests compact at it instead of `--threshold` or `--threshold-1m`, clipped to the declared input capacity; a live reservation still takes priority, and release returns to the scope's threshold. `context status` reports it as `scope_threshold_tokens` and the limiting reason as `scope_threshold`. Existing context stores gain the column on first open.

## v0.8.4 - 2026-10-01

Gobstopper keeps slow clients, optional storage and compaction work from holding up proxy controls. A new launcher checks the proxy before starting Claude Code or Codex. Claude can use its direct provider when the proxy is unavailable; Codex keeps its existing configuration and requires a healthy proxy.

- Request headers and bodies have time and memory limits. Proxy readiness and status use short deadlines, and compaction uses a separate worker pool with a forwarding deadline. Expired compaction work cannot publish a cached prefix or a context observation.
- Early request rejections send their response before closing, with bounded cleanup of unread uploads, so an overload response is less likely to be lost to a connection reset.
- Statistics and diagnostic logs write through bounded background queues and report dropped entries or write failures. Context storage opens when a scoped request needs it, retries after temporary failure and preserves existing reservations.
- Sleep inhibition runs in a background worker, so slow native power calls cannot block request forwarding or status.
- `gobstopper proxy launch` checks readiness before starting a client. Claude direct fallback preserves authentication and refuses routing it cannot safely reproduce. Codex launches only with an existing explicit custom proxy provider and a healthy proxy; direct fallback remains unavailable because effective cloud configuration cannot be fully verified. It does not replay a request already sent to a provider.
- Service recovery documentation records the one-time legacy migration filter outage and its cleanup. Future updates use the managed drain protocol without firewall changes.

## v0.8.3 - 2026-09-30

Gobstopper retries temporary database locking failures when several commands start at once.

- Concurrent session database startup retries temporary SQLite lock failures, including the lock-protocol race observed on Windows, while preserving stored observations and session identities.

## v0.8.2 - 2026-09-30

Gobstopper's macOS and Linux release installations update automatically before a command when a newer verified release is available.

- `gobstopper update` installs a newer release. `update check`, `status`, `enable`, and `disable` inspect or change update behavior. Supported native installations check at most once a day by default; CI, MCP, offline replay, read-only inspection and pinned versions skip automatic checks.
- Updates verify the canonical immutable release, archive hashes, archive contents, executable identity and Mac signature, then replace the executable and preserve the previous copy if installation fails. Running commands prevent replacement for their entire lifetime, including proxies and MCP servers.
- The public installer can upgrade a verified 0.7.5, 0.8.0 or 0.8.1 copy into an installation that updates itself. Cargo, source builds, unknown copies and Windows keep their existing update workflow. `--no-update` or `HRANESS_NO_UPDATE=1` skips an automatic check for one invocation.

## v0.8.1 - 2026-09-30

Managed service changes can wait for inference to finish while new requests receive a retry response. A timed wait reopens requests if its owner stops responding; once the service change begins, recovery checks its outcome before reopening requests.

- Service changes use an owned, renewable wait with a separate final stop check. A stale owner cannot complete or cancel a newer wait.
- The proxy keeps existing inference, provider retries and response forwarding active while waiting for them to finish. A timeout cancels the wait instead of stopping busy inference.
- Interrupted service changes record their progress. An uncertain service-manager result keeps requests paused until recovery can safely reconcile it.
- New service definitions prevent automatic restarts from accepting requests while an earlier stop remains unresolved. Older definitions retain idle-only upgrades until replaced.

## v0.8.0 - 2026-09-30

Gobstopper can reserve a larger context for difficult work, carry original observations across repeated compactions, and record local session metrics. User services restart it after failure and request idle-sleep prevention during active inference.

- Temporary, capability-scoped context reservations have request counts, expiry, provider/client capacity limits and output headroom. Optional adaptive rescue responds to repeated unchanged evidence reads after eviction; it does not treat polling alone as a loop.
- Bounded evidence carry retains selected original tool results and images with their invocation, identifies excerpts, and reconstructs from the original history after restart. Responses freeform tool input and semantic image/tool content now participate in summary and cache identity.
- A versioned local SQLite event store separates requests, provider attempts, usage, context decisions and observed tool calls. Pure metric projections preserve unknowns and explain token-rate denominators. JSONL interchange, duplicate-safe import, backup, schema migration and integrity checks support local analysis without a cloud account.
- Startup uses an owned macOS LaunchAgent, Linux systemd user service or Windows user task. Identity checks, readiness probes, rollback, diagnosis, repair and idle drain protect replacements. Legacy Mac service migration is explicit.
- Inference activity holds a native idle-sleep assertion and releases it after the last request or process exit. Unsupported or unavailable platform integration is visible in status.
- The release workflow signs and notarizes the Apple silicon binary after the exact source passes CI. Compilation runs without Apple credentials, and temporary signing credentials are removed before testing the final installer.
- The Mac installer verifies the expected Apple signing team and Gobstopper identifier before executing the downloaded binary. Releases before 0.7.6 retain their original installation behavior.
- Publication checks the exact artifact and file hashes returned by the signing job. A notarization timeout preserves the submission ID for investigation and stops publication.

## v0.7.5 - 2026-09-29

Gobstopper's source packages now declare the registry versions needed for installation through crates.io once their dependencies are published.

- The three Gobstopper crates share exact matching dependency versions. Source builds retain immutable Git pins for Apple Foundation and the CLI kit; packaged crates use their declared registry versions.
- The CLI kit pin moves to 1.1.2, whose package includes the source, README, and license required for crates.io publication.
- Each crate includes its license text, and the adapters crate includes the CliffCompaction attribution and MIT notice.

## v0.7.4 - 2026-09-29

Gobstopper now ships prebuilt binaries for macOS on Apple silicon, Linux x86_64 and arm64, and Windows x86_64, with one-line installers that check each download's SHA-256.

- `curl -fsSL https://gobstopper.sh/install.sh | sh` installs the latest release into `~/.local/bin` on macOS and Linux, and `irm https://gobstopper.sh/install.ps1 | iex` installs it for the current user on Windows without administrator rights. `GOBSTOPPER_VERSION` pins a release.
- Each release archive is built by the release workflow on GitHub Actions and carries a `.sha256` file and a build provenance attestation that `gh attestation verify` checks.
- On Windows, `detect`, `plan`, `verify`, `mcp` and the proxy run, though Claude Code session usage reads as unknown because Windows lacks the file identity the scan binds to. The vault, `apply`, `watch`, the provider hooks and `proxy install` rely on Unix guarantees and refuse with an error that names what is missing.

## v0.7.3 - 2026-09-28

The request proxy now keeps exactly the newest three turns after a compaction by default, as CliffCompaction does, because the larger tail cost more on a Terminal-Bench 2.1 run without resolving more tasks.

- `proxy serve`, `proxy run` and `proxy replay` default to `--keep-tail-percent 0` instead of 40. `--keep-tail-percent 40` restores the old default, and any share from 0 to 60 still works. `proxy status` and the startup line report the value in use.
- The evidence is one trial of the 89 Terminal-Bench 2.1 tasks through Claude Code 2.1.283 on GLM-5.3-flash, at a 45,000-token threshold, with v0.7.2 serving the tail-40 arm and at least 68 of the 89 tail-0 tasks (the build that served the first 21 was not recorded). Gobstopper at tail 0 resolved 61 of 89 tasks for $5.72 in total; at tail 40, 59 for $7.97; Claude Code with no proxy, 60 for $6.82. The resolution differences are within noise. Tail 0 against tail 40 is the only cost difference whose 95% interval excludes zero: 28% lower (interval 2% to 47%), with most of the gap from five tasks. Dollar figures are Vercel AI Gateway's metered prices for that model.

## v0.7.2 - 2026-09-27

The request proxy samples provider-reported usage on every dialect it serves, including the ChatGPT backend streams that carry no content type.

- `proxy serve` samples the usage a response reports even when it carries no `content-type`, as ChatGPT's backend-api event streams do, so Codex traffic feeds estimate calibration like every other dialect.

## v0.7.1 - 2026-09-27

Help, errors, and permission notices now follow the shared Hraness CLI contract, for people at a terminal and agents calling the binary.

- `gobstopper --help` is a short grouped list of the everyday commands, and `gobstopper help advanced` shows the full surface. Help wraps within 100 columns and drops symbols and color under `NO_COLOR` or `TERM=dumb`.
- A usage error is one sentence naming the input and the help to read next; a mistyped `stauts` suggests `proxy status`. With `--json`, or when the caller is an agent, the same error is a single JSON object on stdout.
- Login-item permission notices and their recovery wording come from the shared kit.

## v0.7.0 - 2026-09-27

The request proxy corrects its token estimate with the input counts the provider reports, so it compacts before a 200,000-token window fills.

- `proxy serve` and `proxy run` read the input tokens each response reports (`input_tokens` plus the two cache fields for Anthropic, `input_tokens` for OpenAI Responses, `prompt_tokens` for OpenAI Chat Completions; a JSON body's `usage`, an Anthropic stream's `message_start` event, or an OpenAI stream's last usage event) after relaying it unchanged. After five responses from one upstream and model, the threshold for that pair is divided by the running ratio of reported to estimated input, between 1.0 and 2.0, so compaction starts earlier and never later. `--no-calibrate` restores the plain four-characters-per-token estimate.
- `proxy status` shows `calibrate` and, for each upstream and model, the applied and measured ratio and the sample count. Compaction log lines show a ratio other than 1.0, and each ledger row records `ratio_permille`.
- `proxy replay` calibrates from the usage a Claude Code transcript records, unless given `--no-calibrate`, and reports the reported-to-estimated ratio, the largest request in reported tokens, and the requests over the threshold in reported tokens.

## v0.6.0 - 2026-09-26

The request proxy carries the conversation's words forward from one compaction to the next.

- Each summary after a session's first compaction opens with the human's words and the assistant's visible replies from the turns that earlier compactions summarized, oldest first, up to `--carry-max-chars` (default 24,000 characters, and at most a quarter of the room below the threshold). From Claude Code, the human's words include messages typed while the agent works, text typed after an interrupt, and feedback typed when rejecting a tool call; tool calls, other tool output, thinking, skill instructions, shell output, and system reminders are never carried. In the Responses and Chat Completions dialects every user-role message is carried, except the context items Codex sends again. Compaction log lines and the ledger record the carried size (`carry N chars`, `carry_chars`), never the text. With `--carry-max-chars 0` no words carry, and each summary covers only the turns since the previous compaction.
- `proxy replay` reports the carried size of each compaction as `carry_chars`.
- `proxy status` shows `carry_max_chars` when the server reports it.

## v0.5.0 - 2026-09-26

The request proxy keeps more of the agent's recent work after each compaction, and Claude Code requests that use a 1M-token window get their own threshold. `gobstopper proxy install` starts the proxy at login, `gobstopper apple` sets up Apple's on-device model, and help, errors and empty states now say what to do next.

- `proxy serve`, `proxy run` and `proxy replay` keep a recent tail sized by `--keep-tail-percent` (default 40, from 0 to 60) of the room below the threshold, instead of only the last `--keep-recent` turns. `--keep-tail-percent 0` keeps the previous tail.
- Anthropic requests whose `anthropic-beta` header lists a `context-1m` token use `--threshold-1m`: 256,000 estimated tokens by default, or `--threshold` if higher. Setting it equal to `--threshold` turns the split off.
- Consecutive assistant messages count as one turn in every dialect, so a compaction no longer separates tool results from their calls.
- `proxy replay` reads subagent transcripts and reports head, summary and tail sizes per compaction, repeated reads, and the gap between compactions.
- `proxy status` shows `threshold_1m_tokens`, `keep_tail_percent` and `requests_1m` when the server reports them.
- Each row of the proxy's stats file records `threshold_tokens`, the threshold applied to that request. A request over the configured threshold that is sent unchanged now gets a log line saying why: a large verbatim head raised the threshold, or there was nothing to compact.
- `scripts/monitor.py --proxy PORT` runs `proxy status --json` on each pass and fails the pass when the proxy does not answer. It is off by default.
- `gobstopper apple install` builds the helper the Apple scorer and state card need, and `gobstopper apple status` says whether Apple's on-device model is ready. Install checks for Xcode's command line tools first and prints `xcode-select --install` instead of letting macOS open its install dialog; compiler output goes to a log file.
- When `GOBSTOPPER_SCORER=apple` or `GOBSTOPPER_DIGEST=apple` can't use Apple's model, gobstopper now says why once, with the fix and the System Settings link: Apple Intelligence is off, the model is still downloading, the Mac or macOS can't run it, or the helper isn't installed. The state card writer used to fall back silently, and the scorer printed `scorer_unavailable`.
- apple-foundation moves to v0.2.0.
- `gobstopper proxy install` starts the proxy at login on macOS: it writes the `sh.gobstopper.proxy` LaunchAgent with the `serve` settings you pass, says first that macOS will show a Background Items notice, logs to `~/Library/Logs/gobstopper-proxy.log`, and waits for the proxy to answer. `--print` shows the file, `--replace` swaps an existing agent, and `proxy uninstall` removes it.
- `proxy status` says what to do when nothing answers: install the proxy, or restart an installed one and read its log. `proxy serve` ends with the `ANTHROPIC_BASE_URL` line to set.
- Bare `gobstopper` prints a short "Start here" list and exits 0. `--help` groups commands under Start here, Sessions and snapshots, and Setup; evaluation and integration commands moved to `gobstopper help advanced`. Every command and option has a description.
- Errors print one sentence and the next command to run (`✗ …` then `→ …`), with ASCII fallbacks for `TERM=dumb` and non-UTF-8 locales. Usage errors name the input and suggest the closest command or option. With `--json`, or when an agent runs gobstopper, an error is one `{"ok":false,"error":{…}}` object on stderr; a failed command still writes nothing to stdout.
- `gobstopper detect | head` no longer panics, and `detect` shows the newest 20 sessions with a count of the rest (`--limit 0` shows all; `--json` still lists every session). Empty `detect` and `presets` say so.
- `gobstopper auth jev` asks before it reads the clipboard. Keychain failures say whether access was denied or no keychain is available, and `auth jev --status` reports a stored key macOS refused to read instead of "no key".
- `gobstopper events` names the log it can't read and why.

## v0.4.1 - 2026-09-25

`watch` and `report` now skip sessions that have been idle longer than a window you set, and the monitor script can follow every active session of a provider without a session list.

- `watch` and `report` share one discovery window, `[discovery] max_age_secs` in `~/.config/gobstopper/config.toml` (default 7 days, `0` for every session). Idle sessions older than the window are skipped at discovery instead of being read and dropped later.
- `watch --max-age SECS`, `report --max-age SECS` and `report --all` override the window for one run. `--active-only` still selects sessions active in the last 180 seconds.
- Locked Devin sessions count as live whatever their age, so running work stays covered.
- `scripts/monitor.py --provider codex` and `--provider claude_code` cover every active session of that provider, so new sessions are followed without a `--session` list.

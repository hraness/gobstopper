# Changelog

Each version section is the text of that version's release page: a summary paragraph, then one bullet per change. [`docs/release.md`](docs/release.md) describes how a release page is built from it.

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

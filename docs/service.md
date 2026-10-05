# Start at login and prevent idle sleep during inference

Install Gobstopper's proxy as a service for your user account when you want it to start at login. The service manager restarts failed processes. Gobstopper checks the saved configuration and the running proxy's identity before changing an installation.

## Install and inspect

Use the binary in its permanent installed location. The service records its absolute path.

```sh
gobstopper proxy install --print
gobstopper proxy install
gobstopper proxy doctor --json
```

`--print` previews the platform's service definition without writing files or starting a service. Installation starts the proxy immediately on port 8260 by default and waits for a matching health response. An identical repeated installation checks the service and repairs a missing job. Changing settings requires `--replace`:

```sh
gobstopper proxy install --replace --port 8260 --no-keep-awake
```

On proxies with drain-lease protocol 1 and the managed startup guard, replacement pauses new inference requests and waits up to ten minutes for current requests to finish. New requests receive HTTP 503 with `Retry-After: 1`; they are not queued or forwarded. The controller renews a 30-second lease while waiting. If it disappears before committing the stop, waiting expires and admission resumes on the next status or request check. A timeout releases the waiting lease and preserves active requests. Older managed proxies retain their idle-only replacement behavior. An occupied port belonging to another process also stops installation. Gobstopper doesn't stop that process to obtain the port.

| Platform | Startup and restart behavior | Requirements |
| --- | --- | --- |
| macOS | A per-user LaunchAgent starts at login. launchd keeps it running and throttles restarts to 30 seconds. | A logged-in graphical user session and `launchctl`. |
| Linux | A systemd user service starts with the user session and restarts on failure after 10 seconds. Five starts within five minutes reach its restart limit. | A running systemd user manager and `busctl` with JSON output support. Installation doesn't enable lingering or install a system service. |
| Windows | A Task Scheduler task starts when the current user logs in. It uses that user's interactive token with least privilege, and permits five restart attempts at one-minute intervals. | Task Scheduler and the same logged-in user. Administrator access and a stored password aren't required. |

These are login services. They don't promise availability before a user signs in. Restart policy recovers a process exit; it doesn't diagnose every kind of unresponsive process. Native power behavior has separate qualification from service definition generation and compilation on each platform.

## Diagnose, repair, and remove

```sh
gobstopper proxy doctor --json
gobstopper proxy repair --print
gobstopper proxy repair
gobstopper proxy uninstall
```

The doctor reports configuration ownership, registration with the service manager, the executable and port, any pending operation, live health, sleep-inhibition state, and the drain phase. Waiting and committed drains are reported as unhealthy for ordinary use. The public status response exposes the proxy process identity, drain protocol, epoch, phase, and remaining wait; it does not expose the controller token. A matching HTTP response alone doesn't establish that startup is configured correctly.

`proxy status` uses the installed service's recorded port. An explicit `--port`
checks that address even when the saved service configuration is damaged. Its
network check has a two-second total deadline and a response size limit, so
a listening socket that never responds cannot hang the command indefinitely.
If a background transform holds the prefix-store lock, status returns
`store_details_available: false` and null store sizes instead of waiting for it.

`GET /gobstopper/ready` is a small local check of the listener and of whether
the proxy accepts new requests. It returns the process and service identity,
the active inference count, and whether new requests are accepted. It returns
503 while draining or while that state is busy. It does not wait for
statistics, calibration, context storage, or power-status collection. This
checks whether Gobstopper can accept work, not provider credentials or
upstream availability.

Normal forwarding has its own concurrency limit. Extra capacity remains for
health and service-control requests. Request headers and bodies have total
read deadlines, including clients that keep sending small amounts of data.
Early rejections send their response before draining unread input for at most
300 milliseconds and 64 KiB; stalled uploads cannot hold that worker indefinitely.
At the absolute socket limit, new sockets close immediately so the accept
thread continues handling connections. Existing inference is not restarted
or replayed to recover capacity.

Repair recreates missing managed definitions and restarts an absent owned job. Running repair or an identical installation from the service's recorded executable also restarts a proxy when its running version differs from the installed version, using the same [waiting lease](#install-and-inspect) on capable proxies. It resumes a legacy idle pause and reconciles recorded lease operations before starting a service. Installations record an immutable configuration snapshot and a journal before replacement. Repair reconciles an interrupted operation only when its files and loaded job match those recorded identities. It restores the predecessor configuration when available, or completes a first installation. It leaves externally edited files and unrelated jobs intact, and reports the mismatch.

A damaged manifest, a missing executable, unavailable service manager, or changed job requires investigation before repair can proceed. On Windows, a process interruption between task registration and recording the queried task definition can require manual reconciliation; repair won't overwrite a task whose ownership it cannot establish.

Uninstall uses the same [drain procedure](#install-and-inspect), then removes the owned startup definition. It retains observation data, logs, and backups. Older proxies require idle inference; unresolved operations require recovery first.

## Upgrade the service

```sh
gobstopper proxy upgrade --print
gobstopper proxy upgrade
gobstopper proxy upgrade --version 0.8.6 --wait
gobstopper proxy doctor --json
```

The foreground command requires a healthy installed managed service. It resolves the latest supported release, or the requested `--version`; a pinned installation requires an explicit version. It downloads and verifies the archive, checksum, executable identity, and macOS signature before starting a detached controller. If that version is already installed, it exits without starting a job. `--print` shows the versions, planned staging directory, and one-shot job definition without writing. The command returns the job label and log path; `--wait` follows the doctor's upgrade result and exits unsuccessfully when the controller fails or rolls back.

The controller runs outside the caller's tool shell. On macOS it uses a one-shot `sh.gobstopper.upgrade` LaunchAgent with `RunAtLoad` and no `KeepAlive`; on Linux it uses a transient `gobstopper-upgrade` systemd user unit without a restart policy. The macOS controller removes its plist immediately on entry; once removed, a later login cannot relaunch it after a crash. It exits normally rather than unloading its own running job; the next upgrade removes that finished registration only after verifying the controller has exited. It drains and stops the owned proxy, replaces the installed executable and receipt under the update lock, and starts the same service definition. Health is polled through the five-second readiness deadline, and a target that reports the new version by then is kept, even after a failed start. If replacement or startup fails after a confirmed stop, it restores the previous executable and receipt and restarts the old service when the stop can be proven. A responding but unhealthy service is stopped through the drain protocol. An unresponsive service may be force-stopped only while the manager still reports the observed PID and its kernel-reported birth time is unchanged. The rollback stop has one recorded deadline. A refused stop leaves the journal and backup with its reason for repair.

`proxy doctor` includes an `upgrade` object with the last outcome. Its stages are `staging`, `started`, `draining`, `stopped`, `replacing`, `replaced`, `starting`, `healthy`, `failed`, and `rolled_back`; failures include a reason. A second upgrade refuses while one is pending and directs you to doctor. The stage directory path is recorded before creation. `proxy repair` records `healthy` when the installed target is running and `failed` with `controller_exited` when the controller is gone. It removes a recorded stage left by a dead controller before the service stop, or reports why removal failed. A later upgrade can proceed after that reconciliation. An unacknowledged launch with no recorded controller is reconciled only when the owner has exited and the service manager proves the job is absent under the service-operation lock; unknown manager results still block recovery. Restoration keeps the original binary and receipt backups while publishing verified copies, so interruption between the two replacements does not consume the recovery source. Changed installed bytes or receipts are never overwritten. A failed restoration retains the journal and backup for investigation. The existing drain journal remains authoritative when an external stop has no durable acknowledgement.

`proxy upgrade`, `proxy install --replace`, `proxy uninstall`, `proxy repair` when it might restart, and `proxy migrate-service` refuse callers whose environment indicates they depend on this proxy: a matching loopback `ANTHROPIC_BASE_URL` or `OPENAI_BASE_URL`, `GOBSTOPPER_SCOPE`, an `X-Gobstopper-Scope` custom header, or `CLAUDECODE`. Run the command from a terminal outside an agent tool shell, or pass `--allow-dependent-caller`. A detached controller does not make it safe to stop the service its caller depends on. Preview commands with `--print` do not refuse.

### Updates and long-running commands

Every command on a managed release installation holds its update activity lock for its entire lifetime, so the executable is never replaced under a running process — except `gobstopper mcp`, which releases the lock once its startup check completes because a stdio tool server never re-executes its binary and would otherwise pin the lock for the whole agent session. `gobstopper update` reports `Busy` while any command holds the lock and names the lock path for `lsof` holder enumeration; while the managed service is running, `proxy upgrade` is the path that works — it stops the service under the drain protocol, releases the lock, replaces, and restarts.

Continuous daemons on the managed binary block every update path the same way, including `proxy upgrade`. Run watchers and monitors as periodic supervised passes — `gobstopper watch --once` under `StartInterval` or a timer — rather than always-on `KeepAlive` loops, so the lock is free between passes. A continuous `watch` on an enrolled installation prints a reminder. The upgrade controller also retries brief lock contention for up to 90 seconds before rolling back.

`proxy doctor`'s `installation` object reports whether the service executable is a managed release, its update policy, and pin state; `stale_references` lists embedded gobstopper command paths in agent configs and service definitions that no longer exist or sit where managed enrollment refuses to install, such as a package-manager bin directory.

### Interrupted service operations

Generated service definitions record their private state directory. At startup, the proxy checks its recorded identity and that directory's journal before accepting inference. It refuses startup for commit intent, committed or submitted stops, and damaged journals. This prevents an automatic replacement process from accepting work that a delayed stop could interrupt. Public status reports `drain_control.startup_guard`.

Service definitions without the recorded directory use idle-only configuration replacement. Run `proxy install --replace` with the intended proxy settings while idle to generate a guarded definition. Older running binaries cannot gain the startup check until that first configuration replacement.

Once active inference reaches zero, the controller commits the drain before asking the operating system to stop the service. A committed drain does not expire: reopening it after an uncertain stop could accept work that a delayed operating-system command then interrupts. Other controllers and the legacy resume endpoint cannot release it.

The service journal records waiting, commit intent, committed, stop started, and stop acknowledged stages. Repair checks that journal even when the old listener or startup job has disappeared. It restarts only after the stop is acknowledged and the port is free. Startup health must identify the expected service, accepting requests, and the installed version when repair runs from the recorded executable.

If the controller dies or loses a response after recording `stop_started`, repair leaves the service paused and reports the unresolved stop. Inspect `proxy doctor --json` and reconcile the operating-system operation before restarting. A dead controller, an expired timeout, or an absent listener cannot prove that a previously submitted stop will not run later. This boundary deliberately requires investigation; automatic recovery covers waiting and acknowledged stops. A crash after commit intent can prevent the replacement process from starting; if no responsive owned process remains, that state also requires investigation. Do not delete the journal or resume requests to bypass an unknown outcome.

A registered but unresponsive proxy is also preserved. The controller cannot prove that new work is paused, including across an automatic process restart, so it refuses to issue a stop. The platform's own restart policy remains responsible for ordinary process exits.

### Migrate a legacy macOS service

```sh
gobstopper proxy migrate-service --print
gobstopper proxy migrate-service
```

Migration recognizes the legacy `sh.gobstopper.proxy` and `io.hraness.gobstopper.proxy` LaunchAgents. It requires exactly one matching definition, matching loaded job arguments, and a healthy proxy. `--print` reads that proxy's settings and shows the planned arguments without changing the service; migration itself also requires the proxy to be idle. It preserves the previous plist in the service state directory, including its working directory and environment, and checks the new service's health before reporting success.

Explicit proxy arguments are preserved. When `--keep-tail-percent` is omitted, migration saves the running proxy's reported value as an explicit argument: a service using the older 40% default keeps it even though current releases default to 0%. A value read from status must be an integer from 0 through 60. If an older version does not report that setting, migration leaves the argument absent; it does not infer an unavailable value. New features without a legacy setting use the new release's defaults. The preview's `serve_args` shows the arguments that will be saved.

Legacy versions cannot atomically pause new requests. Stop initiating requests from connected clients before migrating and keep them paused until migration completes. The legacy idle check observes current activity; it cannot prevent a later request from starting during that first upgrade. Subsequent managed upgrades pause new requests as described in [Install and inspect](#install-and-inspect).

Do not add temporary firewall or packet-filter rules to automate that first
upgrade. An external filter can outlive its controller and block client
connections even when the replacement proxy is healthy. Gobstopper's service
commands do not install network filters; managed upgrades pause new requests
inside the proxy instead.

Migration keeps the original plist bytes in its backup and journal. If rollback must restart the legacy job using the upgraded binary, a separate prepared definition makes the observed retained-history setting explicit and preserves the original label, environment, working directory, logging, and other settings. This prevents rollback from switching an implicit 40% setting to the newer 0% default. Explicit proxy arguments remain authoritative.

Immediate failure and `proxy repair` share recovery checks. Matching prepared legacy processes are preserved; an already healthy upgraded installation completes without restarting it. An original process whose implicit settings need preservation can keep running while busy, with the journal retained. After clients are paused and idle activity is confirmed, repair revalidates the original process and restarts it with the prepared settings; launchd otherwise retains the old implicit arguments for future process restarts. Prepared legacy startup must report the expected process, executable, and retained-history setting before recovery completes. A failed readiness check retains the journal and backup, including after interruption between restoration and bootstrap. Edited files and unrelated jobs are rejected. Older journals without a prepared definition remain readable and retain their original exact-file recovery behavior.

## Launch with a direct fallback

```sh
gobstopper proxy launch --client claude --print
gobstopper proxy launch --client claude
gobstopper proxy launch --client codex --codex-auth chatgpt
gobstopper proxy launch --client codex --codex-auth api-key
```

The launcher checks the configured proxy with a two-second network deadline,
then starts one client. Claude can use its official provider directly when
the proxy is unavailable and the recorded route permits it. Codex requires
a healthy proxy and an existing explicitly selected custom provider that
already points to it. Codex direct fallback is unavailable: cloud configuration
can contain provider fields the launcher cannot completely verify. If the
check fails, run Codex normally with its existing settings or repair the proxy.

`--print` reports proxy readiness and the Claude route, or that Codex will use
its existing settings, without starting a client. It does not verify Codex's
effective cloud or project route. Saved settings and authentication are
retained; pass ordinary client arguments after `--`. For Codex, `--codex-auth`
identifies the existing ChatGPT or API-key route to check; it does not sign in
or obtain a key.

Claude direct fallback refuses custom upstreams, uncertain provider or
authentication settings, scoped context reservations, and configured capacity
constraints. This includes `X-Gobstopper-Scope` inside Claude's
`ANTHROPIC_CUSTOM_HEADERS` in environment or settings. Healthy launches
preserve scoped headers. Codex retains its entire existing provider table,
including literal and environment-backed headers: the launcher does not
override its provider selection, URL, headers or environment. Built-in Codex
providers and existing direct routes are refused.

The launcher also refuses Codex system and managed configuration layers it
cannot inspect. Checking effective macOS managed preferences has a two-second
deadline; unavailable inspection refuses launch. Pass short client options
separately, since combined flags can conceal a change of working directory or
settings.

This choice happens before the client starts. It does not change existing
sessions, retry a failed client, or replay inference. Sessions already using
a fixed proxy URL still need that listener to remain available.

## Sleep during inference

The proxy requests idle system sleep prevention while it handles model inference requests. Requests share one assertion. A background worker acquires it when inference starts and releases it after the last overlapping request finishes, fails, or disconnects. Acquisition and release are asynchronous; native power calls do not run on request threads. Status calls and an otherwise idle proxy don't keep the computer awake. Display sleep remains allowed.

- macOS uses an IOKit `PreventUserIdleSystemSleep` assertion.
- Linux inhibits logind's configured idle action through `systemd-inhibit --what=idle`, with a helper whose lifetime follows the proxy's open input pipe. It requires logind support and permission to acquire the inhibitor. Desktop-managed automatic suspend may still proceed; Linux desktop behavior is not yet qualified.
- Windows uses a system power request held by the proxy process.

Operating-system policy, explicit sleep, and closing a laptop lid can override idle-sleep prevention. Cached status reports transitions through `keep_awake.pending` and slow native calls through `keep_awake.stalled`. A failed assertion appears in `keep_awake.last_error`; inference continues, and ordinary acquisition errors retry every 30 seconds while inference remains active. If the worker panics or cannot start, `keep_awake.worker_alive` is false and the enabled feature requires a proxy restart to recover. `--no-keep-awake` disables this feature for `proxy serve`, `proxy run`, or `proxy install`.

Process exit releases the native assertion or closes the Linux helper's pipe. macOS qualification tests also inspect the assertion by the owned test process's PID and verify its removal after that process dies.

## Files and logs

On macOS and Linux, service state lives in `$XDG_CONFIG_HOME/gobstopper/service`, or `~/.config/gobstopper/service` when that variable is unset. Windows uses `%LOCALAPPDATA%\gobstopper\service`. The directory contains the manifest, operation lock, and any recovery journal or backup.

macOS writes the LaunchAgent under `~/Library/LaunchAgents/` and logs under `~/Library/Logs/gobstopper-proxy.log`. Linux writes its user unit under the configuration root's `systemd/user/` directory; use the user journal to inspect output. Windows saves the generated task XML in its service directory; Task Scheduler provides task status. Gobstopper's local observation journal is separate from these startup files.

Diagnostic log writes also use a bounded background queue; `diagnostic_log` reports dropped lines and write failures. Startup output does not delay the listener, and the context database opens lazily when a scoped request needs it. Scoped requests fail closed while context storage is unavailable.

Scoped policy reads and reservation consumption have a separate worker limit and a two-second response deadline. Unavailable storage returns a retryable 503 before inference is sent. A timed-out reservation consumption may have completed, so Gobstopper never retries or restores it internally. Adaptive evidence observations use a bounded background queue; `context_control.observations` reports dropped batches and write failures.

Legacy token totals in `proxy-stats.jsonl` are loaded and appended by a
background worker. A slow or unwritable disk does not delay forwarding.
The queue is bounded; status reports dropped events and write failures under
`stats_persistence`. A later request retries storage after a write failure,
without replaying a record whose append result is uncertain. Existing bytes
are preserved. Totals can be partial while `loading_history` is true; an
unreadable, malformed, or incomplete historical record sets
`history_incomplete`. These counters describe legacy JSONL persistence, not
the separate session observation database.

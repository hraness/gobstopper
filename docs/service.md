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

A replacement refuses the operation while inference is active. When idle, the owned proxy atomically stops admitting new inference before the service manager stops it. Requests arriving during replacement receive a retryable error. Retry replacement after active requests finish. An occupied port belonging to another process also stops installation. Gobstopper doesn't stop that process to obtain the port.

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

The doctor reports configuration ownership, registration with the service manager, the executable and port, any pending operation, live health, and sleep-inhibition state. A matching HTTP response alone doesn't establish that startup is configured correctly.

Repair recreates missing managed definitions and restarts an absent owned job. Running repair or an identical installation from the service's recorded executable also restarts an idle proxy when its running version differs from the installed version. It resumes inference admission if an interrupted stop left a healthy owned proxy paused. Installations record an immutable configuration snapshot and a journal before replacement. Repair reconciles an interrupted operation only when its files and loaded job match those recorded identities. It restores the predecessor configuration when available, or completes a first installation. It leaves externally edited files and unrelated jobs intact, and reports the mismatch.

A damaged manifest, a missing executable, unavailable service manager, or changed job requires investigation before repair can proceed. On Windows, a process interruption between task registration and recording the queried task definition can require manual reconciliation; repair won't overwrite a task whose ownership it cannot establish.

Uninstall stops an owned idle service and removes its startup definition. It retains observation data, logs, and backups. It refuses while inference is active or an operation needs recovery.

### Migrate a legacy macOS service

```sh
gobstopper proxy migrate-service --print
gobstopper proxy migrate-service
```

Migration recognizes the legacy `sh.gobstopper.proxy` and `io.hraness.gobstopper.proxy` LaunchAgents. It requires exactly one matching definition, matching loaded job arguments, and a healthy proxy. `--print` reads that proxy's settings and shows the planned arguments without changing the service; migration itself also requires the proxy to be idle. It preserves the previous plist in the service state directory, including its working directory and environment, and checks the new service's health before reporting success.

Explicit proxy arguments are preserved. When `--keep-tail-percent` is omitted, migration saves the running proxy's reported value as an explicit argument: a service using the older 40% default keeps it even though current releases default to 0%. A value read from status must be an integer from 0 through 60. If an older version does not report that setting, migration leaves the argument absent; it does not infer an unavailable value. New features without a legacy setting use the new release's defaults. The preview's `serve_args` shows the arguments that will be saved.

Legacy versions cannot atomically pause new requests. Stop initiating requests from connected clients before migrating and keep them paused until migration completes. The legacy idle check observes current activity; it cannot prevent a later request from starting during that first upgrade. Subsequent managed upgrades use the admission handshake above.

Migration keeps the original plist bytes in its backup and journal. If rollback must restart the legacy job using the upgraded binary, a separate prepared definition makes the observed retained-history setting explicit and preserves the original label, environment, working directory, logging, and other settings. This prevents rollback from switching an implicit 40% setting to the newer 0% default. Explicit proxy arguments remain authoritative.

Immediate failure and `proxy repair` share recovery checks. Matching prepared legacy processes are preserved; an already healthy upgraded installation completes without restarting it. An original process whose implicit settings need preservation can keep running while busy, with the journal retained. After clients are paused and idle activity is confirmed, repair revalidates the original process and restarts it with the prepared settings; launchd otherwise retains the old implicit arguments for future process restarts. Prepared legacy startup must report the expected process, executable, and retained-history setting before recovery completes. A failed readiness check retains the journal and backup, including after interruption between restoration and bootstrap. Edited files and unrelated jobs are rejected. Older journals without a prepared definition remain readable and retain their original exact-file recovery behavior.

## Sleep during inference

The proxy prevents idle system sleep while it handles model inference requests. Requests share one assertion: it remains held until the last overlapping request finishes, fails, or disconnects. Status calls and an otherwise idle proxy don't keep the computer awake. Display sleep remains allowed.

- macOS uses an IOKit `PreventUserIdleSystemSleep` assertion.
- Linux inhibits logind's configured idle action through `systemd-inhibit --what=idle`, with a helper whose lifetime follows the proxy's open input pipe. It requires logind support and permission to acquire the inhibitor. Desktop-managed automatic suspend may still proceed; Linux desktop behavior is not yet qualified.
- Windows uses a system power request held by the proxy process.

Operating-system policy, explicit sleep, and closing a laptop lid can override idle-sleep prevention. A failed assertion appears in `keep_awake.last_error`; inference can continue and later requests retry after a bounded delay. `--no-keep-awake` disables this feature for `proxy serve`, `proxy run`, or `proxy install`.

Process exit releases the native assertion or closes the Linux helper's pipe. macOS qualification tests also inspect the assertion by the owned test process's PID and verify its removal after that process dies.

## Files and logs

On macOS and Linux, service state lives in `$XDG_CONFIG_HOME/gobstopper/service`, or `~/.config/gobstopper/service` when that variable is unset. Windows uses `%LOCALAPPDATA%\gobstopper\service`. The directory contains the manifest, operation lock, and any recovery journal or backup.

macOS writes the LaunchAgent under `~/Library/LaunchAgents/` and logs under `~/Library/Logs/gobstopper-proxy.log`. Linux writes its user unit under the configuration root's `systemd/user/` directory; use the user journal to inspect output. Windows saves the generated task XML in its service directory; Task Scheduler provides task status. Gobstopper's local observation journal is separate from these startup files.

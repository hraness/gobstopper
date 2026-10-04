Gobstopper sees the requests that pass through its proxy. Your coding agents also run sessions that never touch it: a Codex run in another terminal, a Claude Code session started before the proxy, or another agent entirely. `gobstopper usage` covers all of them by reading [aicharts](https://aicharts.io/usage), which keeps a daily record of token use on your computer.

## What the installer sets up

On macOS (Apple silicon) and Linux x86_64, `curl -fsSL https://gobstopper.sh/install.sh | sh` installs aicharts beside `gobstopper` in `~/.local/bin`. The script checks the download against a SHA-256 digest written into the script and, on macOS, checks the binary's Developer ID signature before running it. On a first install it turns on aicharts' local history. aicharts then reads your agents' session files four times a day and keeps daily totals by agent, provider and model. Nothing is uploaded, and no account is involved.

Set `GOBSTOPPER_USAGE_HISTORY=no` to install aicharts but leave history off, or `GOBSTOPPER_AICHARTS=no` to skip aicharts. A later install never changes the choice you made, and an aicharts you installed some other way is left alone.

## Reading the record

```sh
gobstopper usage                        # the last 30 days, per agent
gobstopper usage report --days 7 --csv  # one row per day, agent and model
gobstopper usage status                 # whether aicharts is collecting
```

`gobstopper usage` runs only aicharts' `history` commands: report, status, enable, disable and collect. It refuses anything else, so Gobstopper cannot ask aicharts to enroll or publish. It also runs before Gobstopper reads its configuration or checks for updates, so a report never starts the proxy.

## Two counts, read side by side

`gobstopper data` counts what the proxy saw: requests, tokens before and after compaction, and what compaction saved. aicharts counts what each agent wrote to its own session files. A request that went through the proxy appears in both, so compare the two instead of adding them. The gap between them shows which agents spend tokens outside the proxy.

## Asking an agent

aicharts also serves the record through a read-only MCP server, `aicharts mcp`. Its tools return totals, a daily series, the full report, the agent list and collection status. Register it with an agent once, then ask how your token use changed this week, or have it chart the CSV.

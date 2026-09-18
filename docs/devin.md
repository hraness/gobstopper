# Devin integration

Gobstopper integrates with Devin through documented control-plane and export surfaces. It does not read or modify Devin's private session database.

## Native compaction policy

Devin CLI exposes `/context` and `/compact`. Evaluate the same layered Gobstopper policy used by other agents:

```sh
gobstopper policy-check \
  --provider devin \
  --context-tokens 300000 \
  --session-active \
  --json
```

An over-threshold response uses:

```json
{
  "provider": "devin",
  "action": "provider_compact",
  "control": "/compact"
}
```

The caller executes `/compact` inside the owned Devin session. Gobstopper never injects input into another process and never becomes a second session writer.

Configure Devin independently from Codex and Claude Code:

```toml
[provider.devin]
trigger_tokens = 200_000
floor_tokens = 40_000
min_savings_tokens = 4_096
```

`floor_tokens` and `min_savings_tokens` remain useful shared policy metadata, but Devin controls the actual post-compaction result.

## Read-only MCP

Current Devin CLI versions support stdio MCP registration:

```sh
devin mcp add -s user gobstopper -- gobstopper mcp
devin mcp get gobstopper
```

Use `-s project` for checked-in `.devin/mcp_config.json`, or omit `-s` for the gitignored local `.devin/mcp_config.local.json`. The server exposes only read-only tools. `policy_check` supports Devin; transcript `plan` and `verify` apply only to discovered Codex and Claude Code files.

The agent should call `policy_check` with the token count shown by `/context`, then run `/compact` itself when the response requests `provider_compact`.

## Export inspection

`devin --export out.json` writes ATIF, a whole JSON document rather than JSONL. A provider plugin can inspect it without granting mutation authority:

```sh
gobstopper plugin check /absolute/path/gobstopper-plugin.json
gobstopper plugin inspect /absolute/path/gobstopper-plugin.json \
  --trusted-sha256 <manifest-sha256> \
  --provider devin-atif \
  --source /absolute/path/out.json
```

Provider-read plugins receive bounded source content and return normalized logical items plus usage. Logical item indexes need not equal physical line numbers, so pretty-printed and one-line ATIF exports are both supported. Provider inspection cannot return edits.

## Resume integrity

Use Devin's own `--resume <session-id>` or `--continue` controls. Gobstopper does not synthesize Devin session IDs, restore ATIF into Devin, or claim that an exported trajectory can replace provider-owned resume state.

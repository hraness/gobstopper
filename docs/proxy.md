# Compact live coding-agent requests

`gobstopper proxy` sits on 127.0.0.1 between a coding agent and its model
provider. When a request passes the threshold, it sends the head verbatim,
one mechanical summary of the older turns, and the newest turns verbatim. The
provider then reports the smaller size back to the client, so the client's
own auto-compaction does not reach its trigger. Session files are not
changed. The rule is CliffCompaction's (Nguyen, Cho, Chen and Dettmers,
[arXiv:2609.26779](https://arxiv.org/abs/2609.26779)); the port's MIT notice
is in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).

The proxy understands three API dialects. Any agent that lets you set a
custom provider address can use it:

| Dialect | Endpoint | Agents |
|---|---|---|
| Anthropic Messages | `.../messages` | Claude Code, opencode, Crush |
| OpenAI Responses | `.../responses` | Codex |
| OpenAI Chat Completions | `.../chat/completions` | opencode, Crush, Aider, Goose, other OpenAI-compatible clients |

The proxy is in the current `main` source build. It needs the system `curl`,
version 8.3 or later (`curl --version`).

## Try it on one session

```sh
gobstopper proxy run -- claude
```

`run` starts the proxy on a free port, sets `ANTHROPIC_BASE_URL` and
`OPENAI_BASE_URL` for that command only, and stops when the command exits.
Codex takes its model provider from `~/.codex/config.toml` or `-c`
overrides, so route it with the provider block in [Codex](#codex).

## Run it in the background

```sh
gobstopper proxy serve        # http://127.0.0.1:8260; --port changes it
```

On macOS, a LaunchAgent keeps it running across logins. Save this as
`~/Library/LaunchAgents/sh.gobstopper.proxy.plist`, replacing `YOU` with your
user name:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>sh.gobstopper.proxy</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOU/.cargo/bin/gobstopper</string>
    <string>proxy</string>
    <string>serve</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/Users/YOU/Library/Logs/gobstopper-proxy.log</string>
  <key>StandardErrorPath</key><string>/Users/YOU/Library/Logs/gobstopper-proxy.log</string>
</dict>
</plist>
```

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/sh.gobstopper.proxy.plist
gobstopper proxy status
```

After upgrading the binary, restart it with
`launchctl kickstart -k gui/$(id -u)/sh.gobstopper.proxy`.

## Claude Code

Add this to your shell profile, then start Claude Code from a new terminal:

```sh
export ANTHROPIC_BASE_URL=http://127.0.0.1:8260
```

Claude Code sends your claude.ai sign-in or API key through the proxy
unchanged. If `ANTHROPIC_API_KEY` is set in your environment, Claude Code
uses that key instead of your claude.ai sign-in, with or without the proxy.
To run one session without the proxy, use `env -u ANTHROPIC_BASE_URL claude`.

## Codex

Codex reads its provider from `~/.codex/config.toml`. With ChatGPT sign-in:

```toml
model_provider = "gobstopper"    # top-level key: above the first [table]

[model_providers.gobstopper]
name = "OpenAI via gobstopper"
base_url = "http://127.0.0.1:8260/backend-api/codex"
wire_api = "responses"
requires_openai_auth = true
```

With an OpenAI API key, use `base_url = "http://127.0.0.1:8260/v1"` and
`env_key = "OPENAI_API_KEY"` instead of `requires_openai_auth`. To run one
session without the proxy, use `codex -c model_provider=openai`.

## opencode

opencode providers take an `options.baseURL`. For an Anthropic-shaped
provider, point it at the proxy root in `opencode.json`:

```json
{
  "provider": {
    "gobstopper-anthropic": {
      "npm": "@ai-sdk/anthropic",
      "options": { "baseURL": "http://127.0.0.1:8260/v1" },
      "models": { "claude-sonnet-4-5": {} }
    }
  }
}
```

For an OpenAI-compatible provider, use the Chat Completions dialect:

```json
{
  "provider": {
    "gobstopper-openai": {
      "npm": "@ai-sdk/openai-compatible",
      "options": { "baseURL": "http://127.0.0.1:8260/v1" },
      "models": { "gpt-5": {} }
    }
  }
}
```

API keys come from opencode's own provider credentials; the proxy forwards
them unchanged.

## Crush

Crush providers accept a `base_url` and a `type`. In `crush.json`:

```json
{
  "providers": {
    "gobstopper": {
      "type": "openai-compat",
      "base_url": "http://127.0.0.1:8260/v1",
      "models": [{ "id": "gpt-5", "name": "GPT-5" }]
    }
  }
}
```

A `"type": "anthropic"` provider with `base_url` pointed at the proxy uses
the Messages dialect instead.

## Aider

Aider routes OpenAI-shaped models through a configurable base URL:

```sh
aider --openai-api-base http://127.0.0.1:8260/v1 --model openai/<model>
# or: OPENAI_API_BASE=http://127.0.0.1:8260/v1
```

## Goose

Goose's OpenAI provider reads `OPENAI_HOST` (and `OPENAI_BASE_PATH`):

```sh
OPENAI_HOST=http://127.0.0.1:8260 goose
```

A declarative custom provider with `engine: openai` pointed at
`http://127.0.0.1:8260/v1` works the same way.

Any OpenAI-compatible client that posts to `{base}/chat/completions` works:
give it a base URL of `http://127.0.0.1:8260/v1`. The bare path
`/chat/completions` (no `/v1`) also routes to the OpenAI upstream, but most
providers expect the `/v1` prefix, so configure the base URL with it.

## Preview on a recorded session

```sh
gobstopper proxy replay <session>
gobstopper proxy replay <session> --threshold 100000 --json
```

`replay` rebuilds the requests a Claude Code or Codex session sent, runs each
through the engine, and reports the peak request with and without the proxy,
the number of compactions and reused prefixes, the context sent across all
requests, and any history left with an unpaired tool call. It calls no
provider. Transcripts do not record the system prompt and tool definitions,
so `--fixed-tokens` (20,000 by default) stands in for them.

## Settings

| Flag | Default | Meaning |
|---|---|---|
| `--threshold` | 128000 | Compact when the estimated outgoing request exceeds this many tokens. Keep it below the client's own auto-compaction point. |
| `--keep-recent` | 3 | Newest assistant steps kept verbatim. |
| `--result-max-chars` | 500 | Older tool results longer than this are dropped from the summary; shorter ones stay verbatim. |
| `--drop-thinking` | off | Leave thinking and reasoning text out of summaries. |
| `--shadow` | off | Log what would change and forward every request unchanged. |
| `--strict` | off | Refuse (HTTP 400) a request still over the threshold after every step, instead of sending it. |
| `--anthropic-upstream` | `https://api.anthropic.com` | Where Anthropic requests go. |
| `--openai-upstream` | `https://api.openai.com` | Where OpenAI API requests (`/v1/...`, `.../chat/completions`) go. Point it at any OpenAI-compatible provider. |
| `--chatgpt-upstream` | `https://chatgpt.com` | Where ChatGPT-signed-in Codex requests (`/backend-api/...`) go. |

## How it works

- The proxy compacts Anthropic Messages (`.../messages`), OpenAI Responses
  (`.../responses`), and OpenAI Chat Completions (`.../chat/completions`)
  requests. Token counts, provider-side compaction endpoints, and every
  other path pass through unchanged.
- The head is everything before the first model turn: for Claude Code, the
  first user message; for Codex, the environment and instruction messages
  and the first prompt; for Chat Completions, the `system`, `developer`,
  and `user` messages that precede the first assistant turn. It is always
  sent verbatim, as are the system prompt, tool definitions, and other
  request fields.
- A turn starts at a model message and includes the tool results that answer
  it: `tool_result` blocks for Anthropic, `function_call_output` items for
  Responses, and the run of `tool` messages answering a `tool_calls` turn
  for Chat Completions. The summary keeps human and assistant text (and
  readable thinking), keeps tool results of at most 500 characters, reduces
  each tool call to its name and up to 150 characters of arguments, and
  drops images. Calls are never separated from their results: the kept tail
  is whole turns, so a `tool` message can never outlive the call it answers.
- Clients resend their original history on every request. The proxy keys each
  compaction by a hash of the original prefix and substitutes it into later
  requests, so the compacted prefix stays byte-stable until the next
  compaction and the provider's prompt cache can match it. The cache lives in
  memory; after a restart, the proxy replays the threshold crossings over the
  full history and reaches the same result.
- If one pass leaves a request over the threshold, the proxy retries with one
  kept turn, then with assistant text capped at 300 characters and thinking
  dropped. Without `--strict`, it then sends the request anyway.
- If the provider rejects a request for length, the proxy compacts further and
  retries, and as a last step shortens the summary to its newest parts. If
  the provider rejects the rewritten request for any other reason, the proxy
  resends the client's original bytes.
- If the verbatim head alone approaches the threshold, as it can after the
  client compacted a session itself, the threshold for that session rises to
  the head plus half the configured threshold.

## Privacy and security

- The proxy binds 127.0.0.1 and refuses requests addressed to any host name
  other than 127.0.0.1, localhost, or ::1.
- It forwards your request headers, including API keys and sign-in tokens, to
  curl through curl's environment, not its command line.
- Logs contain paths, sizes, counts, and error summaries, never request or
  response text.
- Every compactable request also appends one JSONL record (timestamp,
  dialect, path, estimated tokens in and out, and flags) to
  `~/.local/share/gobstopper/proxy-stats.jsonl`, so `gobstopper proxy status`
  reports estimated-token totals for this run and all time across restarts.
  `GOBSTOPPER_STATS_FILE` overrides the path; set it to `off` to disable the
  ledger.
- Unparseable or compressed request bodies are forwarded unchanged.

## Limits

- Sizes are estimates at four characters per token, with images priced by
  their dimensions; the provider's count can differ.
- A summary drops the details of long tool results. The agent can read the
  file or rerun the command, but nothing makes it notice that a detail is
  missing.
- Agents that send requests through a vendor service with no configurable
  model address cannot use the proxy.
- Live use through the proxy has been checked for routing with Claude Code
  2.1.282 and Codex 0.156.1. Chat Completions coverage is tested against
  synthetic histories and recorded contracts, not a live opencode, Crush,
  Aider, or Goose session; provider acceptance of that dialect is
  unqualified. Task quality and cost under the proxy have not been
  measured.

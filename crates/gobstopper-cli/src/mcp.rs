//! `gobstopper mcp` — a read-only Model Context Protocol server over stdio.
//!
//! Exposes the session detector and the snapshot vault as MCP tools so an
//! agent can recall state-card digests, inspect recorded states, and dry-run
//! compaction plans without leaving its session. Transport is
//! newline-delimited JSON-RPC 2.0 on stdin/stdout; every tool is read-only.

use std::io::{BufRead as _, Write};

use anyhow::Result;
use serde_json::{json, Value};

use crate::{config, evaluate, find_session, recall_rows, session_rows, show_summary};
use crate::{diff_summary, Cli};
use gobstopper_adapters::{detect, eval, transaction, vault, verify};

const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn run(cli: &Cli, cfg: &config::Config) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(message) => {
                if let Some(response) = handle(cli, cfg, &message) {
                    write_message(&mut out, &response)?;
                }
            }
            Err(e) => write_message(
                &mut out,
                &error(Value::Null, -32700, &format!("parse error: {e}")),
            )?,
        }
    }
    Ok(())
}

fn write_message(out: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, message)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn text_result(id: &Value, value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    result(id, json!({"content": [{"type": "text", "text": text}]}))
}

fn tool_error(id: &Value, message: String) -> Value {
    result(
        id,
        json!({"content": [{"type": "text", "text": message}], "isError": true}),
    )
}

fn handle(cli: &Cli, cfg: &config::Config, message: &Value) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str)?;
    // Notifications carry no id and expect no response.
    let id = message.get("id").cloned()?;

    Some(match method {
        "initialize" => result(
            &id,
            json!({
                "protocolVersion": message
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(PROTOCOL_VERSION),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "gobstopper", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Read-only access to detected Codex/Claude sessions and the gobstopper snapshot vault. Use list_sessions to find sessions, recall to search state-card digests (agent memory), history/show/diff to inspect recorded states, plan to dry-run compaction, verify to check transcript integrity. No tool mutates transcripts.",
            }),
        ),
        "ping" => result(&id, json!({})),
        "tools/list" => result(&id, json!({"tools": tools()})),
        "tools/call" => call_tool(cli, cfg, &id, message.get("params")),
        "resources/list" => result(&id, json!({"resources": []})),
        "prompts/list" => result(&id, json!({"prompts": []})),
        _ => error(id, -32601, &format!("method not found: {method}")),
    })
}

fn call_tool(cli: &Cli, cfg: &config::Config, id: &Value, params: Option<&Value>) -> Value {
    let Some(name) = params.and_then(|p| p.get("name")).and_then(Value::as_str) else {
        return error(id.clone(), -32602, "tools/call missing params.name");
    };
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    match run_tool(cli, cfg, name, &args) {
        Ok(value) => text_result(id, &value),
        Err(e) => tool_error(id, format!("{e:#}")),
    }
}

fn tools() -> Value {
    json!([
        {
            "name": "list_sessions",
            "description": "List detected Codex/Claude Code sessions with context occupancy (context tokens, lifetime input tokens, active state).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "all": {"type": "boolean", "description": "include sessions of any age (default: last 7 days only)"}
                }
            }
        },
        {
            "name": "recall",
            "description": "Search gobstopper state-card digests across archived session snapshots. Returns goal, decisions, files touched and open tasks — agent memory without verbatim tool output.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "case-insensitive substring matched against digest fields"},
                    "session": {"type": "string", "description": "session id prefix; omit to search every session"},
                    "sha": {"type": "string", "description": "restrict to one snapshot sha256 prefix"},
                    "limit": {"type": "integer", "description": "max results (default 20)"}
                }
            }
        },
        {
            "name": "history",
            "description": "List every recorded snapshot of a session transcript, newest first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {"type": "string", "description": "session id prefix or transcript path"}
                },
                "required": ["session"]
            }
        },
        {
            "name": "show",
            "description": "Structural summary of a vault snapshot: byte size, record counts by type, strategy label.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "snapshot sha256 prefix, or a session id/path to show its latest snapshot"}
                },
                "required": ["target"]
            }
        },
        {
            "name": "diff",
            "description": "Structural record-level diff between two vault snapshots: added/removed record counts and per-type counts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "a": {"type": "string", "description": "sha256 prefix of the first snapshot"},
                    "b": {"type": "string", "description": "sha256 prefix of the second snapshot"}
                },
                "required": ["a", "b"]
            }
        },
        {
            "name": "plan",
            "description": "Dry-run a compaction strategy on a session: projected context tokens, elided item count, and preserved prefix tokens. Never modifies the transcript.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {"type": "string", "description": "session id prefix or transcript path"},
                    "strategy": {"type": "string", "description": "auto | sawtooth | elide | compacted | cache_aware | scored | structured | agentic"},
                    "trigger": {"type": "integer", "description": "override trigger threshold (tokens)"},
                    "floor": {"type": "integer", "description": "override post-compaction floor (tokens)"},
                    "adaptive": {"type": "boolean", "description": "derive trigger/floor from the provider window, elidable share, and past compaction yields for this evaluation"}
                },
                "required": ["session"]
            }
        },
        {
            "name": "verify",
            "description": "Check a session transcript for resume-breaking defects (broken parent chains, orphaned tool calls, malformed compaction records).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": {"type": "string", "description": "session id prefix or transcript path"}
                },
                "required": ["session"]
            }
        }
    ])
}

fn run_tool(cli: &Cli, cfg: &config::Config, name: &str, args: &Value) -> Result<Value> {
    let get_str = |key: &str| args.get(key).and_then(Value::as_str);
    match name {
        "list_sessions" => {
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            Ok(json!({"sessions": session_rows(cli, all)}))
        }
        "recall" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .min(100) as usize;
            let digests = recall_rows(
                cli,
                cfg,
                get_str("session"),
                get_str("query"),
                get_str("sha"),
                limit,
            )?;
            let rows: Vec<Value> = digests.iter().map(crate::recall_row_json).collect();
            Ok(json!({"count": rows.len(), "digests": rows}))
        }
        "history" => {
            let session = get_str("session").unwrap_or_default();
            let d = find_session(cli, cfg, session)?;
            let root = vault::default_root();
            let mut entries = vault::list(&root)?;
            entries.retain(|e| e.path == d.handle.path || e.session_id.starts_with(session));
            Ok(json!({"session_id": d.handle.session_id, "snapshots": entries}))
        }
        "show" => show_summary(cli, cfg, get_str("target").unwrap_or_default()),
        "diff" => diff_summary(
            get_str("a").unwrap_or_default(),
            get_str("b").unwrap_or_default(),
        ),
        "plan" => {
            let session = get_str("session").unwrap_or_default();
            let d = find_session(cli, cfg, session)?;
            let mut resolved = cfg.resolve(
                d.handle.provider,
                &d.handle.session_id,
                None,
                get_str("strategy"),
            )?;
            if let Some(t) = args.get("trigger").and_then(Value::as_u64) {
                resolved.policy.trigger_tokens = t;
            }
            if let Some(f) = args.get("floor").and_then(Value::as_u64) {
                resolved.policy.floor_tokens = f;
            }
            if let Some(adaptive) = args.get("adaptive").and_then(Value::as_bool) {
                resolved.policy.adaptive = adaptive;
            }
            let transcript = detect::load(&d)?;
            let (effective, reasons) = crate::effective_policy(&transcript, &resolved);
            let adaptive_block = if resolved.policy.adaptive {
                json!({
                    "enabled": true,
                    "trigger_tokens": effective.trigger_tokens,
                    "floor_tokens": effective.floor_tokens,
                    "reasons": reasons,
                })
            } else {
                Value::Null
            };
            match evaluate(&transcript, &resolved)? {
                Some(plan) => {
                    let prefix = eval::prefix_tokens(&transcript, &plan);
                    Ok(json!({
                        "session_id": d.handle.session_id,
                        "plan": plan,
                        "prefix_tokens": prefix,
                        "adaptive": adaptive_block,
                    }))
                }
                None => Ok(json!({
                    "session_id": d.handle.session_id,
                    "plan": Value::Null,
                    "reason": "no applicable edits at this policy",
                    "adaptive": adaptive_block,
                })),
            }
        }
        "verify" => {
            let session = get_str("session").unwrap_or_default();
            let d = find_session(cli, cfg, session)?;
            let bytes = transaction::read(&d.handle.path)?;
            let findings = verify::verify(d.handle.provider, &bytes);
            let errors = findings
                .iter()
                .filter(|f| f.severity == verify::Severity::Error)
                .count();
            Ok(json!({
                "session_id": d.handle.session_id,
                "errors": errors,
                "findings": findings,
            }))
        }
        _ => anyhow::bail!("unknown tool '{name}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> Cli {
        Cli {
            codex_home: None,
            claude_home: None,
            codex_bin: None,
            command: crate::Cmd::Mcp,
        }
    }

    fn cfg() -> config::Config {
        config::Config::default()
    }

    #[test]
    fn initialize_echoes_client_protocol_version() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"},
        });
        let response = handle(&cli(), &cfg(), &msg).unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(response["result"]["serverInfo"]["name"], "gobstopper");
    }

    #[test]
    fn notifications_get_no_response() {
        let msg = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(handle(&cli(), &cfg(), &msg).is_none());
    }

    #[test]
    fn tools_list_is_read_only() {
        let msg = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
        let response = handle(&cli(), &cfg(), &msg).unwrap();
        let names: Vec<&str> = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in [
            "list_sessions",
            "recall",
            "history",
            "show",
            "diff",
            "plan",
            "verify",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        // Mutating surfaces must never be agent-callable.
        for forbidden in ["apply", "undo", "fork", "watch", "snapshot"] {
            assert!(
                !names.contains(&forbidden),
                "exposed mutating tool {forbidden}"
            );
        }
    }

    #[test]
    fn unknown_method_and_tool_fail_cleanly() {
        let msg = json!({"jsonrpc": "2.0", "id": 3, "method": "bogus/method"});
        let response = handle(&cli(), &cfg(), &msg).unwrap();
        assert_eq!(response["error"]["code"], -32601);

        let msg = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "apply", "arguments": {}},
        });
        let response = handle(&cli(), &cfg(), &msg).unwrap();
        assert_eq!(response["result"]["isError"], true);
    }
}

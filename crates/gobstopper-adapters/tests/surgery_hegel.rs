//! Stateful surgery invariants under Hegel's interleaved draw model.
//!
//! Each case draws a random provider transcript, then a random command
//! sequence — elide subsets, digest injection, vault snapshot/restore,
//! provider appends — asserting after every mutation that the transcript
//! still verifies clean and that order and linkage fields are untouched.
//! This is the "don't break sessions" property, exercised directly
//! against the real write paths rather than at the CLI boundary.

use gobstopper_adapters::{claude, codex, vault, verify};
use gobstopper_core::plan::{DigestBlock, Edit};
use gobstopper_core::Provider;
use hegel::generators as gs;
use hegel::TestCase;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn tmpdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gobstopper-hegel-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_lines(path: &Path, lines: &[String]) {
    fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .split_inclusive('\n')
        .map(|l| l.trim_end_matches('\n').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The structural fields surgery must never disturb. Two lines are
/// "linkage-identical" when all of these match byte-for-byte.
fn linkage(provider: Provider, line: &str) -> Option<Value> {
    let record = serde_json::from_str::<Value>(line).ok()?;
    Some(match provider {
        Provider::ClaudeCode => json!({
            "type": record.get("type"),
            "uuid": record.get("uuid"),
            "parentUuid": record.get("parentUuid"),
            "sessionId": record.get("sessionId"),
            "isSidechain": record.get("isSidechain"),
            "timestamp": record.get("timestamp"),
            "tool_use_ids": record.pointer("/message/content")
                .and_then(Value::as_array)
                .map(|blocks| blocks.iter()
                    .filter_map(|b| b.get("id").cloned())
                    .collect::<Vec<_>>()),
            "tool_result_ids": record.pointer("/message/content")
                .and_then(Value::as_array)
                .map(|blocks| blocks.iter()
                    .filter_map(|b| b.get("tool_use_id").cloned())
                    .collect::<Vec<_>>()),
        }),
        Provider::Codex => json!({
            "type": record.get("type"),
            "ordinal": record.get("ordinal"),
            "id": record.get("payload").and_then(|p| p.get("id")),
            "call_id": record.get("payload").and_then(|p| p.get("call_id")),
            "timestamp": record.get("timestamp"),
            "session_id": record.get("payload").and_then(|p| p.get("id")),
        }),
    })
}

fn no_errors(provider: Provider, path: &Path) {
    let bytes = fs::read(path).unwrap();
    let findings = verify::verify(provider, &bytes);
    let errors: Vec<_> = findings
        .iter()
        .filter(|f| f.severity == verify::Severity::Error)
        .collect();
    assert!(
        errors.is_empty(),
        "verify found errors after surgery: {errors:?}"
    );
}

/// A random Claude Code transcript: uuid/parentUuid chain, tool_use /
/// tool_result pairs, occasional sidechains, varying payload sizes.
fn gen_claude_transcript(tc: &TestCase) -> Vec<String> {
    let session = format!("s{}", tc.draw(gs::integers::<u32>()));
    let n = tc.draw(gs::integers::<usize>().min_value(3).max_value(24));
    let mut lines = Vec::new();
    let mut uuids: Vec<String> = Vec::new();
    let mut open_calls: Vec<String> = Vec::new();
    for i in 0..n {
        let uuid = format!("u{i}");
        let parent = if uuids.is_empty() {
            Value::Null
        } else {
            let idx = tc.draw(gs::integers::<usize>().max_value(uuids.len() - 1));
            json!(uuids[idx])
        };
        // Draw a record kind: plain user text, assistant text, tool_use,
        // tool_result (only when a call is open), or sidechain note.
        let mut choices = vec!["user_text", "assistant_text", "tool_use"];
        if !open_calls.is_empty() {
            choices.push("tool_result");
        }
        let kind = choices[tc.draw(gs::integers::<usize>().max_value(choices.len() - 1))];
        let sidechain = tc.draw(gs::booleans());
        let line = match kind {
            "tool_use" => {
                let call_id = format!("t{}", open_calls.len() + i);
                open_calls.push(call_id.clone());
                json!({
                    "type": "assistant", "uuid": uuid, "parentUuid": parent,
                    "sessionId": session, "isSidechain": sidechain,
                    "message": {"role": "assistant", "content": [
                        {"type": "tool_use", "id": call_id, "name": "Bash", "input": {}}
                    ]}
                })
            }
            "tool_result" => {
                let idx = tc.draw(gs::integers::<usize>().max_value(open_calls.len() - 1));
                let call_id = open_calls.remove(idx);
                // Sizes straddle the 256-byte elision floor.
                let size = tc.draw(gs::integers::<usize>().max_value(1500));
                let content = "x".repeat(size);
                json!({
                    "type": "user", "uuid": uuid, "parentUuid": parent,
                    "sessionId": session, "isSidechain": sidechain,
                    "message": {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": call_id, "content": content}
                    ]}
                })
            }
            "assistant_text" => json!({
                "type": "assistant", "uuid": uuid, "parentUuid": parent,
                "sessionId": session, "isSidechain": sidechain,
                "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "working on it"}
                ]}
            }),
            _ => json!({
                "type": "user", "uuid": uuid, "parentUuid": parent,
                "sessionId": session, "isSidechain": sidechain,
                "message": {"role": "user", "content": "please do the thing"}
            }),
        };
        uuids.push(uuid);
        lines.push(serde_json::to_string(&line).unwrap());
    }
    // Close every outstanding tool_use so verify sees a finished transcript.
    for call_id in open_calls.drain(..) {
        let uuid = format!("u{}", uuids.len());
        let size = tc.draw(gs::integers::<usize>().max_value(1500));
        lines.push(serde_json::to_string(&json!({
            "type": "user", "uuid": uuid,
            "parentUuid": uuids.last(), "sessionId": session,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": call_id, "content": "x".repeat(size)}
            ]}
        })).unwrap());
        uuids.push(uuid);
    }
    lines
}

/// A random Codex rollout: session_meta, response_items with monotonic
/// ordinals, function_call_output payloads in both string and array
/// form, occasional compacted records with replacement_history.
fn gen_codex_transcript(tc: &TestCase) -> Vec<String> {
    let id = format!("c{}", tc.draw(gs::integers::<u32>()));
    let n = tc.draw(gs::integers::<usize>().min_value(3).max_value(24));
    let mut lines = vec![serde_json::to_string(&json!({
        "type": "session_meta",
        "payload": {"id": id, "timestamp": "2026-09-15T00:00:00Z", "cwd": "/tmp"}
    }))
    .unwrap()];
    for i in 0..n {
        let kind_idx = tc.draw(gs::integers::<usize>().max_value(3));
        let line = match kind_idx {
            // function_call_output with a string output body.
            0 => {
                let size = tc.draw(gs::integers::<usize>().max_value(1500));
                json!({
                    "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
                    "ordinal": i as i64,
                    "payload": {"type": "function_call_output", "call_id": format!("c{i}"), "output": "x".repeat(size)}
                })
            }
            // custom_tool_call_output with an array output body.
            1 => {
                let size = tc.draw(gs::integers::<usize>().max_value(1500));
                json!({
                    "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
                    "ordinal": i as i64,
                    "payload": {"type": "custom_tool_call_output", "call_id": format!("c{i}"),
                        "output": [{"type": "input_text", "text": "y".repeat(size)}]}
                })
            }
            // A compacted record carrying replacement_history items.
            2 => {
                let size = tc.draw(gs::integers::<usize>().max_value(900));
                json!({
                    "timestamp": "2026-09-15T00:00:00Z", "type": "compacted",
                    "ordinal": i as i64,
                    "payload": {"replacement_history": [
                        {"type": "function_call_output", "call_id": format!("h{i}"), "output": "z".repeat(size)},
                        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "earlier"}]}
                    ]}
                })
            }
            // A plain message record.
            _ => json!({
                "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
                "ordinal": i as i64,
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "hello"}]}
            }),
        };
        lines.push(serde_json::to_string(&line).unwrap());
    }
    lines
}

fn gen_digest(_tc: &TestCase, covers: usize) -> DigestBlock {
    DigestBlock {
        goal: Some("finish the refactor".to_string()),
        decisions: vec!["chose the append path".to_string()],
        files_touched: vec!["src/main.rs".to_string()],
        open_tasks: vec![],
        covers_items: covers,
    }
}

/// Draw `k` distinct line indexes in `0..n` (or fewer when n is small).
fn draw_line_indexes(tc: &TestCase, n: usize) -> Vec<usize> {
    let want = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
    let mut picks = Vec::new();
    for _ in 0..want.min(n) {
        let idx = tc.draw(gs::integers::<usize>().max_value(n - 1));
        if !picks.contains(&idx) {
            picks.push(idx);
        }
    }
    picks.sort_unstable();
    picks
}

/// The stateful property: any interleaving of elisions, digest injects,
/// vault restores, and provider appends leaves the transcript verify-clean
/// with linkage intact; a vault snapshot always restores byte-identical
/// bytes.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn surgery_preserves_linkage_and_verify_clean(tc: TestCase) {
    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("surgery");
    let path = dir.join(match provider {
        Provider::Codex => "rollout.jsonl",
        Provider::ClaudeCode => "session.jsonl",
    });
    let vault_root = dir.join("vault");

    let mut lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    no_errors(provider, &path);

    // Linkage baseline: index -> structural fields. Elide must not move
    // these; inject appends one new tip.
    let mut baseline: HashMap<usize, Option<Value>> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| (i, linkage(provider, l)))
        .collect();

    let steps = tc.draw(gs::integers::<usize>().min_value(1).max_value(12));
    for _ in 0..steps {
        match tc.draw(gs::integers::<u8>().max_value(3)) {
            // Elide a random subset of lines.
            0 => {
                let before = read_lines(&path);
                let indexes = draw_line_indexes(&tc, before.len());
                let edits = vec![Edit::Elide {
                    line_indexes: indexes.clone(),
                    stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
                }];
                let reclaimed = match provider {
                    Provider::Codex => codex::apply(&path, &edits).unwrap(),
                    Provider::ClaudeCode => claude::apply(&path, &edits).unwrap(),
                };
                let after = read_lines(&path);
                assert_eq!(
                    before.len(),
                    after.len(),
                    "elide changed the record count"
                );
                for (i, (old, new)) in before.iter().zip(after.iter()).enumerate() {
                    assert_eq!(
                        linkage(provider, old),
                        linkage(provider, new),
                        "linkage disturbed on line {i}"
                    );
                    if !indexes.contains(&i) {
                        assert_eq!(old, new, "untargeted line {i} was rewritten");
                    }
                }
                // Re-eliding the same lines is a no-op on the bytes.
                let again = match provider {
                    Provider::Codex => codex::apply(&path, &edits).unwrap(),
                    Provider::ClaudeCode => claude::apply(&path, &edits).unwrap(),
                };
                assert_eq!(read_lines(&path), after, "second elide not idempotent");
                let _ = reclaimed;
                let _ = again;
                no_errors(provider, &path);
            }
            // Inject a digest line (one appended tip, linkage untouched).
            1 => {
                let before = read_lines(&path);
                let edits = vec![Edit::InjectDigest {
                    digest: gen_digest(&tc, before.len()),
                }];
                match provider {
                    Provider::Codex => codex::apply(&path, &edits).unwrap(),
                    Provider::ClaudeCode => claude::apply(&path, &edits).unwrap(),
                };
                let after = read_lines(&path);
                assert_eq!(before.len() + 1, after.len(), "inject did not append exactly one line");
                for (i, (old, new)) in before.iter().zip(after.iter()).enumerate() {
                    assert_eq!(old, new, "inject rewrote existing line {i}");
                }
                no_errors(provider, &path);
            }
            // Vault snapshot, apply an elide, restore, compare bytes.
            2 => {
                let before = fs::read(&path).unwrap();
                let entry = vault::snapshot(
                    &path,
                    provider,
                    "hegel-session",
                    Some("hegel"),
                    &vault_root,
                )
                .unwrap();
                let indexes = draw_line_indexes(&tc, read_lines(&path).len());
                let edits = vec![Edit::Elide {
                    line_indexes: indexes,
                    stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
                }];
                match provider {
                    Provider::Codex => codex::apply(&path, &edits).unwrap(),
                    Provider::ClaudeCode => claude::apply(&path, &edits).unwrap(),
                };
                vault::restore(&entry.sha256, &path, &vault_root).unwrap();
                assert_eq!(
                    fs::read(&path).unwrap(),
                    before,
                    "vault restore did not reproduce byte-identical bytes"
                );
                no_errors(provider, &path);
            }
            // The provider keeps writing after our edit: append a fresh
            // well-formed record continuing the transcript, then re-verify.
            _ => {
                let count = read_lines(&path).len();
                let appended = match provider {
                    Provider::Codex => serde_json::to_string(&json!({
                        "timestamp": "2026-09-15T00:00:01Z", "type": "response_item",
                        "ordinal": count as i64,
                        "payload": {"type": "message", "role": "assistant",
                            "content": [{"type": "output_text", "text": "done"}]}
                    }))
                    .unwrap(),
                    Provider::ClaudeCode => {
                        // Parent onto the last uuid-bearing line so the chain stays valid.
                        let last_uuid = lines.iter().rev().find_map(|l| {
                            serde_json::from_str::<Value>(l)
                                .ok()
                                .and_then(|v| v.get("uuid").and_then(Value::as_str).map(str::to_string))
                        });
                        serde_json::to_string(&json!({
                            "type": "user", "uuid": format!("n{count}"),
                            "parentUuid": last_uuid, "sessionId": "s",
                            "message": {"role": "user", "content": "continue"}
                        }))
                        .unwrap()
                    }
                };
                lines.push(appended);
                baseline.insert(count, linkage(provider, &lines[count]));
                write_lines(&path, &lines);
                no_errors(provider, &path);
            }
        }
        // Refresh the local model for subsequent provider appends.
        lines = read_lines(&path);
    }
}

/// Regression for a shrunk failure: eliding a Codex `output` at or under
/// the 256-byte floor rewrote it into a stub *larger* than the original
/// and double-application churned `{bytes}` — so `watch` would rewrite the
/// file every pass and the claimed savings were negative. The rewrite must
/// leave under-floor outputs untouched.
#[test]
fn codex_small_output_is_not_stubbed() {
    let dir = tmpdir("small-output");
    let path = dir.join("rollout.jsonl");
    let output = "x".repeat(40);
    let line = serde_json::to_string(&json!({
        "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
        "ordinal": 0,
        "payload": {"type": "function_call_output", "call_id": "c0", "output": output}
    }))
    .unwrap();
    write_lines(&path, &[line]);
    let before = fs::read(&path).unwrap();
    let reclaimed = codex::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![0],
            stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(reclaimed, 0);
    assert_eq!(fs::read(&path).unwrap(), before);
}

/// Regression for a shrunk failure: re-applying an elide to already
/// stubbed outputs must be byte-identical. A second pass used to rewrite
/// the stub with a different `{bytes}` count.
#[test]
fn codex_elide_is_idempotent() {
    let dir = tmpdir("idempotent");
    let path = dir.join("rollout.jsonl");
    let line = serde_json::to_string(&json!({
        "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
        "ordinal": 0,
        "payload": {"type": "function_call_output", "call_id": "c0", "output": "x".repeat(1200)}
    }))
    .unwrap();
    write_lines(&path, &[line]);
    let edits = [Edit::Elide {
        line_indexes: vec![0],
        stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
    }];
    codex::apply(&path, &edits).unwrap();
    let once = fs::read(&path).unwrap();
    codex::apply(&path, &edits).unwrap();
    assert_eq!(fs::read(&path).unwrap(), once);
}

/// Out-of-range and duplicate indexes must not touch the file or panic.
#[test]
fn out_of_range_indexes_leave_file_byte_identical() {
    let dir = tmpdir("oor");
    let path = dir.join("rollout.jsonl");
    let line = serde_json::to_string(&json!({
        "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
        "ordinal": 0,
        "payload": {"type": "function_call_output", "call_id": "c0", "output": "x".repeat(1200)}
    }))
    .unwrap();
    write_lines(&path, &[line]);
    let before = fs::read(&path).unwrap();
    let reclaimed = codex::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![7, 7, usize::MAX],
            stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(reclaimed, 0);
    assert_eq!(fs::read(&path).unwrap(), before);
}

/// A malformed line targeted for elision is passed through verbatim —
/// surgery never guesses at content it cannot parse.
#[test]
fn malformed_target_line_passes_through() {
    let dir = tmpdir("malformed");
    let path = dir.join("session.jsonl");
    write_lines(
        &path,
        &[
            r#"{"type":"user","uuid":"u0","message":{"role":"user","content":"hi"}}"#.to_string(),
            "{not valid json at all".to_string(),
        ],
    );
    let before = fs::read(&path).unwrap();
    claude::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![0, 1],
            stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
}

/// Eliding a line that carries no elidable payload leaves the transcript
/// byte-identical — the surgery must not touch records it has nothing to
/// reclaim from.
#[hegel::test(test_cases = 64)]
fn elide_of_non_output_lines_is_byte_identical(tc: TestCase) {
    let dir = tmpdir("noop-elide");
    let path = dir.join("session.jsonl");
    let n = tc.draw(gs::integers::<usize>().min_value(2).max_value(10));
    let mut uuids: Vec<String> = Vec::new();
    let mut lines = Vec::new();
    for i in 0..n {
        let parent = uuids
            .last()
            .map(|p| json!(p))
            .unwrap_or(Value::Null);
        let uuid = format!("u{i}");
        let line = json!({
            "type": "user", "uuid": uuid, "parentUuid": parent, "sessionId": "s",
            "message": {"role": "user", "content": "plain text, nothing elidable"}
        });
        uuids.push(uuid);
        lines.push(serde_json::to_string(&line).unwrap());
    }
    write_lines(&path, &lines);
    let before = fs::read(&path).unwrap();
    let indexes = draw_line_indexes(&tc, n);
    claude::apply(
        &path,
        &[Edit::Elide {
            line_indexes: indexes,
            stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
        }],
    )
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
}

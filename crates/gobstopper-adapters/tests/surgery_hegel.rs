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

/// Generated payload text: mostly ascii filler straddling the elision
/// floor, sometimes unicode/escape-heavy content like real tool output.
fn gen_content(tc: &TestCase) -> String {
    if tc.draw(gs::weighted_booleans(0.2)) {
        let mut s = tc.draw(gs::text().max_size(400));
        // Keep payloads multi-line and awkward: quotes, escapes, unicode.
        s.push_str("\\n\"quoted\" — ünïcödé\t");
        s
    } else {
        "x".repeat(tc.draw(gs::integers::<usize>().max_value(1500)))
    }
}

/// A random Claude Code transcript: uuid/parentUuid chain, tool_use /
/// tool_result pairs, occasional sidechains, meta lines without linkage,
/// varying payload shapes and sizes.
fn gen_claude_transcript(tc: &TestCase) -> Vec<String> {
    let session = format!("s{}", tc.draw(gs::integers::<u32>()));
    let n = tc.draw(gs::integers::<usize>().min_value(3).max_value(24));
    let mut lines = Vec::new();
    let mut uuids: Vec<String> = Vec::new();
    let mut open_calls: Vec<String> = Vec::new();
    for i in 0..n {
        // Occasionally emit a linkage-free bookkeeping line (summary,
        // system, attachment) — Meta records must pass through surgery.
        if tc.draw(gs::weighted_booleans(0.15)) {
            let meta = match tc.draw(gs::integers::<u8>().max_value(2)) {
                0 => json!({"type": "summary", "summary": "earlier work"}),
                1 => json!({"type": "system", "content": "cwd changed"}),
                _ => json!({"type": "attachment", "attachment": {"files": 1}}),
            };
            lines.push(serde_json::to_string(&meta).unwrap());
            continue;
        }
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
                let content = gen_content(tc);
                let is_error = tc.draw(gs::weighted_booleans(0.1));
                // tool_result content arrives as a bare string or an
                // array of text blocks; both must survive surgery.
                let content = if tc.draw(gs::booleans()) {
                    json!(content)
                } else {
                    json!([{"type": "text", "text": content}])
                };
                let mut block = json!({
                    "type": "tool_result", "tool_use_id": call_id, "content": content
                });
                if is_error {
                    block["is_error"] = json!(true);
                }
                json!({
                    "type": "user", "uuid": uuid, "parentUuid": parent,
                    "sessionId": session, "isSidechain": sidechain,
                    "message": {"role": "user", "content": [block]}
                })
            }
            "assistant_text" => {
                let mut record = json!({
                    "type": "assistant", "uuid": uuid, "parentUuid": parent,
                    "sessionId": session, "isSidechain": sidechain,
                    "message": {"role": "assistant", "content": [
                        {"type": "text", "text": "working on it"}
                    ]}
                });
                if tc.draw(gs::booleans()) {
                    record["message"]["usage"] = json!({
                        "input_tokens": 100 + i as u64,
                        "cache_read_input_tokens": 5000,
                        "cache_creation_input_tokens": 0,
                        "output_tokens": 50
                    });
                }
                if tc.draw(gs::weighted_booleans(0.2)) {
                    record["message"]["content"]
                        .as_array_mut()
                        .unwrap()
                        .insert(0, json!({"type": "thinking", "thinking": "hmm"}));
                }
                record
            }
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
/// ordinals, function_call / output payloads in both string and array
/// form, bookkeeping records, occasional compacted records with
/// replacement_history — optionally carrying the window-chain envelope a
/// real provider compaction leaves behind.
fn gen_codex_transcript(tc: &TestCase, with_windows: bool) -> Vec<String> {
    let id = format!("c{}", tc.draw(gs::integers::<u32>()));
    let n = tc.draw(gs::integers::<usize>().min_value(3).max_value(24));
    let mut lines = vec![serde_json::to_string(&json!({
        "type": "session_meta",
        "payload": {"id": id, "timestamp": "2026-09-15T00:00:00Z", "cwd": "/tmp"}
    }))
    .unwrap()];
    let mut compact_round = 0u64;
    for i in 0..n {
        let kind_idx = tc.draw(gs::integers::<usize>().max_value(6));
        let line = match kind_idx {
            // function_call_output with a string output body.
            0 => json!({
                "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
                "ordinal": i as i64,
                "payload": {"type": "function_call_output", "call_id": format!("c{i}"),
                    "output": gen_content(tc)}
            }),
            // custom_tool_call_output with an array output body.
            1 => json!({
                "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
                "ordinal": i as i64,
                "payload": {"type": "custom_tool_call_output", "call_id": format!("c{i}"),
                    "output": [{"type": "input_text", "text": gen_content(tc)}]}
            }),
            // A compacted record carrying replacement_history items —
            // sometimes with the full window-chain envelope.
            2 => {
                compact_round += 1;
                let mut payload = json!({"replacement_history": [
                    {"type": "function_call_output", "call_id": format!("h{i}"), "output": gen_content(tc)},
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "earlier"}]}
                ]});
                if with_windows {
                    let w = format!("w{i}");
                    payload["window_number"] = json!(compact_round);
                    payload["first_window_id"] = json!("first-w");
                    payload["previous_window_id"] = json!(if compact_round == 1 { "first-w" } else { "w-prev" });
                    payload["window_id"] = json!(w);
                }
                json!({
                    "timestamp": "2026-09-15T00:00:00Z", "type": "compacted",
                    "ordinal": i as i64, "payload": payload
                })
            }
            // The call side of a tool pair — may stay unpaired (a
            // warning, not an error, in verify's contract).
            3 => json!({
                "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
                "ordinal": i as i64,
                "payload": {"type": "function_call", "name": "shell",
                    "call_id": format!("call{i}"), "arguments": "{}"}
            }),
            // Bookkeeping records that ride along: turn_context and
            // token_usage_record carry no elidable output.
            4 => json!({
                "timestamp": "2026-09-15T00:00:00Z", "type": "turn_context",
                "ordinal": i as i64,
                "payload": {"cwd": "/tmp", "model": "gpt-5"}
            }),
            5 => json!({
                "timestamp": "2026-09-15T00:00:00Z", "type": "token_usage_record",
                "payload": {"thread_id": id, "turn_id": format!("t{i}"),
                    "usage": {"input_tokens": 100 * i as u64, "output_tokens": 5}}
            }),
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

/// Mirror of `claude::load`'s live-branch walk: the set of uuid-bearing
/// (linked) line indexes, and the subset reachable from the latest leaf
/// by following parentUuid. Dead branches stay provably dead.
fn claude_live_lines(
    lines: &[String],
) -> (std::collections::HashSet<usize>, std::collections::HashSet<usize>) {
    use std::collections::{HashMap, HashSet};
    let mut links: Vec<(usize, String, Option<String>)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Ok(rec) = serde_json::from_str::<Value>(line) {
            if let Some(uuid) = rec.get("uuid").and_then(Value::as_str) {
                links.push((
                    i,
                    uuid.to_string(),
                    rec.get("parentUuid")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ));
            }
        }
    }
    let linked: HashSet<usize> = links.iter().map(|(l, _, _)| *l).collect();
    let mut live = HashSet::new();
    if links.is_empty() {
        return (linked, live);
    }
    let parent_of: HashMap<&str, Option<&str>> = links
        .iter()
        .map(|(_, u, p)| (u.as_str(), p.as_deref()))
        .collect();
    let line_of: HashMap<&str, usize> = links
        .iter()
        .map(|(l, u, _)| (u.as_str(), *l))
        .collect();
    let mut cursor = Some(links.last().unwrap().1.as_str());
    let mut steps = 0usize;
    while let Some(uuid) = cursor {
        if let Some(&line) = line_of.get(uuid) {
            live.insert(line);
        }
        cursor = parent_of.get(uuid).copied().flatten();
        steps += 1;
        if steps > links.len() {
            break;
        }
    }
    (linked, live)
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
        Provider::Codex => gen_codex_transcript(&tc, tc.draw(gs::booleans())),
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

/// When the rewrite cannot be staged (read-only directory), apply must
/// fail and leave the original file byte-identical — never a truncated or
/// partially written transcript.
#[cfg(unix)]
#[test]
fn failed_write_preserves_original() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("readonly");
    let path = dir.join("rollout.jsonl");
    let line = serde_json::to_string(&json!({
        "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
        "ordinal": 0,
        "payload": {"type": "function_call_output", "call_id": "c0", "output": "x".repeat(1200)}
    }))
    .unwrap();
    write_lines(&path, &[line]);
    let before = fs::read(&path).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let result = codex::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![0],
            stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
        }],
    );
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
}

/// A transcript whose last line lacks a trailing newline must not gain
/// one — byte-level fidelity of untouched structure is part of the
/// not-breaking-sessions contract.
#[test]
fn missing_trailing_newline_is_preserved() {
    let dir = tmpdir("nonewline");
    let path = dir.join("rollout.jsonl");
    let line = serde_json::to_string(&json!({
        "timestamp": "2026-09-15T00:00:00Z", "type": "response_item",
        "ordinal": 0,
        "payload": {"type": "function_call_output", "call_id": "c0", "output": "x".repeat(1200)}
    }))
    .unwrap();
    fs::write(&path, &line).unwrap(); // no trailing newline
    codex::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![0],
            stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
        }],
    )
    .unwrap();
    let after = fs::read_to_string(&path).unwrap();
    assert!(!after.ends_with('\n'), "rewrite added a trailing newline");
}

/// End-to-end property: for any generated transcript, every strategy that
/// fires must emit only in-bounds line indexes, and applying the full
/// plan leaves the file verify-clean with linkage intact. `ProviderCompact`
/// edits are skipped here — they are routed to the provider, not the file.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn plans_apply_cleanly(tc: TestCase) {
    use gobstopper_core::model::SessionHandle;
    use gobstopper_core::strategy::{builtin_strategies, PolicyConfig};

    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("plan-apply");
    let path = dir.join("session.jsonl");
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, tc.draw(gs::booleans())),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);

    let handle = SessionHandle {
        provider,
        session_id: "hegel".to_string(),
        path: path.clone(),
        cwd: None,
        age_secs: 0,
    };
    let transcript = match provider {
        Provider::Codex => codex::load(handle).unwrap(),
        Provider::ClaudeCode => claude::load(handle).unwrap(),
    };
    let line_count = read_lines(&path).len();
    let policy = PolicyConfig {
        trigger_tokens: 100,
        floor_tokens: 10,
        keep_recent_tool_outputs: 1,
        min_interval_secs: 0,
        quota_pressure: gobstopper_core::strategy::QuotaPressure::Normal,
    };

    for strategy in builtin_strategies() {
        // Each strategy applies against a fresh copy: plans are computed
        // from the same transcript and must not interact.
        let copy = dir.join(format!("{}.jsonl", strategy.id()));
        fs::copy(&path, &copy).unwrap();
        let Some(plan) = strategy.evaluate(&transcript, &policy) else {
            continue;
        };
        for edit in &plan.edits {
            if let Edit::Elide { line_indexes, .. } = edit {
                for &i in line_indexes {
                    assert!(
                        i < line_count,
                        "strategy {} emitted out-of-bounds index {i} (file has {line_count} lines)",
                        strategy.id()
                    );
                    // A linked line on a dead Claude branch is never a
                    // target: load() strips its elidable_bytes.
                    if provider == Provider::ClaudeCode {
                        let (linked, live) = claude_live_lines(&lines);
                        assert!(
                            !linked.contains(&i) || live.contains(&i),
                            "strategy {} targeted dead-branch line {i}",
                            strategy.id()
                        );
                    }
                }
            }
        }
        let file_edits: Vec<Edit> = plan
            .edits
            .into_iter()
            .filter(|e| !matches!(e, Edit::ProviderCompact { .. }))
            .collect();
        match provider {
            Provider::Codex => codex::apply(&copy, &file_edits).unwrap(),
            Provider::ClaudeCode => claude::apply(&copy, &file_edits).unwrap(),
        };
        no_errors(provider, &copy);

        // Watch-loop fixpoint: reload the surgically edited transcript
        // and let the same strategy plan again — a second round must
        // still apply verify-clean (or stop planning entirely).
        let mut current_lines = read_lines(&copy);
        for _ in 0..2 {
            let handle = SessionHandle {
                provider,
                session_id: "hegel".to_string(),
                path: copy.clone(),
                cwd: None,
                age_secs: 0,
            };
            let t2 = match provider {
                Provider::Codex => codex::load(handle).unwrap(),
                Provider::ClaudeCode => claude::load(handle).unwrap(),
            };
            let Some(plan2) = strategy.evaluate(&t2, &policy) else {
                break;
            };
            for edit in &plan2.edits {
                if let Edit::Elide { line_indexes, .. } = edit {
                    for &i in line_indexes {
                        assert!(
                            i < current_lines.len(),
                            "round-2 plan out of bounds: {i} ({} lines)",
                            current_lines.len()
                        );
                    }
                }
            }
            let file_edits2: Vec<Edit> = plan2
                .edits
                .into_iter()
                .filter(|e| !matches!(e, Edit::ProviderCompact { .. }))
                .collect();
            match provider {
                Provider::Codex => codex::apply(&copy, &file_edits2).unwrap(),
                Provider::ClaudeCode => claude::apply(&copy, &file_edits2).unwrap(),
            };
            no_errors(provider, &copy);
            current_lines = read_lines(&copy);
        }
    }
}

/// Dirty-transcript property: real transcripts carry torn tail writes,
/// blank lines, mid-file garbage, and non-object JSON. Surgery must pass
/// every such line through byte-identically and must never introduce a
/// NEW verify finding — findings after ⊆ findings before.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn dirty_transcript_surgery_adds_no_findings(tc: TestCase) {
    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("dirty");
    let path = dir.join("session.jsonl");
    let mut lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    // Inject k dirty lines at random positions.
    let junk = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
    for _ in 0..junk {
        let pos = tc.draw(gs::integers::<usize>().max_value(lines.len()));
        let line = match tc.draw(gs::integers::<u8>().max_value(4)) {
            0 => "{bad json mid-file".to_string(),
            1 => "   ".to_string(),
            2 => "42".to_string(),
            3 => "[1, 2, 3]".to_string(),
            _ => r#""just a string""#.to_string(),
        };
        lines.insert(pos, line);
    }
    write_lines(&path, &lines);

    // load() must tolerate dirty lines too — it skips what it can't parse.
    let handle = gobstopper_core::model::SessionHandle {
        provider,
        session_id: "dirty".to_string(),
        path: path.clone(),
        cwd: None,
        age_secs: 0,
    };
    match provider {
        Provider::Codex => codex::load(handle).unwrap(),
        Provider::ClaudeCode => claude::load(handle).unwrap(),
    };
    let before: std::collections::HashSet<(&'static str, Option<usize>)> =
        verify::verify(provider, &fs::read(&path).unwrap())
            .iter()
            .map(|f| (f.code, f.line_index))
            .collect();
    let indexes = draw_line_indexes(&tc, lines.len());
    match provider {
        Provider::Codex => codex::apply(
            &path,
            &[Edit::Elide {
                line_indexes: indexes,
                stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
            }],
        )
        .unwrap(),
        Provider::ClaudeCode => claude::apply(
            &path,
            &[Edit::Elide {
                line_indexes: indexes,
                stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
            }],
        )
        .unwrap(),
    };
    let after_lines = read_lines(&path);
    assert_eq!(lines.len(), after_lines.len());
    // Every dirty line survived verbatim: the dirty candidates we know
    // we wrote must appear at their positions unchanged.
    for (i, (want, got)) in lines.iter().zip(after_lines.iter()).enumerate() {
        if serde_json::from_str::<Value>(want).is_err()
            || !want.trim_start().starts_with('{')
        {
            assert_eq!(want, got, "dirty line {i} was not passed through verbatim");
        }
    }
    let after: std::collections::HashSet<(&'static str, Option<usize>)> =
        verify::verify(provider, &fs::read(&path).unwrap())
            .iter()
            .map(|f| (f.code, f.line_index))
            .collect();
    let new_findings: Vec<_> = after.difference(&before).collect();
    assert!(
        new_findings.is_empty(),
        "surgery introduced new verify findings: {new_findings:?}"
    );
}

/// Stale-plan property: `watch` plans at time T and applies later. The
/// provider appends records in between — appends never shift line
/// indexes, so the plan's targets are still the intended lines.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn stale_plan_survives_provider_appends(tc: TestCase) {
    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("stale-plan");
    let path = dir.join("session.jsonl");
    let mut lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    // Plan against the current file.
    let indexes = draw_line_indexes(&tc, lines.len());
    // The provider keeps writing: append k well-formed records.
    let appends = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
    for _ in 0..appends {
        let count = lines.len();
        let appended = match provider {
            Provider::Codex => serde_json::to_string(&json!({
                "timestamp": "t", "type": "response_item", "ordinal": count as i64,
                "payload": {"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "more work"}]}
            }))
            .unwrap(),
            Provider::ClaudeCode => {
                let last_uuid = lines.iter().rev().find_map(|l| {
                    serde_json::from_str::<Value>(l)
                        .ok()
                        .and_then(|v| v.get("uuid").and_then(Value::as_str).map(str::to_string))
                });
                serde_json::to_string(&json!({
                    "type": "user", "uuid": format!("a{count}"),
                    "parentUuid": last_uuid, "sessionId": "s",
                    "message": {"role": "user", "content": "continue"}
                }))
                .unwrap()
            }
        };
        lines.push(appended);
    }
    write_lines(&path, &lines);
    match provider {
        Provider::Codex => codex::apply(
            &path,
            &[Edit::Elide {
                line_indexes: indexes.clone(),
                stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
            }],
        )
        .unwrap(),
        Provider::ClaudeCode => claude::apply(
            &path,
            &[Edit::Elide {
                line_indexes: indexes.clone(),
                stub_template: "[elided {bytes} bytes of {kind}]".to_string(),
            }],
        )
        .unwrap(),
    };
    let after = read_lines(&path);
    assert_eq!(lines.len(), after.len());
    // The appended tail lines survived untouched.
    for (want, got) in lines.iter().zip(after.iter()).skip(lines.len() - appends) {
        assert_eq!(want, got, "provider-appended tail line was disturbed");
    }
    // The planned targets were the lines we meant to hit: each targeted
    // line is either still its original bytes (nothing to reclaim) or
    // the stubbed form of exactly that line — never an unrelated record.
    for &i in &indexes {
        if after[i] != lines[i] {
            assert!(
                after[i].contains("elided"),
                "targeted line {i} changed to something that is not a stub"
            );
        }
    }
    no_errors(provider, &path);
}

/// The experimental compacted-record writer over generated rollouts:
/// every appended record must carry a valid window chain — monotonic
/// window_number, previous_window_id == prior window_id, stable
/// first_window_id, unique window_id — and the file must stay
/// verify-clean across repeated compactions.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn compacted_records_chain_cleanly(tc: TestCase) {
    use gobstopper_adapters::codex_compact;

    let dir = tmpdir("compact-chain");
    let path = dir.join("rollout.jsonl");
    let lines = gen_codex_transcript(&tc, tc.draw(gs::booleans()));
    write_lines(&path, &lines);
    no_errors(Provider::Codex, &path);

    let mut seen_windows: Vec<String> = Vec::new();
    let rounds = tc.draw(gs::integers::<usize>().min_value(1).max_value(3));
    for _ in 0..rounds {
        let before = codex_compact::read_rollout_lines(&path).unwrap();
        let keep = tc.draw(gs::integers::<usize>().max_value(4));
        compact_with_digest_entry(&path, keep);
        let after = codex_compact::read_rollout_lines(&path).unwrap();
        assert_eq!(before.len() + 1, after.len(), "compaction did not append exactly one record");
        // Untouched prefix: every prior line is byte-identical.
        for (i, (a, b)) in before.iter().zip(after.iter()).enumerate() {
            assert_eq!(a, b, "compaction rewrote existing line {i}");
        }
        let rec: Value = serde_json::from_str(after.last().unwrap()).unwrap();
        assert_eq!(rec["type"], "compacted");
        assert_eq!(rec["ordinal"], before.len() as u64);
        let payload = &rec["payload"];
        let history = payload["replacement_history"].as_array().unwrap();
        assert!(!history.is_empty());
        assert_eq!(history[0]["type"], "message");
        assert_eq!(history[0]["role"], "user");

        // The chain extends the file's last compacted record (provider
        // or gobstopper-written), exactly as the provider's own writer does.
        let prior = before.iter().rev().find_map(|l| {
            let r: Value = serde_json::from_str(l).ok()?;
            (r.get("type")?.as_str()? == "compacted").then(|| r.get("payload").cloned())?
        });
        let number = payload["window_number"].as_u64().unwrap();
        let expected_number = prior
            .as_ref()
            .and_then(|p| p.get("window_number").and_then(Value::as_u64))
            .unwrap_or(0)
            + 1;
        assert_eq!(number, expected_number, "window_number did not advance by one");
        let first = payload["first_window_id"].as_str().unwrap();
        if let Some(ef) = prior
            .as_ref()
            .and_then(|p| p.get("first_window_id").and_then(Value::as_str))
        {
            assert_eq!(first, ef, "first_window_id drifted");
        }
        let prev_wid = payload["previous_window_id"].as_str().unwrap();
        let expected_prev = prior
            .as_ref()
            .and_then(|p| p.get("window_id").and_then(Value::as_str))
            .unwrap_or(first);
        assert_eq!(prev_wid, expected_prev, "previous_window_id is not the prior window_id");
        let wid = payload["window_id"].as_str().unwrap().to_string();
        assert!(!seen_windows.contains(&wid), "window_id reused: {wid}");
        seen_windows.push(wid);
        no_errors(Provider::Codex, &path);
    }
}

fn compact_with_digest_entry(path: &Path, keep_tail: usize) {
    gobstopper_adapters::codex_compact::compact_with_digest(
        path,
        &DigestBlock {
            goal: Some("g".to_string()),
            decisions: vec![],
            files_touched: vec![],
            open_tasks: vec![],
            covers_items: 0,
        },
        keep_tail,
    )
    .unwrap();
}

/// verify itself is a safety net — it must return findings, never panic,
/// on arbitrary bytes.
#[hegel::test(test_cases = 64)]
fn verify_never_panics_on_arbitrary_bytes(tc: TestCase) {
    let bytes = tc.draw(gs::binary().max_size(2048));
    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let findings = verify::verify(provider, &bytes);
    // Findings never carry transcript bytes — messages are counts and
    // line indexes only.
    for f in &findings {
        assert!(f.message.len() < 200);
    }
}

/// `eval` runs every strategy against temp copies: the source file must
/// come out byte-identical, and each row's plan (when present) stays
/// in-bounds with internally consistent finding counts.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn eval_never_mutates_source(tc: TestCase) {
    use gobstopper_adapters::eval;
    use gobstopper_core::strategy::PolicyConfig;

    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("eval");
    let path = dir.join("session.jsonl");
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    let before = fs::read(&path).unwrap();

    let policy = PolicyConfig {
        trigger_tokens: 100,
        floor_tokens: 10,
        keep_recent_tool_outputs: 1,
        min_interval_secs: 0,
        quota_pressure: gobstopper_core::strategy::QuotaPressure::Normal,
    };
    let rows = eval::eval_transcript(provider, &path, &policy, None).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before, "eval rewrote the source transcript");
    assert!(!rows.is_empty());
    for row in &rows {
        assert_eq!(
            row.verify_errors,
            row.findings
                .iter()
                .filter(|f| f.severity == verify::Severity::Error)
                .count(),
            "row {} has inconsistent error rollup",
            row.strategy
        );
        assert_eq!(
            row.verify_warnings,
            row.findings
                .iter()
                .filter(|f| f.severity == verify::Severity::Warning)
                .count(),
            "row {} has inconsistent warning rollup",
            row.strategy
        );
        if let Some(plan) = &row.plan {
            for edit in &plan.edits {
                if let Edit::Elide { line_indexes, .. } = edit {
                    for &i in line_indexes {
                        assert!(i < lines.len(), "eval row {} out of bounds", row.strategy);
                    }
                }
            }
        }
    }
}

/// Fork is additive-only: the source stays byte-identical, the sibling
/// keeps uuid/parentUuid linkage and verifies clean, session ids are
/// rewritten, and a second fork to the same id refuses rather than
/// overwriting.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn fork_is_additive_and_linkage_preserving(tc: TestCase) {
    use gobstopper_adapters::fork;

    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("fork");
    let path = dir.join(match provider {
        Provider::Codex => "rollout-2026-09-15T00-00-00-c0.jsonl",
        Provider::ClaudeCode => "session.jsonl",
    });
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    let original = fs::read(&path).unwrap();

    let result = fork::fork(provider, &path, None).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original, "fork modified the source");
    let forked = read_lines(&result.path);
    assert_eq!(forked.len(), lines.len(), "fork changed the record count");
    for (i, (a, b)) in lines.iter().zip(forked.iter()).enumerate() {
        // Forked lines must equal originals except session-identity
        // fields — mask those and require full record equality, which
        // covers linkage (uuid/parentUuid/ordinal/call_id) and more.
        let mut ra: Value = serde_json::from_str(a).unwrap();
        let mut rb: Value = serde_json::from_str(b).unwrap();
        match provider {
            Provider::ClaudeCode => {
                ra["sessionId"] = Value::Null;
                rb["sessionId"] = Value::Null;
            }
            Provider::Codex => {
                for r in [&mut ra, &mut rb] {
                    if r.get("type").and_then(Value::as_str) == Some("session_meta") {
                        if let Some(p) = r.get_mut("payload") {
                            p["id"] = Value::Null;
                            p["session_id"] = Value::Null;
                        }
                    }
                }
            }
        }
        assert_eq!(ra, rb, "fork changed non-identity fields on line {i}");
        // Session identity must be rewritten to the new id.
        let rec: Value = serde_json::from_str(b).unwrap();
        match provider {
            Provider::ClaudeCode => {
                if let Some(sid) = rec.get("sessionId") {
                    assert_eq!(
                        sid.as_str().unwrap(),
                        result.session_id,
                        "fork left a stale sessionId on line {i}"
                    );
                }
            }
            Provider::Codex => {
                if rec.get("type").and_then(Value::as_str) == Some("session_meta") {
                    let id = rec.pointer("/payload/id").and_then(Value::as_str);
                    assert_eq!(id, Some(result.session_id.as_str()));
                }
            }
        }
    }
    no_errors(provider, &result.path);

    // A second fork with an explicit colliding id must refuse, not overwrite.
    let again = fork::fork(provider, &path, Some(result.session_id.clone()));
    assert!(again.is_err(), "fork silently overwrote an existing sibling");
    assert_eq!(read_lines(&result.path), forked);
}

/// Vault: snapshot → mutate → restore reproduces byte-identical bytes;
/// identical content dedups to the same object; the index keeps both
/// entries so `latest_for` sees the newest snapshot.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn vault_roundtrip_dedups_and_restores_exactly(tc: TestCase) {
    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("vault");
    let path = dir.join("session.jsonl");
    let vault_root = dir.join("vault");
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    let original = fs::read(&path).unwrap();

    let e1 = vault::snapshot(&path, provider, "s", Some("a"), &vault_root).unwrap();
    let e2 = vault::snapshot(&path, provider, "s", Some("b"), &vault_root).unwrap();
    assert_eq!(e1.sha256, e2.sha256, "identical content produced different digests");

    // Mutate, then restore — the original bytes come back exactly.
    fs::write(&path, "garbage-not-jsonl\n").unwrap();
    let restored = vault::restore(&e1.sha256, &path, &vault_root).unwrap();
    assert_eq!(restored.sha256, e1.sha256);
    assert_eq!(fs::read(&path).unwrap(), original, "restore did not reproduce exact bytes");

    // The index holds both snapshots; latest_for finds this path's newest.
    let entries = vault::list(&vault_root).unwrap();
    assert!(entries.iter().filter(|e| e.sha256 == e1.sha256).count() >= 2);
    let latest = vault::latest_for(&path, &vault_root).unwrap().unwrap();
    assert_eq!(latest.sha256, e1.sha256);
}

/// `tail_response_items` takes the last n response_item payloads in file
/// order; `digest_to_replacement_history` puts the digest user-message
/// first and the tail verbatim after.
#[hegel::test(test_cases = 64)]
fn tail_and_history_shapes_hold(tc: TestCase) {
    use gobstopper_adapters::codex_compact;

    let lines = gen_codex_transcript(&tc, false);
    let n = tc.draw(gs::integers::<usize>().max_value(8));
    let tail = codex_compact::tail_response_items(&lines, n);

    // Expected: payloads of the last n response_item records, in order.
    let expected: Vec<Value> = lines
        .iter()
        .filter_map(|l| {
            let rec: Value = serde_json::from_str(l).ok()?;
            (rec.get("type")?.as_str()? == "response_item")
                .then(|| rec.get("payload").cloned())?
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    assert_eq!(tail, expected);

    let digest = gen_digest(&tc, tail.len());
    let history = codex_compact::digest_to_replacement_history(&digest, &tail);
    assert_eq!(history.len(), tail.len() + 1);
    assert_eq!(history[0]["type"], "message");
    assert_eq!(history[0]["role"], "user");
    assert!(history[0]["id"].as_str().unwrap().starts_with("msg_"));
    assert_eq!(&history[1..], &tail[..], "tail items were not verbatim");
}

/// A digest carrying hostile content — newlines, quotes, unicode,
/// braces — must still land as exactly one valid JSONL line.
#[hegel::test(test_cases = 64)]
fn hostile_digest_injects_as_one_line(tc: TestCase) {
    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("hostile-digest");
    let path = dir.join("session.jsonl");
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    let digest = DigestBlock {
        goal: Some(tc.draw(gs::text().max_size(120))),
        decisions: vec![format!("weird\nnewline \"quotes\" {}", tc.draw(gs::text().max_size(80)))],
        files_touched: vec!["{not json}\r\n../escape".to_string()],
        open_tasks: vec![],
        covers_items: lines.len(),
    };
    let before = read_lines(&path).len();
    match provider {
        Provider::Codex => codex::apply(&path, &[Edit::InjectDigest { digest }]).unwrap(),
        Provider::ClaudeCode => claude::apply(&path, &[Edit::InjectDigest { digest }]).unwrap(),
    };
    let after = read_lines(&path);
    assert_eq!(before + 1, after.len(), "digest injected more than one line");
    let last: Value = serde_json::from_str(after.last().unwrap())
        .expect("injected line is not valid JSON");
    assert!(last.is_object());
    no_errors(provider, &path);
}

/// Transcript-model invariants strategies rely on: item line_indexes are
/// real file positions, elidable_bytes never exceeds the line's size,
/// and provably-dead Claude branches carry no elidable bytes and no cost.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn load_item_invariants_hold(tc: TestCase) {
    use gobstopper_core::model::SessionHandle;

    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("load-inv");
    let path = dir.join("session.jsonl");
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    let handle = SessionHandle {
        provider,
        session_id: "s".to_string(),
        path: path.clone(),
        cwd: None,
        age_secs: 0,
    };
    let t = match provider {
        Provider::Codex => codex::load(handle).unwrap(),
        Provider::ClaudeCode => claude::load(handle).unwrap(),
    };
    let (linked, live) = if provider == Provider::ClaudeCode {
        claude_live_lines(&lines)
    } else {
        Default::default()
    };
    let mut seen_indexes = std::collections::HashSet::new();
    for item in &t.items {
        assert!(item.line_index < lines.len(), "item index out of bounds");
        assert!(
            seen_indexes.insert(item.line_index),
            "duplicate item for line {}",
            item.line_index
        );
        if let Some(b) = item.elidable_bytes {
            assert!(
                b as usize <= lines[item.line_index].len() + 64,
                "elidable_bytes {b} exceeds line size at {}",
                item.line_index
            );
        }
        if provider == Provider::ClaudeCode
            && linked.contains(&item.line_index)
            && !live.is_empty()
            && !live.contains(&item.line_index)
        {
            assert_eq!(
                item.elidable_bytes, None,
                "dead-branch line {} stayed elidable",
                item.line_index
            );
            assert_eq!(item.est_tokens, 0);
        }
    }
}

/// The fast tail-scan used by `detect` must agree with the full `load`
/// usage pass on files smaller than the tail window — a divergence means
/// watch would plan against one estimate and report another.
#[hegel::test(test_cases = 64, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn scan_usage_agrees_with_load(tc: TestCase) {
    use gobstopper_core::model::SessionHandle;

    let provider = if tc.draw(gs::booleans()) {
        Provider::Codex
    } else {
        Provider::ClaudeCode
    };
    let dir = tmpdir("scan-agree");
    let path = dir.join("session.jsonl");
    let lines = match provider {
        Provider::Codex => gen_codex_transcript(&tc, false),
        Provider::ClaudeCode => gen_claude_transcript(&tc),
    };
    write_lines(&path, &lines);
    assert!(
        fs::metadata(&path).unwrap().len() < 512 * 1024,
        "generator exceeded the tail window"
    );
    let scanned = match provider {
        Provider::Codex => codex::scan_usage(&path),
        Provider::ClaudeCode => claude::scan_usage(&path),
    };
    let loaded = match provider {
        Provider::Codex => codex::load(SessionHandle {
            provider,
            session_id: "s".into(),
            path: path.clone(),
            cwd: None,
            age_secs: 0,
        })
        .unwrap(),
        Provider::ClaudeCode => claude::load(SessionHandle {
            provider,
            session_id: "s".into(),
            path: path.clone(),
            cwd: None,
            age_secs: 0,
        })
        .unwrap(),
    };
    assert_eq!(
        scanned.context_tokens, loaded.usage.context_tokens,
        "tail-scan and full-load context estimates diverged"
    );
}

/// `discover` must surface every generated session under the roots
/// exactly once, with the right provider and the recovered session id;
/// `find` matches on the id prefix.
#[hegel::test(test_cases = 32, suppress_health_check = [hegel::HealthCheck::TooSlow])]
fn discover_finds_generated_sessions(tc: TestCase) {
    use gobstopper_adapters::detect::{self, Roots};

    let dir = tmpdir("discover");
    let roots = Roots {
        codex_home: dir.join("codex"),
        claude_home: dir.join("claude"),
    };
    let n_codex = tc.draw(gs::integers::<usize>().max_value(3));
    let n_claude = tc.draw(gs::integers::<usize>().max_value(3));
    let mut expected: Vec<(Provider, String, PathBuf)> = Vec::new();
    for i in 0..n_codex {
        let id = format!("uniq-c{i}");
        let p = roots
            .codex_home
            .join("sessions/2026/09/15")
            .join(format!("rollout-2026-09-15T00-00-00-{id}.jsonl"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut lines = gen_codex_transcript(&tc, false);
        // Rewrite the session_meta id to our unique one.
        lines[0] = serde_json::to_string(&json!({
            "type": "session_meta",
            "payload": {"id": id, "timestamp": "t", "cwd": "/tmp"}
        }))
        .unwrap();
        write_lines(&p, &lines);
        expected.push((Provider::Codex, id, p));
    }
    for i in 0..n_claude {
        let id = format!("uniq-k{i}");
        let p = roots
            .claude_home
            .join("projects/proj-a")
            .join(format!("{id}.jsonl"));
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut lines = gen_claude_transcript(&tc);
        // Rewrite every sessionId to the unique id.
        lines = lines
            .iter()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                if v.get("sessionId").is_some() {
                    v["sessionId"] = json!(id);
                }
                serde_json::to_string(&v).unwrap()
            })
            .collect();
        write_lines(&p, &lines);
        expected.push((Provider::ClaudeCode, id, p));
    }

    let found = detect::discover(&roots, 0);
    for (provider, id, path) in &expected {
        let hits: Vec<_> = found.iter().filter(|d| d.handle.path == *path).collect();
        assert_eq!(hits.len(), 1, "{id} discovered {} times", hits.len());
        let d = hits[0];
        assert_eq!(d.handle.provider, *provider);
        assert_eq!(d.handle.session_id, *id, "session id not recovered for {id}");
    }
    // Nothing extra surfaced.
    assert_eq!(found.len(), expected.len());

    // find() matches on id prefix.
    if let Some((provider, id, path)) = expected.first() {
        let hits = detect::find(&roots, id);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].handle.path, *path);
        assert_eq!(hits[0].handle.provider, *provider);
    }
}

/// A compacted record whose replacement_history outputs are all below
/// the stub floor must read as non-elidable in `load` and must pass
/// through `apply` byte-identically — otherwise the estimate and the
/// effect diverge and watch rewrites the line forever for zero gain.
#[test]
fn compacted_all_small_outputs_never_rewritten() {
    use gobstopper_core::model::SessionHandle;

    let dir = tmpdir("compacted-floor");
    let path = dir.join("session.jsonl");
    let lines = vec![
        serde_json::to_string(&json!({
            "type": "session_meta",
            "payload": {"id": "c1", "timestamp": "t", "cwd": "/tmp"}
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "compacted", "ordinal": 1,
            "payload": {"replacement_history": [
                {"type": "function_call_output", "call_id": "h1", "output": "x".repeat(100)},
                {"type": "function_call_output", "call_id": "h2", "output": "y".repeat(100)},
                {"type": "function_call_output", "call_id": "h3", "output": "z".repeat(100)}
            ]}
        }))
        .unwrap(),
    ];
    write_lines(&path, &lines);
    let handle = SessionHandle {
        provider: Provider::Codex,
        session_id: "c1".into(),
        path: path.clone(),
        cwd: None,
        age_secs: 0,
    };
    let t = codex::load(handle).unwrap();
    let compacted = t
        .items
        .iter()
        .find(|i| i.label == "compacted@1")
        .expect("compacted item");
    assert_eq!(
        compacted.elidable_bytes, None,
        "300 bytes across 3 sub-floor outputs is not elidable"
    );
    let before = fs::read(&path).unwrap();
    codex::apply(
        &path,
        &[Edit::Elide {
            line_indexes: vec![1],
            stub_template: "[elided {bytes} bytes of {kind}]".into(),
        }],
    )
    .unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "no-op elide must not even re-serialize the record"
    );
}

/// The digest is a state card, not a lossy copy: unique marker strings
/// planted in elided outputs must never appear inside the injected
/// digest line. If a strategy ever started copying payload text into
/// DigestBlock fields, this is the tripwire.
#[test]
fn digest_line_carries_no_elided_content() {
    use gobstopper_core::model::SessionHandle;
    use gobstopper_core::strategy::{PolicyConfig, Strategy, StructuredStrategy};

    let dir = tmpdir("digest-privacy");
    let path = dir.join("session.jsonl");
    let mut lines = vec![
        serde_json::to_string(&json!({
            "type": "session_meta",
            "payload": {"id": "c1", "timestamp": "t", "cwd": "/tmp"}
        }))
        .unwrap(),
        serde_json::to_string(&json!({
            "type": "response_item", "ordinal": 0,
            "payload": {"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "goal text"}]}
        }))
        .unwrap(),
    ];
    for i in 0..30usize {
        lines.push(
            serde_json::to_string(&json!({
                "type": "response_item", "ordinal": i + 1,
                "payload": {"type": "function_call_output", "call_id": format!("c{i}"),
                    "output": format!("PAYLOAD-MARKER-{i}-{}", "z".repeat(400))}
            }))
            .unwrap(),
        );
    }
    lines.push(
        serde_json::to_string(&json!({
            "type": "token_usage_record",
            "payload": {"usage": {"input_tokens": 90000, "output_tokens": 20000},
                "thread_token_usage": {"input_tokens": 90000, "cached_input_tokens": 0}}
        }))
        .unwrap(),
    );
    write_lines(&path, &lines);

    let handle = SessionHandle {
        provider: Provider::Codex,
        session_id: "c1".into(),
        path: path.clone(),
        cwd: None,
        age_secs: 0,
    };
    let t = codex::load(handle).unwrap();
    let policy = PolicyConfig {
        trigger_tokens: 1,
        ..Default::default()
    };
    let plan = StructuredStrategy.evaluate(&t, &policy).expect("plan fires");
    let has_digest = plan
        .edits
        .iter()
        .any(|e| matches!(e, Edit::InjectDigest { .. }));
    assert!(has_digest, "structured plan must inject a digest");
    codex::apply(&path, &plan.edits).unwrap();

    let after = read_lines(&path);
    let digest_line = after.last().expect("digest line appended");
    assert!(
        !digest_line.contains("PAYLOAD-MARKER"),
        "digest leaked elided payload content"
    );
    // Sanity: the covered markers really were elided, so this isn't a
    // vacuous pass — stubs replaced their payload bytes.
    let stubs = after
        .iter()
        .filter(|l| l.contains("output elided"))
        .count();
    assert!(stubs > 0, "expected covered outputs to be stubbed");
}

/// A cyclic parentUuid chain (a1->b1, b1->a1) must not hang load — the
/// cycle guard bounds the walk. Corrupt-but-parseable linkage is a real
/// transcript state after interrupted provider writes.
#[test]
fn cyclic_parent_chain_terminates() {
    let dir = tmpdir("cycle");
    let path = dir.join("session.jsonl");
    write_lines(
        &path,
        &[
            r#"{"type":"user","uuid":"a1","parentUuid":"b1","message":{"role":"user","content":"x"}}"#.to_string(),
            r#"{"type":"assistant","uuid":"b1","parentUuid":"a1","message":{"role":"assistant","content":[{"type":"text","text":"y"}]}}"#.to_string(),
        ],
    );
    let t = claude::load(gobstopper_core::model::SessionHandle {
        provider: Provider::ClaudeCode,
        session_id: "cycle".to_string(),
        path,
        cwd: None,
        age_secs: 0,
    })
    .unwrap();
    assert!(!t.items.is_empty());
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

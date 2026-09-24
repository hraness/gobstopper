use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-report-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for path in [
            "codex/sessions",
            "claude/projects/synthetic",
            "devin",
            "config",
        ] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        fs::write(
            root.join("codex/sessions/codex.jsonl"),
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-synthetic\"}}\n",
                "{\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"input_tokens\":5000,\"output_tokens\":20},\"thread_token_usage\":{\"input_tokens\":5000}}}\n"
            ),
        )
        .unwrap();
        fs::write(
            root.join("claude/projects/synthetic/claude.jsonl"),
            format!(
                "{}\n",
                json!({"type":"assistant","sessionId":"claude-synthetic","uuid":"a1",
                    "message":{"role":"assistant","content":[],
                        "usage":{"input_tokens":200,"output_tokens":3,"cache_read_input_tokens":1000}}})
            ),
        )
        .unwrap();
        let conn = Connection::open(root.join("devin/sessions.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, working_directory TEXT,
             created_at INTEGER, last_activity_at INTEGER, main_chain_id INTEGER);
             CREATE TABLE message_nodes (session_id TEXT, node_id INTEGER, parent_node_id INTEGER,
             chat_message TEXT, created_at INTEGER, metadata TEXT);",
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO sessions VALUES ('devin-synthetic', 'synthetic', '/synthetic', ?1, ?1, 1)",
            [now],
        )
        .unwrap();
        for (node, parent, input, cached, output) in [
            (0, None, 100, 50, 5),
            (1, Some(0), 200, 80, 7),
            (2, Some(0), 900, 100, 1),
        ] {
            let message = json!({"message_id":format!("a{node}"),"role":"assistant",
                "content":"synthetic","metadata":{"metrics":{
                    "input_tokens":input,"cache_read_tokens":cached,"output_tokens":output}}});
            conn.execute(
                "INSERT INTO message_nodes VALUES ('devin-synthetic', ?1, ?2, ?3, ?4, NULL)",
                params![node, parent, message.to_string(), now],
            )
            .unwrap();
        }
        Self(root)
    }

    fn run(&self, args: &[&str]) -> Output {
        let db_before = fs::read(self.0.join("devin/sessions.db")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(name);
            }
        }
        let output = command
            .arg("--codex-home")
            .arg(self.0.join("codex"))
            .arg("--claude-home")
            .arg(self.0.join("claude"))
            .arg("--devin-home")
            .arg(self.0.join("devin"))
            .args(args)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(self.0.join("devin/sessions.db")).unwrap(),
            db_before
        );
        assert!(!self.0.join("data").exists());
        output
    }

    fn report(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args).stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn session<'a>(report: &'a Value, provider: &str) -> &'a Value {
    report["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["provider"] == provider)
        .unwrap()
}

#[test]
fn context_only_preserves_provider_context_and_lifetime_missingness() {
    let fixture = Fixture::new();
    let full = fixture.report(&["report", "--active-only"]);
    let context = fixture.report(&["report", "--active-only", "--context-only"]);
    assert_eq!(full["sessions"].as_array().unwrap().len(), 3);
    assert_eq!(context["sessions"].as_array().unwrap().len(), 3);
    for provider in ["codex", "claude_code", "devin"] {
        let before = session(&full, provider);
        let after = session(&context, provider);
        for field in [
            "contextState",
            "reportedContextTokens",
            "sourceIdentitySha256",
        ] {
            assert_eq!(before["gobstopper"][field], after["gobstopper"][field]);
        }
        assert_eq!(after["gobstopper"]["contextState"], "reported");
        if provider != "devin" {
            let mut before_usage = before["usage"].clone();
            let mut after_usage = after["usage"].clone();
            // Each report stamps its own observation time; the measured
            // counters and their presence must stay unchanged between modes.
            for usage in [&mut before_usage, &mut after_usage] {
                for row in usage.as_array_mut().unwrap() {
                    row.as_object_mut().unwrap().remove("atMs");
                }
            }
            assert_eq!(before_usage, after_usage);
            assert_eq!(
                before["gobstopper"]["lifetimeScope"],
                after["gobstopper"]["lifetimeScope"]
            );
        }
    }
    let full_devin = session(&full, "devin");
    let context_devin = session(&context, "devin");
    assert_eq!(full_devin["gobstopper"]["lifetimeScope"], "full");
    assert_eq!(full_devin["gobstopper"]["lifetimeInputTokens"], 1430);
    assert_eq!(full_devin["usage"].as_array().unwrap().len(), 1);
    assert_eq!(context_devin["gobstopper"]["reportedContextTokens"], 287);
    assert_eq!(context_devin["gobstopper"]["lifetimeScope"], "absent");
    assert!(context_devin["usage"].as_array().unwrap().is_empty());

    let strict = fixture.report(&["report", "--context-only", "--strict"]);
    assert!(strict["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row.get("gobstopper").is_none()));
    assert!(session(&strict, "devin")["usage"]
        .as_array()
        .unwrap()
        .is_empty());
    let help = String::from_utf8(fixture.run(&["report", "--help"]).stdout).unwrap();
    assert!(help.contains("--context-only"));
}

#[test]
fn context_only_does_not_read_unreachable_devin_payloads() {
    let fixture = Fixture::new();
    // The graph remains valid. This dead sibling's non-text payload makes any
    // full-history export fail, while the live-chain context is still readable.
    let conn = Connection::open(fixture.0.join("devin/sessions.db")).unwrap();
    conn.execute(
        "UPDATE message_nodes SET chat_message = X'80' WHERE node_id = 2",
        [],
    )
    .unwrap();
    drop(conn);
    let full = fixture.report(&["report", "--active-only"]);
    assert!(session(&full, "devin")["gobstopper"]["reportedContextTokens"].is_null());
    let context = fixture.report(&["report", "--active-only", "--context-only"]);
    let devin = session(&context, "devin");
    assert_eq!(devin["gobstopper"]["reportedContextTokens"], 287);
    assert_eq!(devin["gobstopper"]["contextState"], "reported");
    assert_eq!(devin["gobstopper"]["lifetimeScope"], "absent");
    assert!(devin["usage"].as_array().unwrap().is_empty());
}

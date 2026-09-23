use gobstopper_adapters::{devin, vault};
use gobstopper_core::Provider;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const SELECTED: &str = "selected-session";
const OTHER: &str = "unrelated-session";

struct Fixture {
    root: PathBuf,
    original: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-mcp-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("devin")).unwrap();
        let db = root.join("devin/sessions.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, working_directory TEXT,
             created_at INTEGER, last_activity_at INTEGER, main_chain_id INTEGER);
             CREATE TABLE message_nodes (session_id TEXT, node_id INTEGER, parent_node_id INTEGER,
             chat_message TEXT, created_at INTEGER, metadata TEXT);",
        )
        .unwrap();
        for session in [SELECTED, OTHER] {
            conn.execute(
                "INSERT INTO sessions VALUES (?1, ?1, '/synthetic', 1, 1, 1)",
                [session],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message_nodes VALUES (?1, 1, NULL, ?2, 1, NULL)",
                params![
                    session,
                    json!({"role":"user", "content":"synthetic"}).to_string()
                ],
            )
            .unwrap();
        }
        drop(conn);
        Self {
            original: fs::read(&db).unwrap(),
            root,
        }
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                cmd.env_remove(key);
            }
        }
        cmd.args(["--codex-home"])
            .arg(self.root.join("codex"))
            .arg("--claude-home")
            .arg(self.root.join("claude"))
            .arg("--devin-home")
            .arg(self.root.join("devin"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"));
        cmd
    }

    fn mcp(&self, name: &str) -> Value {
        let mut child = self
            .command()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(
            child.stdin.take().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0", "id":1, "method":"tools/call",
                "params":{"name":name, "arguments":{"session":SELECTED}}})
        )
        .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{:?}", output);
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(response["error"].is_null(), "{response}");
        assert_ne!(response["result"]["isError"], true, "{response}");
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn assert_unchanged(&self) {
        assert_eq!(
            fs::read(self.root.join("devin/sessions.db")).unwrap(),
            self.original
        );
        assert!(!self.root.join("data/gobstopper/events.jsonl").exists());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn mcp_verifies_the_selected_devin_export_without_writing_state() {
    let fixture = Fixture::new();
    let response = fixture.mcp("verify");
    assert_eq!(response["session_id"], SELECTED);
    assert_eq!(response["errors"], 0);
    assert_eq!(response["findings"], json!([]));
    fixture.assert_unchanged();
    assert!(!fixture.root.join("data").exists());
    assert!(!fixture.root.join("devin/session_locks").exists());
}

#[test]
fn history_and_vault_keep_devin_sessions_in_the_same_database_separate() {
    let fixture = Fixture::new();
    let root = fixture.root.join("data/gobstopper/vault");
    for session in [SELECTED, OTHER] {
        vault::snapshot(
            &devin::db_path(&fixture.root.join("devin")),
            Provider::Devin,
            session,
            Some("fixture"),
            &root,
        )
        .unwrap();
    }
    let history = fixture.mcp("history");
    assert_eq!(history["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(history["snapshots"][0]["session_id"], SELECTED);
    for command in ["history", "vault"] {
        let output = fixture
            .command()
            .args([command, SELECTED, "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let entries: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(entries.as_array().unwrap().len(), 1, "{command}: {entries}");
        assert_eq!(entries[0]["session_id"], SELECTED);
    }
    fixture.assert_unchanged();
}

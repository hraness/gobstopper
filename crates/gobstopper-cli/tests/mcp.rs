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
            .env("HOME", self.root.join("home"))
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

fn rpc(mut command: Command, name: &str, arguments: Value) -> Value {
    let mut child = command
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn content(response: &Value) -> Value {
    assert!(response["error"].is_null(), "{response}");
    assert_ne!(response["result"]["isError"], true, "{response}");
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn show_and_recall_bind_provider_session_and_store_after_prefix_resolution() {
    let fixture = Fixture::new();
    let root = fixture.root.join("data/gobstopper/vault");
    let db = devin::db_path(&fixture.root.join("devin"))
        .canonicalize()
        .unwrap();
    let foreign = fixture.root.join("foreign/sessions.db");
    let mut selected = String::new();
    for (provider, session, path, summary) in [
        (Provider::Devin, SELECTED, &db, "SELECTED_SUMMARY"),
        (
            Provider::Devin,
            "selected-session-archived",
            &db,
            "ARCHIVED_SECRET_SENTINEL",
        ),
        (Provider::Codex, SELECTED, &db, "PROVIDER_SECRET_SENTINEL"),
        (Provider::Devin, SELECTED, &foreign, "STORE_SECRET_SENTINEL"),
        (Provider::Devin, OTHER, &db, "UNRELATED_SECRET_SENTINEL"),
    ] {
        let bytes = format!(
            "{}\n",
            json!({"type":"user","message":{"role":"user","content":format!("[gobstopper state card]\nsummary: {summary}")}})
        );
        let entry = vault::snapshot_data(
            bytes.as_bytes(),
            path,
            provider,
            session,
            Some("fixture"),
            &root,
        )
        .unwrap();
        if summary == "SELECTED_SUMMARY" {
            selected = entry.sha256;
        }
    }
    let shown = content(&rpc(
        fixture.command(),
        "show",
        json!({"target":"selected"}),
    ));
    assert_eq!(shown["sha256"], selected);
    assert_eq!(shown["session_id"], SELECTED);
    let recalled = content(&rpc(
        fixture.command(),
        "recall",
        json!({"session":"selected"}),
    ));
    assert_eq!(recalled["count"], 1);
    assert_eq!(recalled["digests"][0]["summary"], "SELECTED_SUMMARY");
    assert!(!recalled.to_string().contains("SECRET_SENTINEL"));
    fixture.assert_unchanged();
}

#[cfg(unix)]
#[test]
fn inspection_disables_external_strategies_models_and_bridge_writes() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("codex/sessions")).unwrap();
    fs::create_dir_all(fixture.root.join("config/gobstopper")).unwrap();
    fs::create_dir_all(fixture.root.join("bin")).unwrap();
    let marker = fixture.root.join("external-executed");
    let script = "#!/bin/sh\nprintf called >> \"$MCP_MARKER\"\nexit 1\n";
    for name in ["forbidden", "curl", "swiftc"] {
        let path = fixture.root.join("bin").join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = fixture.root.join("codex/sessions/rollout-inspection.jsonl");
    let original = [
        json!({"type":"session_meta","payload":{"id":"inspection-session"}}),
        json!({"type":"response_item","payload":{"type":"function_call","name":"exec","call_id":"c","arguments":"{}"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"fixture-output ".repeat(2000)}}),
        json!({"type":"token_usage_record","payload":{"usage":{"input_tokens":10000},"thread_token_usage":{"input_tokens":10000}}}),
    ].iter().map(|row| format!("{row}\n")).collect::<String>();
    fs::write(&source, &original).unwrap();
    let config = "[policy]\nstrategy='scored'\ntrigger_tokens=1000\nfloor_tokens=100\nkeep_recent_tool_outputs=0\nmin_savings_tokens=0\n";
    let config_path = fixture.root.join("config/gobstopper/config.toml");
    fs::write(&config_path, config).unwrap();
    let command = |scorer: &str| {
        let mut cmd = fixture.command();
        cmd.env("PATH", fixture.root.join("bin"))
            .env("MCP_MARKER", &marker)
            .env("GOBSTOPPER_SCORER", scorer)
            .env("GOBSTOPPER_DIGEST", "apple")
            .env(
                "GOBSTOPPER_APPLE_BRIDGE",
                fixture.root.join("bin/forbidden"),
            )
            .env("AI_GATEWAY_API_KEY", "synthetic-test-key")
            .env("TYPESAFE_API_KEY", "synthetic-test-key");
        cmd
    };
    let mut expected = None;
    for scorer in ["apple", "jev", "llm"] {
        let value = content(&rpc(command(scorer), "plan", json!({"session":source})));
        assert!(
            value["plan"].is_object(),
            "fixture must exercise an admitted plan: {value}"
        );
        if let Some(previous) = &expected {
            assert_eq!(&value, previous);
        } else {
            expected = Some(value);
        }
        assert!(!marker.exists());
    }
    let external = format!(
        "{config}command={}\ntrusted_legacy_command=true\n",
        serde_json::to_string(&fixture.root.join("bin/forbidden")).unwrap()
    );
    fs::write(&config_path, &external).unwrap();
    let response = rpc(command("apple"), "plan", json!({"session":source}));
    assert_eq!(response["result"]["isError"], true);
    assert!(!marker.exists());
    assert_eq!(fs::read_to_string(&config_path).unwrap(), external);
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert!(!fixture.root.join("data").exists());
    assert!(!fixture.root.join("home").exists());
    fixture.assert_unchanged();
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

use rusqlite::{params, Connection};
use serde_json::json;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-custody-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        for path in ["config/gobstopper", "devin", "claude/projects/synthetic"] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        Self(root)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        command
            .env_clear()
            .env("PATH", "")
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .arg("--codex-home")
            .arg(self.0.join("codex"))
            .arg("--claude-home")
            .arg(self.0.join("claude"))
            .arg("--devin-home")
            .arg(self.0.join("devin"));
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
}

#[test]
fn hooks_refuse_settings_mutation_and_export_one_private_candidate() {
    let fixture = Fixture::new();
    let settings = fixture.0.join("claude/settings.json");
    let original = br#"{"private":"settings-sentinel","hooks":{}}"#;
    fs::write(&settings, original).unwrap();
    // A provider/editor retains an open descriptor while both commands run.
    // Their refusal cannot replace the inode, truncate it or overwrite edits.
    let mut writer = fs::OpenOptions::new().append(true).open(&settings).unwrap();
    let before = fs::metadata(&settings).unwrap();
    let mut install = fixture
        .command()
        .arg("install-hooks")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut uninstall = fixture
        .command()
        .arg("uninstall-hooks")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    writer.write_all(b"\n ").unwrap();
    writer.sync_all().unwrap();
    assert!(!install.wait().unwrap().success());
    assert!(!uninstall.wait().unwrap().success());
    let expected = [original.as_slice(), b"\n "].concat();
    assert_eq!(fs::read(&settings).unwrap(), expected);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&settings).unwrap().ino(), before.ino());
    }
    assert_eq!(fs::read_dir(fixture.0.join("claude")).unwrap().count(), 2);

    let bundle = fixture.0.join("candidates.json");
    let launch = || {
        fixture
            .command()
            .args(["install-hooks", "--output"])
            .arg(&bundle)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = launch();
    let second = launch();
    let outputs = [
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    ];
    assert_eq!(outputs.iter().filter(|out| out.status.success()).count(), 1);
    for output in outputs {
        assert!(!String::from_utf8_lossy(&output.stdout).contains("settings-sentinel"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("settings-sentinel"));
    }
    let candidate: serde_json::Value = serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    assert_eq!(
        candidate["candidates"][0]["source_bytes"],
        String::from_utf8(expected.clone()).unwrap()
    );
    assert_eq!(
        candidate["candidates"][0]["candidate"]["private"],
        "settings-sentinel"
    );
    assert_eq!(fs::read(&settings).unwrap(), expected);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(bundle).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert!(!fixture.0.join("data").exists());
}

#[test]
fn hook_stdin_is_bounded_even_when_the_sender_never_closes() {
    let fixture = Fixture::new();
    let started = std::time::Instant::now();
    let child = fixture
        .command()
        .args(["hook", "precompact"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // wait_with_output drops stdin; retain it outside Child to test the actual
    // no-EOF condition, not merely an empty payload.
    let mut child = child;
    let retained_pipe = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    drop(retained_pipe);
    assert!(output.status.success());
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(!fixture.0.join("data").exists());
}

#[test]
fn undo_refuses_contradictory_snapshot_binding_before_recording_or_publishing() {
    use gobstopper_adapters::vault;
    use gobstopper_core::Provider;
    for contradiction in ["session", "source_hash", "byte_count"] {
        let fixture = Fixture::new();
        let sessions = fixture.0.join("codex/sessions");
        fs::create_dir_all(&sessions).unwrap();
        let source = sessions.join("rollout-selected.jsonl");
        let original = format!(
            "{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":"selected"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"selected source"}]}}),
        );
        fs::write(&source, &original).unwrap();
        let snapshot = if contradiction == "session" {
            format!(
                "{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":"other"}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"foreign snapshot sentinel"}]}}),
            )
        } else {
            original.clone()
        };
        let root = fixture.0.join("data/gobstopper/vault");
        let mut entry = vault::snapshot_data(
            snapshot.as_bytes(),
            &source.canonicalize().unwrap(),
            Provider::Codex,
            "selected",
            None,
            &root,
        )
        .unwrap();
        if contradiction == "source_hash" {
            entry.source_sha256 = "a".repeat(64);
        } else if contradiction == "byte_count" {
            entry.bytes += 1;
        }
        let index = format!("{}\n", serde_json::to_string(&entry).unwrap());
        fs::write(root.join("index.jsonl"), &index).unwrap();
        let output = fixture
            .command()
            .arg("undo")
            .arg(&source)
            .args(["--sha", &entry.sha256, "--yes"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {contradiction}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains("foreign snapshot sentinel"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("foreign snapshot sentinel"));
        assert_eq!(fs::read_to_string(&source).unwrap(), original);
        assert_eq!(fs::read_to_string(root.join("index.jsonl")).unwrap(), index);
        assert_eq!(fs::read_dir(sessions).unwrap().count(), 1);
        assert!(!root.join("operations").exists());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn configuration_special_files_refuse_without_blocking_or_disclosing_contents() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let config = fixture.0.join("config/gobstopper/config.toml");
    let fifo = std::ffi::CString::new(config.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let mut child = fixture
        .command()
        .args(["detect", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let completed = child.try_wait().unwrap().is_some();
    if !completed {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(completed, "configuration FIFO blocked CLI startup");
    assert!(!output.status.success());
    fs::remove_file(&config).unwrap();
    let target = fixture.0.join("private-config");
    fs::write(&target, "private-config-sentinel").unwrap();
    std::os::unix::fs::symlink(&target, &config).unwrap();
    let output = fixture.run(&["detect", "--json"]);
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-config-sentinel"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-config-sentinel"));
    assert_eq!(
        fs::read_to_string(target).unwrap(),
        "private-config-sentinel"
    );
}

#[test]
fn devin_apply_and_undo_refuse_before_snapshot_or_confirmation() {
    let fixture = Fixture::new();
    let db = fixture.0.join("devin/sessions.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, working_directory TEXT,
         created_at INTEGER, last_activity_at INTEGER, main_chain_id INTEGER);
         CREATE TABLE message_nodes (session_id TEXT, node_id INTEGER, parent_node_id INTEGER,
         chat_message TEXT, created_at INTEGER, metadata TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO sessions VALUES ('synthetic', 'synthetic', '/synthetic', 1, 1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_nodes VALUES ('synthetic', 1, NULL, ?1, 1, NULL)",
        params![json!({"role":"user","content":"preserve this source"}).to_string()],
    )
    .unwrap();
    drop(conn);
    let original = fs::read(&db).unwrap();
    // Evaluation must export this database session into one frozen, read-only
    // snapshot; feeding SQLite bytes into the JSONL evaluator used to drop it.
    let output = fixture.run(&["eval", "synthetic", "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|row| row["source_sha256"]
        .as_str()
        .is_some_and(|hash| hash.len() == 64)));
    let output = fixture.run(&["bench", "--all"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let csv = String::from_utf8(output.stdout).unwrap();
    assert_eq!(csv.lines().skip(1).count(), rows.len());
    assert!(csv
        .lines()
        .skip(1)
        .all(|line| line.starts_with("devin,synthetic,")));
    assert_eq!(fs::read(&db).unwrap(), original);
    assert_eq!(fs::read_dir(fixture.0.join("devin")).unwrap().count(), 1);
    assert!(!fixture.0.join("data").exists());
    for operation in ["apply", "undo"] {
        let output = fixture.run(&[operation, "synthetic", "--yes"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("lifetime provider custody is unavailable"));
        assert!(output.stdout.is_empty());
        assert_eq!(fs::read(&db).unwrap(), original);
        assert_eq!(fs::read_dir(fixture.0.join("devin")).unwrap().count(), 1);
        assert!(!fixture.0.join("data").exists());
    }
}

#[test]
fn legacy_claude_inplace_setting_cannot_mutate_or_fall_back_to_a_copy() {
    let fixture = Fixture::new();
    let config = fixture.0.join("config/gobstopper/config.toml");
    let settings = "[policy]\nstrategy='elide'\ntrigger_tokens=2\nfloor_tokens=1\nmin_savings_tokens=0\nkeep_recent_tool_outputs=0\n[provider.claude_code]\nauto_apply_inplace=true\n";
    fs::write(&config, settings).unwrap();
    let source = fixture.0.join("claude/projects/synthetic/session.jsonl");
    let records = [
        json!({"type":"assistant","uuid":"a","parentUuid":null,"sessionId":"synthetic", "message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"Read","input":{}}]}}),
        json!({"type":"user","uuid":"b","parentUuid":"a","sessionId":"synthetic", "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"x".repeat(12_000)}]}}),
    ];
    let original = records
        .iter()
        .map(|record| format!("{record}\n"))
        .collect::<String>();
    fs::write(&source, &original).unwrap();
    fs::File::open(&source)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600)),
        )
        .unwrap();
    let output = fixture.run(&["watch", "--provider", "claude", "--once"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("lifetime custody unavailable"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert_eq!(fs::read_to_string(config).unwrap(), settings);
    assert_eq!(fs::read_dir(source.parent().unwrap()).unwrap().count(), 1);
    assert!(!fixture.0.join("data/gobstopper/vault").exists());
    let log = fs::read_to_string(fixture.0.join("data/gobstopper/events.jsonl")).unwrap();
    let events: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["outcome"], "blocked");
    assert_eq!(events[0]["error_code"], "custody_unavailable");
    assert_eq!(events[0]["est_reclaimed_tokens"], 0);
}

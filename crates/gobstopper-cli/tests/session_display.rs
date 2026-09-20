use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    original: Vec<u8>,
    session_id: String,
}

impl Fixture {
    fn new(session_id: String, metadata_id: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-session-display-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(root.join("codex/sessions")).unwrap();
        fs::create_dir_all(root.join("config/gobstopper")).unwrap();
        fs::write(
            root.join("config/gobstopper/config.toml"),
            "[policy]\ntrigger_tokens=1000\nfloor_tokens=100\nmin_savings_tokens=0\n",
        )
        .unwrap();
        let mut records = Vec::new();
        if metadata_id {
            records
                .push(serde_json::json!({"type": "session_meta", "payload": {"id": session_id}}));
        }
        records.push(
            serde_json::json!({"type": "token_usage_record", "payload": {
                "usage": {"input_tokens": 5000, "output_tokens": 0},
                "thread_token_usage": {"input_tokens": 5000}
            }}),
        );
        let original = records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>()
            .into_bytes();
        let filename = if metadata_id {
            "rollout-fixture"
        } else {
            &session_id
        };
        let source = root
            .join("codex/sessions")
            .join(format!("{filename}.jsonl"));
        fs::write(&source, &original).unwrap();
        // Vault entries store canonical paths, so history's path selector must
        // use that same identity even when the platform temp root is a symlink.
        let source = source.canonicalize().unwrap();
        Self {
            root,
            source,
            original,
            session_id,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(key);
            }
        }
        let output = command
            .arg("--codex-home")
            .arg(self.root.join("codex"))
            .arg("--claude-home")
            .arg(self.root.join("claude"))
            .args(args)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&self.source).unwrap(), self.original);
        assert_eq!(
            fs::read_dir(self.root.join("codex/sessions"))
                .unwrap()
                .count(),
            1
        );
        output
    }

    fn snapshot(&self) -> PathBuf {
        let vault = self.root.join("data/gobstopper/vault");
        let entry = gobstopper_adapters::vault::snapshot(
            &self.source,
            gobstopper_core::Provider::Codex,
            &self.session_id,
            None,
            &vault,
        )
        .unwrap();
        assert_eq!(entry.session_id, self.session_id);
        vault.join("index.jsonl")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn detect_at_boundary(metadata_id: bool) {
    let fixture = Fixture::new(format!("{}😀tail", "a".repeat(37)), metadata_id);
    let output = fixture.run(&["detect"]);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text.lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap(),
        "a".repeat(37)
    );
    assert!(!fixture.root.join("data").exists());
}

#[test]
fn detect_handles_metadata_id_crossing_its_byte_limit() {
    detect_at_boundary(true);
}

#[test]
fn detect_handles_filename_id_crossing_its_byte_limit() {
    detect_at_boundary(false);
}

fn watch_at_boundary(metadata_id: bool) {
    let fixture = Fixture::new(format!("{}😀tail", "a".repeat(11)), metadata_id);
    let output = fixture.run(&["watch", "--dry-run", "--once"]);
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(
        text.contains(&format!("[dry-run] codex {}:", "a".repeat(11))),
        "{text}"
    );
    assert!(!fixture.root.join("data").exists());
}

#[test]
fn watch_handles_metadata_id_crossing_its_byte_limit() {
    watch_at_boundary(true);
}

#[test]
fn watch_handles_filename_id_crossing_its_byte_limit() {
    watch_at_boundary(false);
}

fn archived_at_boundary(history: bool) {
    let fixture = Fixture::new(format!("{}😀tail", "a".repeat(11)), true);
    let index = fixture.snapshot();
    let original_index = fs::read(&index).unwrap();
    let output = if history {
        fixture.run(&["history", fixture.source.to_str().unwrap()])
    } else {
        fixture.run(&["vault"])
    };
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text.lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .nth(4)
            .unwrap(),
        "a".repeat(11)
    );
    assert_eq!(fs::read(index).unwrap(), original_index);
    assert!(!fixture.root.join("data/gobstopper/events.jsonl").exists());
}

#[test]
fn vault_handles_id_crossing_its_byte_limit() {
    archived_at_boundary(false);
}

#[test]
fn history_handles_id_crossing_its_byte_limit() {
    archived_at_boundary(true);
}

#[test]
fn json_and_archived_ids_remain_complete() {
    let session_id = format!("{}😀{}😀tail", "a".repeat(11), "b".repeat(22));
    for metadata_id in [true, false] {
        let fixture = Fixture::new(session_id.clone(), metadata_id);
        let detected: serde_json::Value =
            serde_json::from_slice(&fixture.run(&["detect", "--json"]).stdout).unwrap();
        assert_eq!(detected[0]["session_id"], session_id);
        let index = fixture.snapshot();
        let original_index = fs::read(&index).unwrap();
        for args in [
            vec!["vault", "--json"],
            vec!["history", fixture.source.to_str().unwrap(), "--json"],
        ] {
            let entries: serde_json::Value =
                serde_json::from_slice(&fixture.run(&args).stdout).unwrap();
            assert_eq!(entries[0]["session_id"], session_id);
        }
        assert_eq!(fs::read(index).unwrap(), original_index);
        assert!(!fixture.root.join("data/gobstopper/events.jsonl").exists());
    }
}

#[test]
fn ascii_display_widths_remain_unchanged() {
    let fixture = Fixture::new("a".repeat(42), true);
    let detect = String::from_utf8(fixture.run(&["detect"]).stdout).unwrap();
    assert!(detect.lines().nth(1).unwrap().starts_with(&format!(
        "{:<12} {:<38} {:<6}",
        "codex",
        "a".repeat(38),
        "live"
    )));
    let watch = String::from_utf8(fixture.run(&["watch", "--dry-run", "--once"]).stderr).unwrap();
    assert!(watch.contains(&format!("[dry-run] codex {}:", "a".repeat(12))));
    fixture.snapshot();
    for args in [
        vec!["vault"],
        vec!["history", fixture.source.to_str().unwrap()],
    ] {
        let listing = String::from_utf8(fixture.run(&args).stdout).unwrap();
        let row = listing.lines().nth(1).unwrap();
        assert_eq!(row.split_whitespace().nth(4).unwrap(), "a".repeat(12));
        assert!(row.ends_with(&format!(
            "{} {}",
            "a".repeat(12),
            fixture.source.canonicalize().unwrap().display()
        )));
    }
}

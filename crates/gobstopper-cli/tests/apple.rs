//! `gobstopper apple …` and the Apple scorer's fallback notice, run through
//! the real binary against a fake helper. Nothing here builds Swift, starts
//! Apple's model, or can open a macOS dialog.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-apple-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        Self { root }
    }

    /// A helper whose `--check` prints `check` and exits 0.
    fn helper(&self, check: &str) -> PathBuf {
        let path = self.root.join("apple-bridge");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --check ]; then printf '%s\\n' '{check}'; exit 0; fi\nexit 3\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
    }

    /// The binary with a clean, human-free environment: temp HOME, no
    /// gobstopper settings, no agent markers, UTF-8, `NO_COLOR`.
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy().into_owned();
            if key.starts_with("GOBSTOPPER_")
                || key.starts_with("HRANESS_")
                || matches!(
                    key.as_str(),
                    "AI_AGENT"
                        | "CLAUDECODE"
                        | "CODEX_SANDBOX"
                        | "CODEX_SANDBOX_NETWORK_DISABLED"
                        | "CURSOR_AGENT"
                        | "GEMINI_CLI"
                        | "FORCE_COLOR"
                        | "LC_ALL"
                        | "LC_CTYPE"
                )
            {
                cmd.env_remove(key);
            }
        }
        cmd.env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("LANG", "en_US.UTF-8")
            .env("TERM", "xterm-256color")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null());
        cmd
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().unwrap()
}

#[test]
fn apple_help_exits_zero_for_every_command() {
    let fixture = Fixture::new();
    for args in [
        &["apple", "--help"][..],
        &["apple", "status", "--help"],
        &["apple", "install", "--help"],
        &["help", "apple"],
    ] {
        let out = run(fixture.command().args(args));
        assert!(out.status.success(), "{args:?}: {out:?}");
        let stdout = text(&out.stdout);
        assert!(stdout.starts_with("Usage: gobstopper apple") || stdout.contains("apple"));
        assert!(!stdout.contains('\x1b'), "no color when piped");
    }
    let help = text(&run(fixture.command().args(["apple", "install", "--help"])).stdout);
    assert!(
        help.contains("never opens the macOS install dialog"),
        "{help}"
    );
}

#[test]
fn status_reports_the_reason_and_one_next_step() {
    let fixture = Fixture::new();
    let helper = fixture.helper(r#"{"available":false,"reason":"appleIntelligenceNotEnabled"}"#);
    // A person at a terminal gets the `→` step; HRANESS_AUDIENCE stands in for
    // the terminal, since stderr here is a pipe.
    let out = run(fixture
        .command()
        .args(["apple", "status"])
        .env("GOBSTOPPER_APPLE_BRIDGE", &helper)
        .env("HRANESS_AUDIENCE", "human"));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let helper_line = format!("  Helper: {}\n", helper.display());
    assert_eq!(
        text(&out.stdout),
        format!(
            "⚠ Apple Intelligence is off.\n  Turn it on in System Settings › Apple Intelligence & Siri, then try again.\n{helper_line}→ open x-apple.systempreferences:com.apple.Siri-Settings.extension\n"
        )
    );
    assert_eq!(text(&out.stderr), "");

    // Not a terminal and no audience set: quiet, so no next-step line.
    let out = run(fixture
        .command()
        .args(["apple", "status"])
        .env("GOBSTOPPER_APPLE_BRIDGE", &helper));
    assert_eq!(
        text(&out.stdout),
        format!(
            "⚠ Apple Intelligence is off.\n  Turn it on in System Settings › Apple Intelligence & Siri, then try again.\n{helper_line}"
        )
    );

    // ASCII fallback.
    let out = run(fixture
        .command()
        .args(["apple", "status"])
        .env("GOBSTOPPER_APPLE_BRIDGE", &helper)
        .env("TERM", "dumb"));
    assert!(text(&out.stdout).starts_with("WARN Apple Intelligence is off.\n"));
}

#[test]
fn status_json_for_flag_and_for_agents() {
    let fixture = Fixture::new();
    let helper = fixture.helper(r#"{"available":true,"provider":"apple"}"#);
    for extra in [&["--json"][..], &[]] {
        let mut cmd = fixture.command();
        cmd.args(["apple", "status"])
            .args(extra)
            .env("GOBSTOPPER_APPLE_BRIDGE", &helper);
        if extra.is_empty() {
            cmd.env("CLAUDECODE", "1");
        }
        let out = run(&mut cmd);
        assert!(out.status.success(), "{out:?}");
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["available"], true);
        assert_eq!(value["reason"], serde_json::Value::Null);
        assert_eq!(
            value["next"],
            "GOBSTOPPER_SCORER=apple gobstopper plan <session>"
        );
        assert_eq!(text(&out.stderr), "");
    }
}

#[test]
fn status_never_echoes_unknown_bridge_text() {
    let fixture = Fixture::new();
    let helper = fixture.helper(r#"{"available":false,"reason":"PRIVATE_SENTINEL"}"#);
    let out = run(fixture
        .command()
        .args(["apple", "status", "--json"])
        .env("GOBSTOPPER_APPLE_BRIDGE", &helper));
    let stdout = text(&out.stdout);
    assert!(!stdout.contains("PRIVATE_SENTINEL"), "{stdout}");
    let value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(value["reason"], "unavailable");
}

#[test]
fn status_output_survives_a_closed_pipe() {
    let fixture = Fixture::new();
    let helper = fixture.helper(r#"{"available":false,"reason":"modelNotReady"}"#);
    let mut child = fixture
        .command()
        .args(["apple", "status"])
        .env("GOBSTOPPER_APPLE_BRIDGE", &helper)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = text(&out.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_ne!(out.status.code(), Some(101));
}

fn scored_session(root: &Path) -> PathBuf {
    fs::create_dir_all(root.join("codex/sessions")).unwrap();
    fs::create_dir_all(root.join("config/gobstopper")).unwrap();
    fs::write(
        root.join("config/gobstopper/config.toml"),
        "[policy]\nstrategy='scored'\ntrigger_tokens=1000\nfloor_tokens=100\nkeep_recent_tool_outputs=0\nmin_savings_tokens=0\n",
    )
    .unwrap();
    let rows = [
        serde_json::json!({"type":"session_meta","payload":{"id":"22222222-2222-4222-8222-222222222222"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"function_call","name":"exec","call_id":"c","arguments":"{}"}}),
        serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"c","output":"fixture-output ".repeat(2000)}}),
        serde_json::json!({"type":"token_usage_record","payload":{"usage":{"input_tokens":10000},"thread_token_usage":{"input_tokens":10000}}}),
    ];
    let source = root.join("codex/sessions/rollout-apple.jsonl");
    fs::write(
        &source,
        rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
    source
}

#[test]
fn apple_scorer_says_why_it_fell_back() {
    let fixture = Fixture::new();
    let source = scored_session(&fixture.root);
    let helper = fixture.helper(r#"{"available":false,"reason":"modelNotReady"}"#);
    let out = run(fixture
        .command()
        .arg("--codex-home")
        .arg(fixture.root.join("codex"))
        .arg("--claude-home")
        .arg(fixture.root.join("claude"))
        .args(["plan", "--json"])
        .arg(&source)
        .env("GOBSTOPPER_SCORER", "apple")
        .env("GOBSTOPPER_APPLE_BRIDGE", &helper));
    assert!(out.status.success(), "{out:?}");
    let stderr = text(&out.stderr);
    let expected = if cfg!(target_os = "macos") {
        "⚠ Apple's on-device model is still downloading, so gobstopper is using the built-in scorer.\n  Try again in a few minutes. System Settings › Apple Intelligence & Siri shows the progress.\n"
    } else {
        "⚠ Apple's on-device model only runs on macOS, so gobstopper is using the built-in scorer.\n  Unset GOBSTOPPER_SCORER to stop seeing this.\n"
    };
    assert!(stderr.contains(expected), "{stderr}");
    assert_eq!(stderr.matches('⚠').count(), 1, "once per process: {stderr}");
    assert!(!stderr.contains("scorer_unavailable"), "{stderr}");
}

//! Golden output for the CLI style contract: bare invocation, grouped help,
//! version, usage errors (text, ASCII, agent JSON), empty states, closed
//! pipes, `proxy status` when nothing answers, and `proxy install` against a
//! fake `launchctl`.
//!
//! Every run clears the environment and uses a private temporary HOME, so no
//! real session, keychain, clipboard or LaunchAgent is touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("gobstopper-cli-ux-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["home", "codex", "claude", "config", "bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self(root)
    }
    fn home(&self) -> PathBuf {
        self.0.join("home")
    }
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        command
            .env_clear()
            .env("HOME", self.home())
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("PATH", self.0.join("bin"))
            .env("LANG", "en_US.UTF-8")
            .env("CODEX_HOME", self.0.join("codex"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("claude"))
            .args(args)
            .stdin(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.command(args, env).output().unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn bare_invocation_is_a_short_start_here_and_exits_zero() {
    let sandbox = Sandbox::new("bare");
    let output = sandbox.run(&[], &[]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = text(&output.stdout);
    assert!(stdout.starts_with("Gobstopper makes long coding sessions smaller.\n\nStart here\n"));
    assert!(stdout.contains("  gobstopper detect "));
    assert!(stdout.ends_with(&format!("gobstopper {}\n", env!("CARGO_PKG_VERSION"))));
    assert!(stdout.lines().count() <= 25);
    assert!(output.stderr.is_empty());
}

#[test]
fn help_is_grouped_and_every_command_help_exits_zero() {
    let sandbox = Sandbox::new("help");
    for args in [&["--help"][..], &["-h"], &["help"]] {
        let output = sandbox.run(args, &[]);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        let stdout = text(&output.stdout);
        assert!(stdout.contains("\nStart here\n  detect "), "{stdout}");
        assert!(stdout.contains("gobstopper help advanced"));
        assert!(!stdout.contains("native-reconcile"));
        assert!(stdout.lines().count() <= 60);
    }
    let advanced = sandbox.run(&["help", "advanced"], &[]);
    assert_eq!(advanced.status.code(), Some(0));
    assert!(text(&advanced.stdout).contains("  native-reconcile "));
    for args in [
        &["detect", "--help"][..],
        &["help", "plan"],
        &["plan", "-h"],
        &["proxy", "--help"],
        &["proxy", "install", "--help"],
        &["auth", "--help"],
        &["plugin", "--help"],
        &["plugin", "check", "--help"],
    ] {
        let output = sandbox.run(args, &[]);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        assert!(!output.stdout.is_empty(), "{args:?}");
    }
    // A group with no subcommand answers with its help, not an error.
    let proxy = sandbox.run(&["proxy"], &[]);
    assert_eq!(proxy.status.code(), Some(0));
    assert!(text(&proxy.stdout).contains("install"));
    // Global options sit under their own heading in command help.
    let detect = text(&sandbox.run(&["detect", "--help"], &[]).stdout);
    assert!(detect.contains("Global options:"), "{detect}");
    let plugin = text(&sandbox.run(&["plugin", "--help"], &[]).stdout);
    assert!(plugin.contains("Validate a plugin manifest"), "{plugin}");
}

#[test]
fn version_prints_name_and_version() {
    let sandbox = Sandbox::new("version");
    let output = sandbox.run(&["--version"], &[]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        text(&output.stdout),
        format!("gobstopper {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn usage_errors_name_the_input_and_the_help_to_read() {
    let sandbox = Sandbox::new("usage");
    let output = sandbox.run(&["detcet"], &[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        text(&output.stderr),
        "✗ Unknown command \"detcet\". Did you mean \"detect\"?\n→ gobstopper --help\n"
    );
    let dumb = sandbox.run(&["detcet"], &[("TERM", "dumb")]);
    assert_eq!(
        text(&dumb.stderr),
        "FAIL Unknown command \"detcet\". Did you mean \"detect\"?\n-> gobstopper --help\n"
    );
    let no_color = sandbox.run(&["detect", "--limt", "3"], &[("NO_COLOR", "1")]);
    assert_eq!(no_color.status.code(), Some(2));
    let stderr = text(&no_color.stderr);
    assert!(!stderr.contains('\x1b'));
    assert_eq!(
        stderr,
        "✗ Unknown option \"--limt\". Did you mean \"--limit\"?\n→ gobstopper detect --help\n"
    );
    let missing = sandbox.run(&["plan"], &[]);
    assert_eq!(missing.status.code(), Some(2));
    assert_eq!(
        text(&missing.stderr),
        "✗ Missing <SESSION>.\n→ gobstopper plan --help\n"
    );
    let agent = sandbox.run(&["detcet"], &[("CLAUDECODE", "1")]);
    assert_eq!(agent.status.code(), Some(2));
    assert!(agent.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&agent.stderr).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "usage");
    assert_eq!(value["error"]["next"], "gobstopper --help");
}

#[test]
fn empty_states_say_so_and_point_at_one_next_step() {
    let sandbox = Sandbox::new("empty");
    let detect = sandbox.run(&["detect"], &[]);
    assert_eq!(detect.status.code(), Some(0));
    assert_eq!(
        text(&detect.stdout),
        "No Claude Code or Codex sessions found in the last 7 days.\n"
    );
    // A pipe is a quiet reader: no hint.
    assert!(detect.stderr.is_empty());
    let human = sandbox.run(&["detect"], &[("HRANESS_AUDIENCE", "human")]);
    assert_eq!(text(&human.stderr), "Next: gobstopper detect --all\n");
    let presets = sandbox.run(&["presets"], &[("HRANESS_AUDIENCE", "human")]);
    assert_eq!(presets.status.code(), Some(0));
    assert_eq!(
        text(&presets.stdout),
        format!(
            "No presets in {}.\n",
            sandbox.0.join("config/gobstopper/config.toml").display()
        )
    );
    assert!(text(&presets.stderr).starts_with("Next: copy a [presets.<name>] table"));
}

#[test]
fn a_closed_pipe_ends_quietly() {
    let sandbox = Sandbox::new("pipe");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = sandbox
        .command(&["help", "advanced"], &[])
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("Broken pipe"), "{stderr}");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = sandbox
        .command(&["--help"], &[])
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!text(&output.stderr).contains("panicked"));
}

#[test]
fn proxy_status_says_how_to_start_the_proxy() {
    let sandbox = Sandbox::new("status");
    let port = free_port().to_string();
    let output = sandbox.run(&["proxy", "status", "--port", &port], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        format!("✗ No proxy is running on 127.0.0.1:{port}.\n→ gobstopper proxy install\n")
    );
    let json = sandbox.run(&["proxy", "status", "--port", &port, "--json"], &[]);
    assert!(json.stdout.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&json.stderr).unwrap();
    assert_eq!(value["error"]["code"], "proxy-not-running");
    // Installed but not answering: point at the log and a restart.
    let agents = sandbox.home().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("sh.gobstopper.proxy.plist"), "<plist/>").unwrap();
    let installed = sandbox.run(&["proxy", "status", "--port", &port], &[]);
    let stderr = text(&installed.stderr);
    assert!(
        stderr.starts_with(&format!(
            "✗ The proxy is installed but isn't answering on 127.0.0.1:{port}. Its log is {}.\n",
            sandbox
                .home()
                .join("Library/Logs/gobstopper-proxy.log")
                .display()
        )),
        "{stderr}"
    );
    assert!(stderr.contains("→ launchctl kickstart -k gui/"), "{stderr}");
    assert!(stderr.ends_with("/sh.gobstopper.proxy\n"), "{stderr}");
}

fn fake_launchctl(sandbox: &Sandbox) -> (PathBuf, PathBuf) {
    let log = sandbox.0.join("launchctl.log");
    let script = sandbox.0.join("bin/fake-launchctl");
    std::fs::write(
        &script,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n", log.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (script, log)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn proxy_install_says_what_macos_will_show_then_loads_the_agent() {
    let sandbox = Sandbox::new("install");
    let port = free_port().to_string();
    let printed = sandbox.run(
        &[
            "proxy",
            "install",
            "--port",
            &port,
            "--threshold",
            "256000",
            "--print",
        ],
        &[],
    );
    assert_eq!(printed.status.code(), Some(0), "{printed:?}");
    let plist = text(&printed.stdout);
    assert!(plist.contains("<string>serve</string>\n    <string>--port</string>"));
    assert!(plist.contains("<string>--threshold</string>\n    <string>256000</string>"));
    assert!(!plist.contains("--print"));
    let plist_path = sandbox
        .home()
        .join("Library/LaunchAgents/sh.gobstopper.proxy.plist");
    assert!(!plist_path.exists(), "--print changes nothing");

    let (launchctl, calls) = fake_launchctl(&sandbox);
    let launchctl = launchctl.to_str().unwrap();
    let env = [
        ("GOBSTOPPER_TEST_LAUNCHCTL", launchctl),
        ("HRANESS_AUDIENCE", "human"),
    ];
    let installed = sandbox.run(&["proxy", "install", "--port", &port], &env);
    assert_eq!(installed.status.code(), Some(0), "{installed:?}");
    assert_eq!(
        text(&installed.stderr),
        format!(
            "🔐 macOS will show a notice that gobstopper can open at login.\n   It keeps the request proxy on 127.0.0.1:{port} running so Claude Code and Codex requests stay small. Turn it off any time in System Settings › General › Login Items & Extensions.\nNext: export ANTHROPIC_BASE_URL=http://127.0.0.1:{port} in your shell profile, then start Claude Code\n"
        )
    );
    assert!(
        text(&installed.stdout).starts_with("✓ The proxy starts at login. It isn't answering yet")
    );
    assert!(read(&plist_path).contains("<string>--port</string>"));
    assert!(
        read(&calls).starts_with("bootstrap gui/"),
        "{}",
        read(&calls)
    );

    let again = sandbox.run(&["proxy", "install", "--port", &port], &env);
    assert_eq!(again.status.code(), Some(1));
    assert!(text(&again.stderr).ends_with("→ gobstopper proxy status\n"));
    let different = sandbox.run(&["proxy", "install", "--port", "9"], &env);
    assert!(text(&different.stderr).ends_with("→ gobstopper proxy install --replace\n"));

    let quiet = sandbox.run(
        &["proxy", "install", "--port", &port, "--replace"],
        &[("GOBSTOPPER_TEST_LAUNCHCTL", launchctl)],
    );
    assert_eq!(quiet.status.code(), Some(0));
    assert!(quiet.stderr.is_empty(), "a quiet reader gets no notice");

    // A proxy already on the port (started by hand) blocks a second one.
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy_port = busy.local_addr().unwrap().port().to_string();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in busy.incoming().flatten() {
            let mut stream = stream;
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{}");
        }
    });
    let blocked = sandbox.run(
        &["proxy", "install", "--port", &busy_port, "--replace"],
        &env,
    );
    assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
    assert!(text(&blocked.stderr).starts_with(&format!(
        "✗ A gobstopper proxy is already running on 127.0.0.1:{busy_port}."
    )));

    let removed = sandbox.run(&["proxy", "uninstall"], &env);
    assert_eq!(removed.status.code(), Some(0));
    assert_eq!(
        text(&removed.stdout),
        "✓ Removed the proxy LaunchAgent. The proxy no longer starts at login.\n"
    );
    assert!(!plist_path.exists());
    assert!(read(&calls).contains("bootout gui/"));
}

#[test]
fn auth_names_the_one_supported_provider() {
    let sandbox = Sandbox::new("auth");
    let output = sandbox.run(&["auth", "openai"], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "✗ Gobstopper stores keys for jev only, not \"openai\".\n→ gobstopper auth jev\n"
    );
}

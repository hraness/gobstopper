//! Golden output for the CLI style contract: bare invocation, grouped help,
//! version, usage errors (text, ASCII, agent JSON), empty states, closed
//! pipes, `proxy status` when nothing answers, and non-mutating service setup
//! previews and ownership checks.
//!
//! Every run clears the environment and uses a private temporary HOME, so no
//! real session, keychain, clipboard or LaunchAgent is touched.

use std::path::PathBuf;
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
            .env("USERPROFILE", self.home())
            .env("LOCALAPPDATA", self.0.join("local-app-data"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("PATH", self.0.join("bin"))
            .env("LANG", "en_US.UTF-8")
            .env("TERM", "xterm-256color")
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
fn help_fits_100_columns_in_a_wide_terminal() {
    let sandbox = Sandbox::new("width");
    let commands: &[&[&str]] = &[
        &[],
        &["detect"],
        &["plan"],
        &["apply"],
        &["verify"],
        &["report"],
        &["events"],
        &["recall"],
        &["mcp"],
        &["watch"],
        &["export"],
        &["policy-check"],
        &["proxy", "serve"],
        &["proxy", "run"],
        &["proxy", "replay"],
        &["proxy", "install"],
        &["plugin", "inspect"],
        &["apple", "install"],
    ];
    for command in commands {
        let args: Vec<&str> = command.iter().copied().chain(["--help"]).collect();
        let output = sandbox.run(&args, &[("COLUMNS", "200")]);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        let stdout = text(&output.stdout);
        let wide: Vec<&str> = stdout
            .lines()
            .filter(|line| line.chars().count() > 100)
            .collect();
        assert!(wide.is_empty(), "{args:?}: {wide:#?}");
    }
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
        "✗ Missing <session>.\n→ gobstopper plan --help\n"
    );
    // `status` lives under `proxy`; a misspelling finds it there.
    let status = sandbox.run(&["stauts"], &[]);
    assert_eq!(status.status.code(), Some(2));
    assert_eq!(
        text(&status.stderr),
        "✗ Unknown command \"stauts\". Did you mean \"proxy status\"?\n→ gobstopper --help\n"
    );
    // An agent, or --json, gets one JSON document on stdout and nothing on
    // stderr, with the same exit code.
    for (args, env) in [
        (&["detcet"][..], &[("CLAUDECODE", "1")][..]),
        (&["detcet", "--json"], &[]),
    ] {
        let agent = sandbox.run(args, env);
        assert_eq!(agent.status.code(), Some(2), "{args:?}");
        assert!(agent.stderr.is_empty(), "{args:?}");
        let value: serde_json::Value = serde_json::from_slice(&agent.stdout).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"]["code"], "usage");
        assert_eq!(
            value["error"]["message"],
            "Unknown command \"detcet\". Did you mean \"detect\"?"
        );
        assert_eq!(value["error"]["next"], "gobstopper --help");
    }
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
    assert_eq!(json.status.code(), Some(1));
    assert!(json.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "proxy-not-running");
    assert_eq!(value["error"]["next"], "gobstopper proxy install");
    let agent = sandbox.run(&["proxy", "status", "--port", &port], &[("AI_AGENT", "1")]);
    assert!(agent.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&agent.stdout).unwrap();
    assert_eq!(value["error"]["code"], "proxy-not-running");
    // A bare plist is not proof of an installation we own. Preserve it and
    // report its migration path through doctor instead of offering kill -k.
    let agents = sandbox.home().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("sh.gobstopper.proxy.plist"), "<plist/>").unwrap();
    let installed = sandbox.run(&["proxy", "status", "--port", &port], &[]);
    assert!(text(&installed.stderr).contains("gobstopper proxy install"));
}

#[test]
fn proxy_install_preview_has_identity_and_supervision_without_writes() {
    let sandbox = Sandbox::new("install-preview");
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
    let definition = text(&printed.stdout);
    assert!(definition.contains("--service-id"));
    assert!(definition.contains("--threshold"));
    assert!(definition.contains("256000"));
    assert!(!definition.contains("--print"));
    #[cfg(target_os = "macos")]
    assert!(definition.contains("<key>ThrottleInterval</key><integer>30</integer>"));
    #[cfg(target_os = "linux")]
    assert!(definition.contains("StartLimitBurst=5"));
    assert!(
        !sandbox.0.join("config/gobstopper/service").exists(),
        "--print changes nothing"
    );
    assert!(!sandbox
        .home()
        .join("Library/LaunchAgents/sh.gobstopper.proxy.plist")
        .exists());
}

#[test]
fn proxy_setup_preserves_unmanaged_files_even_with_replace() {
    let sandbox = Sandbox::new("install-ownership");
    #[cfg(target_os = "macos")]
    let path = sandbox
        .home()
        .join("Library/LaunchAgents/sh.gobstopper.proxy.plist");
    #[cfg(target_os = "linux")]
    let path = sandbox
        .0
        .join("config/systemd/user/gobstopper-proxy.service");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "owner-controlled existing definition").unwrap();
        let blocked = sandbox.run(&["proxy", "install", "--replace"], &[]);
        assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
        assert!(text(&blocked.stderr).contains("unmanaged service definition"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "owner-controlled existing definition"
        );
        let removed = sandbox.run(&["proxy", "uninstall"], &[]);
        assert_eq!(removed.status.code(), Some(0));
        assert!(
            path.exists(),
            "uninstall cannot delete an unmanaged definition"
        );
    }
}

#[test]
fn proxy_install_refuses_an_occupied_port_without_killing_its_owner() {
    let sandbox = Sandbox::new("install-port");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let blocked = sandbox.run(&["proxy", "install", "--port", &port], &[]);
    assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
    assert!(text(&blocked.stderr).contains("occupied"));
    assert!(listener.local_addr().is_ok());
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

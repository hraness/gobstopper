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
        // Winsock loads its provider using %SystemRoot%, even for loopback.
        #[cfg(windows)]
        command.env(
            "SystemRoot",
            std::env::var_os("SystemRoot").expect("Windows requires SystemRoot for socket tests"),
        );
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
#[cfg(unix)]
fn proxy_upgrade_refuses_dependent_callers_before_staging_unless_preview_or_override() {
    let sandbox = Sandbox::new("upgrade-dependent-caller");
    for args in [vec!["proxy", "upgrade"], vec!["proxy", "upgrade", "--wait"]] {
        let output = sandbox.run(&args, &[("GOBSTOPPER_SCOPE", "isolated-test-scope")]);
        assert!(!output.status.success());
        assert!(
            text(&output.stderr).contains("depends on the proxy"),
            "{}",
            text(&output.stderr)
        );
    }
    for args in [
        vec!["proxy", "upgrade", "--print"],
        vec!["proxy", "upgrade", "--allow-dependent-caller"],
    ] {
        let output = sandbox.run(&args, &[("GOBSTOPPER_SCOPE", "isolated-test-scope")]);
        assert!(!output.status.success());
        assert!(!text(&output.stderr).contains("depends on the proxy"));
        assert!(!text(&output.stderr).contains("unexpected argument"));
    }
    assert!(!sandbox.0.join("data").exists());
    assert_eq!(
        std::fs::read_dir(sandbox.0.join("config")).unwrap().count(),
        0
    );
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
fn proxy_status_uses_the_managed_port_and_explicit_port_overrides_damaged_manifest() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    let sandbox = Sandbox::new("status-managed-port");
    let service = if cfg!(windows) {
        sandbox.0.join("local-app-data/Gobstopper/service")
    } else {
        sandbox.0.join("config/gobstopper/service")
    };
    std::fs::create_dir_all(&service).unwrap();
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    for explicit in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let manifest = if explicit {
            b"damaged manifest".to_vec()
        } else {
            serde_json::to_vec(&serde_json::json!({
                "schema":1,"platform":platform,"service_id":"aabbcc",
                "executable":"/unused/gobstopper","serve_args":["--port",port.to_string()],
                "port":port,"definition_sha256":"","rendered_definition":""
            }))
            .unwrap()
        };
        std::fs::write(service.join("manifest.json"), manifest).unwrap();
        let server = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "status did not use the expected port"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 1024];
            let count = socket.read(&mut request).unwrap();
            assert!(request[..count].starts_with(b"GET /gobstopper/status "));
            let body = serde_json::json!({"name":"gobstopper-proxy","port":port}).to_string();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let port_arg = port.to_string();
        let args = if explicit {
            vec!["proxy", "status", "--port", &port_arg, "--json"]
        } else {
            vec!["proxy", "status", "--json"]
        };
        let result = sandbox.run(&args, &[]);
        server
            .join()
            .unwrap_or_else(|_| panic!("status fixture failed; CLI output: {result:?}"));
        assert!(result.status.success(), "{}", text(&result.stdout));
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["port"], port);
    }
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

#[cfg(unix)]
#[test]
fn clef_decide_forwards_only_explicit_evidence_to_fixed_model_endpoint() {
    use std::os::unix::fs::PermissionsExt;
    for model in ["clef", "clef-flash"] {
        let sandbox = Sandbox::new(&format!("clef-decide-{model}"));
        let path = sandbox.0.join("evidence.json");
        let recorded = sandbox.0.join("request.json");
        let url = sandbox.0.join("url");
        use base64::Engine;
        let mut image = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(1, 1)
            .write_to(&mut image, image::ImageFormat::Png)
            .unwrap();
        let evidence = serde_json::json!({"model":model,"state":{"report":"PRIVATE_EXPLICIT_STATE"},"questions":{"keep":{"type":"noul","instructions":"Keep this?"}},"images":[{"content_type":"image/png","base64":base64::engine::general_purpose::STANDARD.encode(image.into_inner())}]});
        let bytes = serde_json::to_vec(&evidence).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let reply = serde_json::json!({"success":true,"errors":[],"result":{"model":model,"answers":{"keep":{"type":"noul","noul":0.7}},"usage":{"input_tokens":1,"output_tokens":0}}});
        let script = format!("#!/bin/sh\nset -eu\ntest -z \"${{CLOUDFLARE_API_TOKEN-}}\"\ntest \"$GOBSTOPPER_CURL_BEARER\" = synthetic-clef-token\nlast=''\nfor arg do last=\"$arg\"; done\nprintf '%s' \"$last\" >'{}'\n/bin/cat >'{}'\nprintf '%s\\n%s' '{}' 200\n", url.display(), recorded.display(), reply);
        let curl = sandbox.0.join("bin/curl");
        std::fs::write(&curl, script).unwrap();
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = sandbox.run(
            &["decide", path.to_str().unwrap()],
            &[
                ("CLOUDFLARE_ACCOUNT_ID", "0123456789abcdef0123456789abcdef"),
                ("CLOUDFLARE_API_TOKEN", "synthetic-clef-token"),
                ("GOBSTOPPER_JEV_ENDPOINT", "https://untrusted.invalid"),
            ],
        );
        assert!(output.status.success(), "{}", text(&output.stderr));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            reply["result"]
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(recorded).unwrap()).unwrap(),
            evidence
        );
        assert_eq!(std::fs::read_to_string(url).unwrap(), format!("https://api.cloudflare.com/client/v4/accounts/0123456789abcdef0123456789abcdef/ai/run/@cf/cloudflare/{model}"));
    }
}

#[cfg(unix)]
#[test]
fn clef_decide_transports_explicit_evidence_above_the_plugin_input_cap() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("clef-large-evidence");
    let path = sandbox.0.join("evidence.json");
    let recorded = sandbox.0.join("request.json");
    let evidence = serde_json::json!({"state":"x".repeat(2 * 1024 * 1024),"questions":{"keep":{"type":"noul","instructions":"Keep?"}}});
    std::fs::write(&path, evidence.to_string()).unwrap();
    let reply = serde_json::json!({"success":true,"errors":[],"result":{"model":"clef","answers":{"keep":{"type":"noul","noul":0.7}},"usage":{"input_tokens":1,"output_tokens":0}}});
    let curl = sandbox.0.join("bin/curl");
    std::fs::write(
        &curl,
        format!(
            "#!/bin/sh\nset -eu\n/bin/cat >'{}'\nprintf '%s\\n%s' '{}' 200\n",
            recorded.display(),
            reply
        ),
    )
    .unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = sandbox.run(
        &["decide", path.to_str().unwrap()],
        &[
            ("CLOUDFLARE_ACCOUNT_ID", "0123456789abcdef0123456789abcdef"),
            ("CLOUDFLARE_API_TOKEN", "synthetic-clef-token"),
        ],
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    let mut expected = evidence;
    expected["model"] = serde_json::json!("clef");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(recorded).unwrap()).unwrap(),
        expected
    );
}

#[cfg(unix)]
#[test]
fn clef_decide_never_replays_an_uncertain_transport_failure() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("clef-uncertain-transport");
    let path = sandbox.0.join("evidence.json");
    let calls = sandbox.0.join("calls");
    std::fs::write(
        &path,
        r#"{"state":"synthetic","questions":{"keep":{"type":"noul","instructions":"Keep?"}}}"#,
    )
    .unwrap();
    let curl = sandbox.0.join("bin/curl");
    std::fs::write(&curl, format!("#!/bin/sh\nprintf x >>'{}'\n/bin/cat >/dev/null\nprintf PRIVATE_TRANSPORT >&2\nexit 7\n", calls.display())).unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = sandbox.run(
        &["decide", path.to_str().unwrap()],
        &[
            ("CLOUDFLARE_ACCOUNT_ID", "0123456789abcdef0123456789abcdef"),
            ("CLOUDFLARE_API_TOKEN", "synthetic-clef-token"),
        ],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr)
        .to_ascii_lowercase()
        .contains("clef_transport_failed"));
    assert!(!text(&output.stderr).contains("PRIVATE"));
    assert_eq!(std::fs::read(calls).unwrap(), b"x");
}

#[cfg(unix)]
#[test]
fn clef_decide_http_failures_redirects_unknown_status_and_malformed_output_get_one_attempt() {
    use std::os::unix::fs::PermissionsExt;
    for code in ["500", "503", "302", "999", "000", "malformed", "200"] {
        let sandbox = Sandbox::new(&format!("clef-one-post-{code}"));
        let path = sandbox.0.join("evidence.json");
        let calls = sandbox.0.join("calls");
        std::fs::write(
            &path,
            r#"{"state":"synthetic","questions":{"keep":{"type":"noul","instructions":"Keep?"}}}"#,
        )
        .unwrap();
        let curl = sandbox.0.join("bin/curl");
        let script = format!("#!/bin/sh\nset -eu\ntest \"$1\" = -q\nfor arg do\n case \"$arg\" in --location|--location-trusted|-L|--retry|--retry-all-errors|--retry-connrefused) exit 90;; esac\ndone\nprintf x >>'{}'\n/bin/cat >/dev/null\nprintf '%s\\n%s' PRIVATE_SYNTHETIC_RESPONSE '{code}'\n", calls.display());
        std::fs::write(&curl, script).unwrap();
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700)).unwrap();
        let output = sandbox.run(
            &["decide", path.to_str().unwrap()],
            &[
                ("CLOUDFLARE_ACCOUNT_ID", "0123456789abcdef0123456789abcdef"),
                ("CLOUDFLARE_API_TOKEN", "synthetic-clef-token"),
            ],
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!text(&output.stderr).contains("PRIVATE_SYNTHETIC_RESPONSE"));
        assert_eq!(std::fs::read(calls).unwrap(), b"x", "replayed HTTP {code}");
    }
}

#[cfg(unix)]
#[test]
fn clef_decide_timeout_aborts_owned_curl_without_replay() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new("clef-single-attempt-timeout");
    let path = sandbox.0.join("evidence.json");
    let calls = sandbox.0.join("calls");
    std::fs::write(
        &path,
        r#"{"state":"synthetic","questions":{"keep":{"type":"noul","instructions":"Keep?"}}}"#,
    )
    .unwrap();
    let curl = sandbox.0.join("bin/curl");
    std::fs::write(&curl, format!("#!/bin/sh\nprintf x >>'{}'\n/bin/cat >/dev/null\n/bin/sleep 5\nprintf 'unexpected\\n200'\n", calls.display())).unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started = std::time::Instant::now();
    let output = sandbox.run(
        &["decide", path.to_str().unwrap()],
        &[
            ("CLOUDFLARE_ACCOUNT_ID", "0123456789abcdef0123456789abcdef"),
            ("CLOUDFLARE_API_TOKEN", "synthetic-clef-token"),
            ("GOBSTOPPER_CLEF_TIMEOUT_MS", "1000"),
        ],
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr)
        .to_ascii_lowercase()
        .contains("clef_transport_failed"));
    assert_eq!(std::fs::read(calls).unwrap(), b"x");
}

#[test]
fn clef_auth_help_describes_environment_configuration_and_refused_storage() {
    let sandbox = Sandbox::new("clef-auth-help");
    let output = sandbox.run(&["auth", "--help"], &[]);
    assert!(output.status.success());
    let help = text(&output.stdout);
    assert!(help.contains("environment"));
    assert!(help.contains("unsupported"));
    assert!(!help.contains("Manage vaulted"));
    assert!(!help.contains("Remove the stored key"));
}

#[test]
fn clef_decide_invalid_evidence_is_private_and_precedes_credentials() {
    let sandbox = Sandbox::new("clef-invalid");
    let path = sandbox.0.join("PRIVATE_PATH.json");
    let bytes = br#"{"state":"PRIVATE_STATE","questions":{"q":{"type":"noul","instructions":"x"}},"images":["https://example.com/PRIVATE_IMAGE"]}"#;
    std::fs::write(&path, bytes).unwrap();
    let output = sandbox.run(&["decide", path.to_str().unwrap()], &[]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr)
        .to_ascii_lowercase()
        .contains("clef_request_invalid"));
    assert!(!text(&output.stderr).contains("PRIVATE"));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn auth_names_the_one_supported_provider() {
    let sandbox = Sandbox::new("auth");
    let output = sandbox.run(&["auth", "openai"], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        text(&output.stderr),
        "✗ Gobstopper uses Cloudflare Clef; Jev keys are not reused.\n→ gobstopper auth clef\n"
    );
}

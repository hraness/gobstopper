//! Child launch and readiness evidence use private settings and local sockets.
//! No real client, credential, service manager, or upstream is contacted.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gobstopper-launch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for dir in ["home", "codex", "claude", "config", "bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Self(root)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gobstopper"));
        command
            .env_clear()
            .current_dir(self.0.join("home"))
            .env("HOME", self.0.join("home"))
            .env("USERPROFILE", self.0.join("home"))
            .env("CODEX_HOME", self.0.join("codex"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("claude"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("LOCALAPPDATA", self.0.join("local-app-data"))
            .env("HRANESS_NO_UPDATE", "1")
            .env("PATH", self.0.join("bin"))
            .args(args);
        // Winsock expands %SystemRoot% to load its provider DLL. Keep this OS
        // dependency while isolating all client settings and credentials.
        #[cfg(windows)]
        command.env(
            "SystemRoot",
            std::env::var_os("SystemRoot").expect("Windows requires SystemRoot for socket tests"),
        );
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn manifest(&self, port: u16, options: Value) {
        let service = if cfg!(windows) {
            self.0.join("local-app-data/Gobstopper/service")
        } else {
            self.0.join("config/gobstopper/service")
        };
        std::fs::create_dir_all(&service).unwrap();
        let platform = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        std::fs::write(service.join("manifest.json"), json!({"schema":1,"platform":platform,"service_id":"aabbcc","executable":"/unused/gobstopper","serve_args":options,"port":port,"definition_sha256":"","rendered_definition":""}).to_string()).unwrap();
    }
    #[cfg(unix)]
    fn fake_client(&self, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.0.join("bin").join(name);
        std::fs::write(&path, b"#!/bin/sh\nprintf 'call\\n' >> child-count\nprintf '%s\\n' \"$@\"\nprintf 'BASE=%s\\n' \"$ANTHROPIC_BASE_URL\"\nexit 23\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn ready_server(
    ready: bool,
    service: &str,
    constrained: bool,
) -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let service = service.to_string();
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "launcher did not probe readiness"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 1024];
        let count = stream.read(&mut request).unwrap();
        assert!(request[..count].starts_with(b"GET /gobstopper/ready "));
        let body=json!({"name":"gobstopper-proxy","port":port,"pid":123,"instance_id":"abc","service_id":service,"ready":ready,"official_upstreams":true,"context_constrained":constrained}).to_string();
        write!(
            stream,
            "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            if ready { 200 } else { 503 },
            body.len()
        )
        .unwrap();
    });
    (port, server)
}

#[test]
fn proxy_launch_requires_explicit_codex_auth_and_refuses_custom_settings() {
    let sandbox = Sandbox::new();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let output = sandbox
            .command(&["proxy", "launch", "--client", "claude", "--print"])
            .env(
                "CLAUDE_CODE_USE_BEDROCK",
                std::ffi::OsString::from_vec(vec![0xff]),
            )
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    assert_eq!(
        sandbox
            .run(&["proxy", "launch", "--client", "codex", "--print"])
            .status
            .code(),
        Some(2)
    );
    for name in ["ANTHROPIC_BASE_URL", "anthropic_base_url"] {
        let mut env = serde_json::Map::new();
        env.insert(name.into(), json!("https://private.invalid"));
        std::fs::write(
            sandbox.0.join("claude/settings.json"),
            json!({"env":env}).to_string(),
        )
        .unwrap();
        let output = sandbox.run(&["proxy", "launch", "--client", "claude", "--print"]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private.invalid"));
    }
}

#[test]
fn proxy_launch_checks_managed_identity_and_respects_context_constraints() {
    for (ready, service, constrained, success, route) in [
        (true, "aabbcc", false, true, "proxy"),
        (false, "aabbcc", false, true, "direct"),
        (true, "wrong", false, false, ""),
        (false, "aabbcc", true, false, ""),
    ] {
        let sandbox = Sandbox::new();
        let (port, server) = ready_server(ready, service, constrained);
        sandbox.manifest(port, json!([]));
        let output = sandbox.run(&["proxy", "launch", "--client", "claude", "--print"]);
        server
            .join()
            .unwrap_or_else(|_| panic!("readiness fixture failed; launcher output: {output:?}"));
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if success {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["route"], route);
            assert_eq!(value["persistent_settings_changed"], false);
        }
    }
    let sandbox = Sandbox::new();
    sandbox.manifest(unused_port(), json!(["--context-window", "600000"]));
    assert!(!sandbox
        .run(&["proxy", "launch", "--client", "claude", "--print"])
        .status
        .success());
    sandbox.manifest(
        unused_port(),
        json!(["--openai-upstream=https://private.invalid"]),
    );
    assert!(!sandbox
        .run(&["proxy", "launch", "--client", "claude", "--print"])
        .status
        .success());
}

#[test]
fn proxy_launch_blackhole_probe_has_one_end_to_end_deadline() {
    let sandbox = Sandbox::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let _stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "launcher did not connect");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        accepted_tx.send(Instant::now()).unwrap();
        // Hold the connection until the launcher has already returned. EOF
        // must not masquerade as a successful deadline; bound broken fixtures.
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    let output = sandbox.run(&[
        "proxy",
        "launch",
        "--client",
        "claude",
        "--port",
        &port.to_string(),
        "--print",
    ]);
    let finished = Instant::now();
    let _ = release_tx.send(());
    // Exclude process startup and OS code-signature checks from the network
    // deadline, while requiring completion before the peer closes its socket.
    let accepted = accepted_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|error| {
            panic!("launcher did not connect: {error}; launcher output: {output:?}")
        });
    let elapsed = finished.duration_since(accepted);
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["route"],
        "direct"
    );
}

#[test]
fn proxy_launch_preserves_scoped_headers_only_on_a_healthy_proxy() {
    const CAPABILITY: &str = "scope-private-capability";
    for case in [
        "codex-literal",
        "codex-environment",
        "codex-layered",
        "claude-environment",
        "claude-settings",
        "claude-lowercase-settings",
        "claude-project",
    ] {
        for ready in [false, true] {
            let sandbox = Sandbox::new();
            let (port, server) = ready_server(ready, "aabbcc", false);
            sandbox.manifest(port, json!([]));
            let mut args = vec!["proxy", "launch", "--client"];
            let mut headers_environment = None;
            if case.starts_with("codex") {
                args.extend(["codex", "--codex-auth", "chatgpt", "--print"]);
                let field = if case == "codex-environment" {
                    "env_http_headers"
                } else {
                    "http_headers"
                };
                let value = if case == "codex-environment" {
                    headers_environment = Some(("TEST_SCOPE_CAPABILITY", CAPABILITY));
                    "TEST_SCOPE_CAPABILITY"
                } else {
                    CAPABILITY
                };
                std::fs::write(sandbox.0.join("codex/config.toml"), format!(
                    "model_provider='gobstopper'\n[model_providers.gobstopper]\nname='Gobstopper'\nbase_url='http://127.0.0.1:{port}/backend-api/codex'\nrequires_openai_auth=true\n[model_providers.gobstopper.{field}]\nX-GoBsToPpEr-ScOpE='{value}'\n"
                )).unwrap();
                if case == "codex-layered" {
                    std::fs::create_dir(sandbox.0.join("home/.codex")).unwrap();
                    std::fs::write(sandbox.0.join("home/.codex/config.toml"),
                        "[model_providers.gobstopper.http_headers]\nx-extra='merged-project-header'\n").unwrap();
                }
            } else {
                args.extend(["claude", "--print"]);
                let headers = format!("x-extra: ordinary\r\nX-GoBsToPpEr-ScOpE: {CAPABILITY}");
                if case == "claude-environment" {
                    headers_environment = Some(("ANTHROPIC_CUSTOM_HEADERS", CAPABILITY));
                } else {
                    let path = if case == "claude-project" {
                        std::fs::create_dir(sandbox.0.join("home/.claude")).unwrap();
                        sandbox.0.join("home/.claude/settings.local.json")
                    } else {
                        sandbox.0.join("claude/settings.json")
                    };
                    let key = if case == "claude-lowercase-settings" {
                        "anthropic_custom_headers"
                    } else {
                        "ANTHROPIC_CUSTOM_HEADERS"
                    };
                    let mut env = serde_json::Map::new();
                    env.insert(key.into(), Value::String(headers));
                    std::fs::write(path, json!({"env":env}).to_string()).unwrap();
                }
            }
            let mut command = sandbox.command(&args);
            if let Some((name, value)) = headers_environment {
                command.env(
                    name,
                    if name == "ANTHROPIC_CUSTOM_HEADERS" {
                        format!("x-extra: ordinary\nX-GoBsToPpEr-ScOpE: {value}")
                    } else {
                        value.to_string()
                    },
                );
            }
            let output = command.output().unwrap();
            server.join().unwrap_or_else(|_| {
                panic!("{case}: readiness fixture failed; launcher output: {output:?}")
            });
            assert_eq!(
                output.status.success(),
                ready,
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!String::from_utf8_lossy(&output.stderr).contains(CAPABILITY));
            assert!(!String::from_utf8_lossy(&output.stdout).contains(CAPABILITY));
            if ready {
                assert_eq!(
                    serde_json::from_slice::<Value>(&output.stdout).unwrap()["route"],
                    if case.starts_with("codex") {
                        "existing"
                    } else {
                        "proxy"
                    }
                );
            } else {
                assert!(String::from_utf8_lossy(&output.stderr).contains(
                    if case.starts_with("codex") {
                        "Codex direct fallback cannot be verified"
                    } else {
                        "context constraints"
                    }
                ));
            }
        }
    }
}

#[test]
fn proxy_launch_refuses_scoped_builtin_codex_headers_and_unreachable_proxy() {
    for provider in ["openai", "gobstopper"] {
        for field in ["http_headers", "env_http_headers"] {
            let sandbox = Sandbox::new();
            let port = unused_port();
            sandbox.manifest(port, json!([]));
            std::fs::write(sandbox.0.join("codex/config.toml"), format!(
                "model_provider='{provider}'\n[model_providers.{provider}]\nbase_url='http://127.0.0.1:{port}/backend-api/codex'\nrequires_openai_auth=true\n[model_providers.{provider}.{field}]\nx-gobstopper-scope='private-capability-or-variable'\n"
            )).unwrap();
            let output = sandbox.run(&[
                "proxy",
                "launch",
                "--client",
                "codex",
                "--codex-auth",
                "chatgpt",
                "--print",
            ]);
            assert!(!output.status.success(), "{provider}/{field}");
            assert!(
                !String::from_utf8_lossy(&output.stderr).contains("private-capability-or-variable")
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn codex_launch_never_changes_an_existing_direct_route_or_falls_back() {
    for configuration in ["absent", "openai", "custom-direct", "top-level-route"] {
        let sandbox = Sandbox::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        sandbox.manifest(port, json!([]));
        sandbox.fake_client("codex");
        let contents = match configuration {
            "absent" => String::new(),
            "openai" => "model_provider='openai'".into(),
            "top-level-route" => "openai_base_url='https://private.invalid'".into(),
            _ => "model_provider='custom'\n[model_providers.custom]\nbase_url='https://chatgpt.com/backend-api/codex'\nrequires_openai_auth=true\n".into(),
        };
        std::fs::write(sandbox.0.join("codex/config.toml"), contents).unwrap();
        let output = sandbox
            .command(&[
                "proxy",
                "launch",
                "--client",
                "codex",
                "--codex-auth",
                "chatgpt",
            ])
            .env("OPENAI_BASE_URL", format!("http://127.0.0.1:{port}/v1"))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{configuration}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private.invalid"));
        assert!(!sandbox.0.join("home/child-count").exists());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    for responds in [false, true] {
        let sandbox = Sandbox::new();
        let (port, server) = if responds {
            let (port, server) = ready_server(false, "aabbcc", false);
            (port, Some(server))
        } else {
            (unused_port(), None)
        };
        sandbox.manifest(port, json!([]));
        sandbox.fake_client("codex");
        std::fs::write(sandbox.0.join("codex/config.toml"), format!("model_provider='custom'\n[model_providers.custom]\nbase_url='http://127.0.0.1:{port}/backend-api/codex'\nrequires_openai_auth=true\n")).unwrap();
        let output = sandbox.run(&[
            "proxy",
            "launch",
            "--client",
            "codex",
            "--codex-auth",
            "chatgpt",
        ]);
        if let Some(server) = server {
            server.join().unwrap();
        }
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("Codex direct fallback cannot be verified"));
        assert!(!sandbox.0.join("home/child-count").exists());
    }
}

#[test]
#[cfg(unix)]
fn proxy_launch_never_prints_malformed_codex_settings() {
    let canary = "PRIVATE_AUTH_CANARY_MUST_STAY_PRIVATE";
    for invalid_utf8 in [false, true] {
        let sandbox = Sandbox::new();
        let mut bytes = format!(
            "[model_providers.gobstopper]\nhttp_headers = {{ Authorization = \"{canary}\"\n"
        )
        .into_bytes();
        if invalid_utf8 {
            bytes.push(0xff);
        }
        std::fs::write(sandbox.0.join("codex/config.toml"), bytes).unwrap();
        let output = sandbox.run(&[
            "proxy",
            "launch",
            "--client",
            "codex",
            "--codex-auth",
            "chatgpt",
            "--print",
        ]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains(canary));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(canary));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Codex settings could not be parsed")
        );
    }
}

#[test]
#[cfg(unix)]
fn proxy_launch_refuses_uninspected_codex_managed_configuration() {
    for path in [
        "codex/managed_config.toml",
        "home/.codex/managed_config.toml",
    ] {
        let sandbox = Sandbox::new();
        let path = sandbox.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "private, deliberately unparsed content").unwrap();
        let output = sandbox.run(&[
            "proxy",
            "launch",
            "--client",
            "codex",
            "--codex-auth",
            "chatgpt",
            "--print",
        ]);
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("deliberately unparsed"));
    }
}

#[test]
#[cfg(unix)]
fn proxy_launch_refuses_builtin_codex_route_and_authentication_overrides() {
    for setting in [
        "base_url = \"https://private.invalid\"",
        "env_key = \"CUSTOM_AUTH_KEY\"",
        "requires_openai_auth = false",
    ] {
        for available in [false, true] {
            let sandbox = Sandbox::new();
            let (port, listener) = if available {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                listener.set_nonblocking(true).unwrap();
                (listener.local_addr().unwrap().port(), Some(listener))
            } else {
                (unused_port(), None)
            };
            std::fs::write(
                sandbox.0.join("codex/config.toml"),
                format!("model_provider = \"openai\"\n[model_providers.openai]\n{setting}\n"),
            )
            .unwrap();
            let output = sandbox.run(&[
                "proxy",
                "launch",
                "--port",
                &port.to_string(),
                "--client",
                "codex",
                "--codex-auth",
                "chatgpt",
                "--print",
            ]);
            assert!(!output.status.success(), "{setting}/{available}");
            let text = String::from_utf8_lossy(&output.stderr);
            assert!(!text.contains("private.invalid") && !text.contains("CUSTOM_AUTH_KEY"));
            if let Some(listener) = listener {
                // Refuse the unsafe configuration before readiness can choose
                // either a healthy proxy or a direct route.
                assert_eq!(
                    listener.accept().unwrap_err().kind(),
                    std::io::ErrorKind::WouldBlock
                );
            }
        }
    }
}

#[test]
#[cfg(unix)]
fn proxy_launch_preserves_saved_routes_and_child_exit_without_replaying() {
    for client in ["claude", "codex"] {
        let sandbox = Sandbox::new();
        let (port, server) = if client == "codex" {
            let (port, server) = ready_server(true, "aabbcc", false);
            (port, Some(server))
        } else {
            (unused_port(), None)
        };
        sandbox.manifest(port, json!([]));
        sandbox.fake_client(client);
        let (path, original) = if client == "claude" {
            (
                sandbox.0.join("claude/settings.json"),
                json!({"env":{"ANTHROPIC_BASE_URL":format!("http://127.0.0.1:{port}")}})
                    .to_string(),
            )
        } else {
            (sandbox.0.join("codex/config.toml"),format!("model_provider = \"gobstopper\"\n[model_providers.gobstopper]\nname = \"Gobstopper\"\nbase_url = \"http://127.0.0.1:{port}/backend-api/codex\"\nwire_api = \"responses\"\nrequires_openai_auth = true\n"))
        };
        std::fs::write(&path, &original).unwrap();
        let mut args = vec!["proxy", "launch", "--client", client];
        if client == "codex" {
            args.extend(["--codex-auth", "chatgpt"]);
        }
        args.extend(["--", "--help"]);
        let output = sandbox.run(&args);
        if let Some(server) = server {
            server.join().unwrap();
        }
        assert_eq!(
            output.status.code(),
            Some(23),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        if client == "claude" {
            assert!(text.contains("BASE=https://api.anthropic.com"));
            assert!(text.contains("--settings\n"));
        } else {
            assert!(text.starts_with("--no-daemon\n"));
            assert!(!text.contains("model_provider"));
            assert!(!text.contains("base_url") && !text.contains("http_headers"));
            assert!(!text.contains("requires_openai_auth"));
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
        assert_eq!(
            std::fs::read_to_string(sandbox.0.join("home/child-count")).unwrap(),
            "call\n"
        );
    }
}

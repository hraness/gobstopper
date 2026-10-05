use std::process::Command;

#[test]
fn authentication_storage_is_refused_without_waiting_for_stdin_eof() {
    use std::io::Write;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    for oversized in [true, false] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .args(["auth", "clef"])
            .env_clear()
            .env("PATH", "")
            .env("XDG_CONFIG_HOME", "/nonexistent-gobstopper-auth-test")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let sentinel = "SENSITIVE_SYNTHETIC_KEY";
        let bytes = if oversized {
            sentinel.repeat(4_000)
        } else {
            sentinel.into()
        };
        // Keep the pipe open: oversized input must fail before EOF, and a small
        // unfinished input must hit its deadline without contacting a provider.
        let _ = input.write_all(bytes.as_bytes());
        let deadline = Instant::now() + Duration::from_secs(8);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("authentication input reader exceeded its bound");
        }
        drop(input);
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("environment-only"));
        assert!(!error.contains(sentinel));
    }
}

#[test]
fn scoring_resolution_has_no_storage_or_credential_file_lookup() {
    let source = include_str!("../src/jev.rs");
    let resolver = source
        .split("fn resolve_key_from(")
        .nth(1)
        .unwrap()
        .split("pub fn env_key()")
        .next()
        .unwrap();
    assert!(resolver.contains("env_key_from(get)"));
    for forbidden in [
        "stored()",
        "GOBSTOPPER_CLEF_USE_KEYCHAIN",
        "clef_key_state(",
        "std::fs::",
    ] {
        assert!(!resolver.contains(forbidden));
    }
    let secrets = include_str!("../src/secrets.rs");
    for forbidden in [
        "keyring::Entry::new",
        ".get_password(",
        ".set_password(",
        ".delete_credential(",
    ] {
        assert!(!secrets.contains(forbidden));
    }
}

#[test]
fn cloudflare_auth_never_uses_storage_even_when_legacy_toggle_is_enabled() {
    for provider in ["clef", "cloudflare"] {
        for arguments in [
            vec!["auth", provider],
            vec!["auth", provider, "--delete"],
            vec!["auth", provider, "--status"],
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
                .args(&arguments)
                .env_clear()
                .env("PATH", "")
                .env("GOBSTOPPER_CLEF_USE_KEYCHAIN", "1")
                .env("TYPESAFE_API_KEY", "PRIVATE_SYNTHETIC_LEGACY")
                .env("XDG_CONFIG_HOME", "/nonexistent-gobstopper-auth-test")
                .output()
                .unwrap();
            if arguments.contains(&"--status") {
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    "clef: no environment token configured\n"
                );
            } else {
                assert!(!output.status.success());
                assert!(output.stdout.is_empty());
                assert!(String::from_utf8_lossy(&output.stderr).contains("environment-only"));
            }
            assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE_SYNTHETIC_LEGACY"));
        }
    }
}

#[test]
fn legacy_auth_is_rejected_before_any_credential_lookup() {
    for provider in ["jev", "typesafe"] {
        let output = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .args(["auth", provider, "--status"])
            .env_clear()
            .env("PATH", "")
            .env("TYPESAFE_API_KEY", "PRIVATE_LEGACY_SENTINEL")
            .env("XDG_CONFIG_HOME", "/nonexistent-gobstopper-auth-test")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("Jev keys are not reused"));
        assert!(!error.contains("PRIVATE_LEGACY_SENTINEL"));
    }
}

#[test]
fn status_discloses_credential_source_but_never_key_fragments() {
    for key in [
        "tsk_SENTINEL_CREDENTIAL_1234",
        "秘密鍵の取り扱いを確認する試験",
    ] {
        // No curl on PATH: this checks output and Unicode handling without
        // contacting a provider, touching a keychain, or reading user config.
        let output = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .args(["auth", "clef", "--status"])
            .env_clear()
            .env("PATH", "")
            .env("CLOUDFLARE_API_TOKEN", key)
            .env("CLOUDFLARE_ACCOUNT_ID", "0123456789abcdef0123456789abcdef")
            .env("XDG_CONFIG_HOME", "/nonexistent-gobstopper-auth-test")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "clef: env CLOUDFLARE_API_TOKEN key configured — account configured; not live-verified\n"
        );
        assert!(output.stderr.is_empty());
    }
}

use std::process::Command;

#[test]
fn authentication_input_has_byte_and_eof_bounds_before_any_credential_effect() {
    use std::io::Write;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    for oversized in [true, false] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .args(["auth", "jev"])
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
        assert!(error.contains("EOF within 64 KiB and five seconds"));
        assert!(!error.contains(sentinel));
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
            .args(["auth", "jev", "--status"])
            .env_clear()
            .env("PATH", "")
            .env("TYPESAFE_API_KEY", key)
            .env("XDG_CONFIG_HOME", "/nonexistent-gobstopper-auth-test")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "jev: env TYPESAFE_API_KEY key configured — could not verify (network/API error)\n"
        );
        assert!(output.stderr.is_empty());
    }
}

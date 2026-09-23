use std::process::Command;

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

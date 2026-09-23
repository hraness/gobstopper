//! Process fixtures only: no installed provider, account, or user session.
#![cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::PathBuf, time::Duration};

struct Fixture(PathBuf);
impl Fixture {
    fn new(mode: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "gobstopper-native-child-{}-{}-{mode}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let script = path.join("provider");
        fs::write(
            &script,
            format!(
                r#"#!/bin/sh
set -eu
(
    printf ready > "$CLAUDE_CONFIG_DIR/child-ready"
    sleep 2
    printf survived > "$CLAUDE_CONFIG_DIR/child-survived"
) &
while [ ! -f "$CLAUDE_CONFIG_DIR/child-ready" ]; do sleep 0.01; done
{}
"#,
                if mode == "timeout" {
                    "sleep 60"
                } else {
                    "exit 0"
                }
            ),
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn claude_owned_group_cleanup_covers_normal_exit_and_timeout() {
    for mode in ["normal", "timeout"] {
        let fixture = Fixture::new(mode);
        let started = std::time::Instant::now();
        let result = gobstopper_adapters::claude::headless_compact_in_home(
            &fixture.0.join("provider"),
            "synthetic-session",
            1,
            Some(&fixture.0),
        );
        assert_eq!(result.is_ok(), mode == "normal");
        assert!(fixture.0.join("child-ready").is_file());
        while started.elapsed() < Duration::from_millis(2400) {
            assert!(
                !fixture.0.join("child-survived").exists(),
                "owned descendant outlived cleanup"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!fixture.0.join("child-survived").exists());
    }
}

#[test]
fn invalid_native_identity_or_deadline_never_starts_provider() {
    let fixture = Fixture::new("normal");
    for (identity, deadline) in [("--wrong", 1), ("synthetic", 0), ("synthetic", u64::MAX)] {
        assert!(gobstopper_adapters::claude::headless_compact_in_home(
            &fixture.0.join("provider"),
            identity,
            deadline,
            Some(&fixture.0)
        )
        .is_err());
    }
    assert!(!fixture.0.join("child-ready").exists());
}

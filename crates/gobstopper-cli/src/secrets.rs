//! OS-native secret storage for provider API keys, plus the bounded
//! clipboard read used by `gobstopper auth` onboarding.
//!
//! Keys live in the platform credential store (macOS Keychain, Windows
//! Credential Manager, Linux Secret Service) under the `gobstopper`
//! service — never in config files, transcripts, or logs. Resolution for
//! consumers is always env-first so CI and ad-hoc shells keep working
//! without a stored credential.

use std::process::Command;

/// Keyring service+account pair for the TypeSafe/Jev API key.
const JEV_SERVICE: &str = "gobstopper";
const JEV_ACCOUNT: &str = "typesafe-api-key";

/// Stored key lookup. Returns `None` when no credential exists or the
/// store is unavailable (headless Linux without Secret Service, locked
/// keychain) — callers fall back to env keys or skip the feature.
pub fn jev_key() -> Option<String> {
    let entry = keyring::Entry::new(JEV_SERVICE, JEV_ACCOUNT).ok()?;
    entry.get_password().ok().filter(|k| !k.trim().is_empty())
}

/// Store a key in the OS credential store.
pub fn store_jev_key(key: &str) -> anyhow::Result<()> {
    let entry = keyring::Entry::new(JEV_SERVICE, JEV_ACCOUNT)
        .map_err(|e| anyhow::anyhow!("keychain unavailable: {e}"))?;
    entry
        .set_password(key)
        .map_err(|e| anyhow::anyhow!("keychain store failed: {e}"))
}

/// Remove the stored key. Ok(true) = a credential was deleted.
pub fn delete_jev_key() -> anyhow::Result<bool> {
    let entry = keyring::Entry::new(JEV_SERVICE, JEV_ACCOUNT)
        .map_err(|e| anyhow::anyhow!("keychain unavailable: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(anyhow::anyhow!("keychain delete failed: {e}")),
    }
}

/// Mask a key for display: keep the first 3 and last 4 chars, hide the
/// rest. Keys too short to split degrade to fully masked.
pub fn masked(key: &str) -> String {
    let k = key.trim();
    let n = k.chars().count();
    if n <= 8 {
        "…".to_string()
    } else {
        format!("{}…{}", &k[..3], &k[n - 4..])
    }
}

/// Read the system clipboard (bounded) for onboarding. Returns `None`
/// when no clipboard tool exists, it fails, or the content is not a
/// plausible API key.
pub fn clipboard_secret() -> Option<String> {
    let candidates: &[(&str, &[&str])] = &[
        ("pbpaste", &[]),                                             // macOS
        ("wl-paste", &["-n"]),                                        // Wayland
        ("xclip", &["-selection", "clipboard", "-o"]),                // X11
        ("xsel", &["--clipboard", "--output"]),                       // X11 alt
        ("powershell", &["-NoProfile", "-Command", "Get-Clipboard"]), // Windows
    ];
    for (bin, args) in candidates {
        let mut cmd = Command::new(bin);
        cmd.args(*args);
        let Ok(out) = gobstopper_adapters::plugins::run_bounded(cmd, Vec::new(), 3_000, 16 * 1024)
        else {
            continue;
        };
        let Ok(text) = String::from_utf8(out) else {
            continue;
        };
        let text = text.trim();
        if plausible_key(text) {
            return Some(text.to_string());
        }
    }
    None
}

/// A plausible pasted API key: single line, 12..512 chars, no spaces or
/// control characters. Deliberately permissive about shape — real
/// validation happens against the API, not a regex.
fn plausible_key(s: &str) -> bool {
    let n = s.chars().count();
    (12..=512).contains(&n)
        && !s.contains(char::is_whitespace)
        && s.chars().all(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masked_shows_only_edges() {
        assert_eq!(masked("tsk_abcdefghijklmnop1234"), "tsk…1234");
        assert_eq!(masked("short"), "…");
    }

    #[test]
    fn plausible_key_filters() {
        assert!(plausible_key("tsk_abcdefghijklmnop1234"));
        assert!(!plausible_key("short"));
        assert!(!plausible_key("has a space in the middle of it"));
        assert!(!plausible_key("line\nbreak_abcdefghijklmnop"));
        assert!(!plausible_key(&"x".repeat(600)));
    }
}

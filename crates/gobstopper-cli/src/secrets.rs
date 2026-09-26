//! OS-native secret storage for provider API keys, the bounded clipboard
//! read used by `gobstopper auth` onboarding, and secret-safe curl auth.
//!
//! Keys live in the platform credential store (macOS Keychain, Windows
//! Credential Manager, Linux Secret Service) under the `gobstopper`
//! service — never in config files, transcripts, logs, or subprocess
//! argv. Resolution for consumers is always env-first so CI and ad-hoc
//! shells keep working without a stored credential.

use std::process::Command;

/// Keyring service+account pair for the TypeSafe/Jev API key.
const JEV_SERVICE: &str = "gobstopper";
const JEV_ACCOUNT: &str = "typesafe-api-key";
const CURL_BEARER_ENV: &str = "GOBSTOPPER_CURL_BEARER";

/// Configure curl bearer authentication without placing the credential
/// in process argv. curl 8.3+ imports the child-only environment value
/// and expands it directly into the header. `-q` must be the first curl
/// argument; it prevents a user `.curlrc` from enabling verbose/header
/// output that could disclose the expanded value.
pub(crate) fn configure_curl_bearer(command: &mut Command, key: &str) -> anyhow::Result<()> {
    if !safe_bearer_key(key) {
        anyhow::bail!("api_key_invalid");
    }
    for name in [
        "AI_GATEWAY_API_KEY",
        "GOBSTOPPER_LLM_API_KEY",
        "TYPESAFE_API_KEY",
        "GOBSTOPPER_JEV_API_KEY",
    ] {
        command.env_remove(name);
    }
    command
        .arg("-q")
        .arg("--variable")
        .arg(format!("%{CURL_BEARER_ENV}"))
        .arg("--expand-header")
        .arg(format!("Authorization: Bearer {{{{{CURL_BEARER_ENV}}}}}"))
        .env(CURL_BEARER_ENV, key);
    Ok(())
}

/// Stored key lookup. Returns `None` when no credential exists or the
/// store is unavailable (headless Linux without Secret Service, locked
/// keychain) — callers fall back to env keys or skip the feature.
pub fn jev_key() -> Option<String> {
    match jev_key_state() {
        KeyState::Stored(key) => Some(key),
        _ => None,
    }
}

/// What the credential store says about the stored key, so a denied read
/// is never reported as "no key".
pub enum KeyState {
    Stored(String),
    Absent,
    /// The item exists but macOS refused access (denied, cancelled, or the
    /// keychain is locked).
    Denied,
    /// No credential store is reachable here.
    Unavailable,
}

pub fn jev_key_state() -> KeyState {
    let Ok(entry) = keyring::Entry::new(JEV_SERVICE, JEV_ACCOUNT) else {
        return KeyState::Unavailable;
    };
    match entry.get_password() {
        Ok(key) if !key.trim().is_empty() => KeyState::Stored(key),
        Ok(_) | Err(keyring::Error::NoEntry) => KeyState::Absent,
        Err(keyring::Error::NoStorageAccess(_)) => KeyState::Unavailable,
        // Only macOS asks the user; elsewhere a failure means the store
        // (Secret Service, D-Bus) isn't reachable.
        Err(_) if cfg!(target_os = "macos") => KeyState::Denied,
        Err(_) => KeyState::Unavailable,
    }
}

/// The recovery copy for a keychain failure (SPEC keychain templates).
fn keychain_error(action: &str, error: &keyring::Error) -> anyhow::Error {
    match error {
        _ if !cfg!(target_os = "macos") => crate::ux::guided_detail(
            "keychain-unavailable",
            format!("Gobstopper can't {action}: no keychain is available here"),
            "Set TYPESAFE_API_KEY in your environment instead.",
            "export TYPESAFE_API_KEY=<your key>",
        ),
        keyring::Error::NoStorageAccess(_) => crate::ux::guided_detail(
            "keychain-unavailable",
            format!("Gobstopper can't {action}: no keychain is available here"),
            "Set TYPESAFE_API_KEY in your environment instead.",
            "export TYPESAFE_API_KEY=<your key>",
        ),
        _ => crate::ux::guided_detail(
            "keychain-denied",
            format!("Gobstopper can't {action}: the keychain request was denied"),
            "Run it again and choose Always Allow when macOS asks.",
            "gobstopper auth jev",
        ),
    }
}

/// Store a key in the OS credential store.
pub fn store_jev_key(key: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        safe_bearer_key(key),
        "that isn't a usable API key: it must be one line of printable text"
    );
    let entry = keyring::Entry::new(JEV_SERVICE, JEV_ACCOUNT)
        .map_err(|error| keychain_error("store your TypeSafe key", &error))?;
    entry
        .set_password(key)
        .map_err(|error| keychain_error("store your TypeSafe key", &error))
}

/// Remove the stored key. Ok(true) = a credential was deleted.
pub fn delete_jev_key() -> anyhow::Result<bool> {
    let entry = keyring::Entry::new(JEV_SERVICE, JEV_ACCOUNT)
        .map_err(|error| keychain_error("remove your TypeSafe key", &error))?;
    match entry.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(keychain_error("remove your TypeSafe key", &error)),
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

fn safe_bearer_key(s: &str) -> bool {
    let n = s.len();
    (1..=4096).contains(&n)
        && !s.contains(char::is_whitespace)
        && s.chars().all(|c| !c.is_control())
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
    fn plausible_key_filters() {
        assert!(plausible_key("tsk_abcdefghijklmnop1234"));
        assert!(plausible_key("秘密鍵の取り扱いを確認する試験"));
        assert!(!plausible_key("short"));
        assert!(!plausible_key("has a space in the middle of it"));
        assert!(!plausible_key("line\nbreak_abcdefghijklmnop"));
        assert!(!plausible_key(&"x".repeat(600)));
    }

    #[test]
    fn curl_bearer_stays_out_of_argv() {
        let key = "tsk_abcdefghijklmnop1234";
        let mut command = Command::new("curl");
        configure_curl_bearer(&mut command, key).unwrap();
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.first().map(String::as_str), Some("-q"));
        assert!(args.iter().all(|arg| !arg.contains(key)));
        assert!(args
            .iter()
            .any(|arg| arg == "Authorization: Bearer {{GOBSTOPPER_CURL_BEARER}}"));
        let env = command
            .get_envs()
            .find(|(name, _)| *name == CURL_BEARER_ENV)
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(env.as_deref(), Some(key));
    }

    #[test]
    fn curl_bearer_rejects_header_injection() {
        let mut command = Command::new("curl");
        assert!(configure_curl_bearer(&mut command, "long-enough\r\nX-Evil: yes").is_err());
        assert_eq!(command.get_args().count(), 0);
        assert!(configure_curl_bearer(&mut Command::new("curl"), "").is_err());
        assert!(configure_curl_bearer(&mut Command::new("curl"), &"x".repeat(4097)).is_err());
        assert!(configure_curl_bearer(&mut Command::new("curl"), "x").is_ok());
    }
}

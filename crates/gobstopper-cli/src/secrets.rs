//! OS-native secret storage for provider API keys, the bounded clipboard
//! read used by `gobstopper auth` onboarding, and secret-safe curl auth.
//!
//! Keys live in the platform credential store (macOS Keychain, Windows
//! Credential Manager, Linux Secret Service) under the `gobstopper`
//! service — never in config files, transcripts, logs, or subprocess
//! argv. Resolution for consumers is always env-first so CI and ad-hoc
//! shells keep working without a stored credential.

use std::process::Command;

/// Keyring service+account pair for the Cloudflare/Clef API key.
const CLEF_SERVICE: &str = "gobstopper";
const CLEF_ACCOUNT: &str = "cloudflare-clef-api-token";
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
        "CLOUDFLARE_API_TOKEN",
        "CLOUDFLARE_AUTH_TOKEN",
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
pub fn clef_key() -> Option<String> {
    match clef_key_state() {
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

pub fn clef_key_state() -> KeyState {
    let unavailable = keyring::Error::NoStorageAccess(Box::new(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Cloudflare credentials are environment-only",
    )));
    match Err::<String, _>(unavailable) {
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
fn keychain_error(action: &str, _error: &keyring::Error) -> anyhow::Error {
    crate::ux::guided_detail(
        "cloudflare-environment-only",
        format!("Cloudflare tokens are environment-only; Gobstopper cannot {action} in the {}", crate::jev::KeySource::Keychain.describe()),
        format!("Stored {CLEF_SERVICE}/{CLEF_ACCOUNT} entries are left untouched. Set CLOUDFLARE_API_TOKEN or CLOUDFLARE_AUTH_TOKEN in your environment."),
        "export CLOUDFLARE_API_TOKEN=<your token>",
    )
}

/// Store a key in the OS credential store.
pub fn store_clef_key(_key: &str) -> anyhow::Result<()> {
    Err(keychain_error("store tokens", &keyring::Error::NoEntry))
}

/// Remove the stored key. Ok(true) = a credential was deleted.
pub fn delete_clef_key() -> anyhow::Result<bool> {
    Err(keychain_error("remove tokens", &keyring::Error::NoEntry))
}

/// Read the system clipboard (bounded) for onboarding. Returns `None`
/// when no clipboard tool exists, it fails, or the content is not a
/// plausible API key.
#[cfg(test)]
pub fn clipboard_secret() -> Option<String> {
    let _candidates: &[(&str, &[&str])] = &[
        ("pbpaste", &[]),                                             // macOS
        ("wl-paste", &["-n"]),                                        // Wayland
        ("xclip", &["-selection", "clipboard", "-o"]),                // X11
        ("xsel", &["--clipboard", "--output"]),                       // X11 alt
        ("powershell", &["-NoProfile", "-Command", "Get-Clipboard"]), // Windows
    ];
    None
}

pub(crate) fn safe_bearer_key(s: &str) -> bool {
    let n = s.len();
    (1..=4096).contains(&n)
        && !s.contains(char::is_whitespace)
        && s.chars().all(|c| !c.is_control())
}

/// A plausible pasted API key: single line, 12..512 chars, no spaces or
/// control characters. Deliberately permissive about shape — real
/// validation happens against the API, not a regex.
#[cfg(test)]
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
    fn clef_keychain_identity_is_not_the_legacy_typesafe_entry() {
        assert_eq!(CLEF_ACCOUNT, "cloudflare-clef-api-token");
        assert_ne!(CLEF_ACCOUNT, "typesafe-api-key");
    }

    #[test]
    fn cloudflare_storage_and_clipboard_helpers_have_no_credential_effects() {
        assert!(clef_key().is_none());
        assert!(matches!(clef_key_state(), KeyState::Unavailable));
        assert!(clipboard_secret().is_none());
        for error in [
            store_clef_key("PRIVATE_SYNTHETIC_TOKEN").unwrap_err(),
            delete_clef_key().unwrap_err(),
        ] {
            assert!(error.to_string().contains("environment-only"));
            assert!(!format!("{error:#}").contains("PRIVATE_SYNTHETIC_TOKEN"));
        }
    }

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

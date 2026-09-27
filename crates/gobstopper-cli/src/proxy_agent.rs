//! `gobstopper proxy install|uninstall`: a macOS LaunchAgent that keeps the
//! request proxy running across logins, with the Background Items notice
//! said before macOS shows it.
//!
//! The agent runs `gobstopper proxy serve` with the settings given to
//! `install`, logs to `~/Library/Logs/gobstopper-proxy.log`, and uses the
//! label `sh.gobstopper.proxy` that `docs/proxy.md` has always used, so a
//! hand-written agent is found and never silently replaced.

use crate::ux::{self, Style, Symbol};
use anyhow::{Context, Result};
use hraness_cli_kit::permissions::{self, presets, PermissionNeed, ProductRef};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "sh.gobstopper.proxy";
/// Test hook: the `launchctl` to run. Never set outside tests.
const LAUNCHCTL_ENV: &str = "GOBSTOPPER_TEST_LAUNCHCTL";

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn plist_path() -> Result<PathBuf> {
    Ok(home()?
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

pub fn log_path() -> Result<PathBuf> {
    Ok(home()?.join("Library/Logs/gobstopper-proxy.log"))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The LaunchAgent property list for `gobstopper proxy serve <serve_args>`.
pub fn plist(executable: &Path, serve_args: &[String], log: &Path) -> String {
    let mut arguments = vec![
        executable.display().to_string(),
        "proxy".to_owned(),
        "serve".to_owned(),
    ];
    arguments.extend(serve_args.iter().cloned());
    let arguments: String = arguments
        .iter()
        .map(|argument| format!("    <string>{}</string>\n", xml_escape(argument)))
        .collect();
    let log = xml_escape(&log.display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key><string>{LABEL}</string>\n\
  <key>ProgramArguments</key>\n\
  <array>\n\
{arguments}  </array>\n\
  <key>RunAtLoad</key><true/>\n\
  <key>KeepAlive</key><true/>\n\
  <key>StandardOutPath</key><string>{log}</string>\n\
  <key>StandardErrorPath</key><string>{log}</string>\n\
</dict>\n\
</plist>\n"
    )
}

/// The Background Items notice: the kit's login-item preset (notifies, so
/// no confirm line). Login Items names the `gobstopper` executable itself.
fn login_item_need(port: u16) -> PermissionNeed {
    let mut need = presets::login_item(ProductRef::new("gobstopper", "gobstopper"));
    need.why = format!(
        "It keeps the request proxy on 127.0.0.1:{port} running so Claude Code and Codex requests stay small."
    );
    need
}

/// The notice as a person reads it.
#[cfg(test)]
fn login_item_notice(port: u16, style: Style) -> String {
    use hraness_cli_kit::permissions::{NoticeKind, Surface};
    let need = login_item_need(port);
    let env = hraness_cli_kit::audience::process_env;
    let notice = permissions::render_pre_prompt(&need, Surface::Cli, &env);
    permissions::format_notice(&notice, NoticeKind::PrePrompt, false, style)
}

/// Say the notice for the audience: text for a person, one JSON line for an
/// agent, nothing for a quiet reader. It never waits for input.
fn say_notice(port: u16) {
    let _ = permissions::pre_prompt(&login_item_need(port), None, &mut permissions::ProcessIo);
}

fn launchctl() -> Command {
    Command::new(std::env::var_os(LAUNCHCTL_ENV).unwrap_or_else(|| "launchctl".into()))
}

fn domain() -> String {
    // SAFETY: getuid has no preconditions and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

/// `gobstopper proxy install`: write the agent, load it, and wait for the
/// proxy to answer. `print` shows the property list and changes nothing.
pub fn install(serve_args: &[String], port: u16, replace: bool, print: bool) -> Result<()> {
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .context("can't find the gobstopper executable to run at login")?;
    let path = plist_path()?;
    let log = log_path()?;
    let text = plist(&executable, serve_args, &log);
    if print {
        print!("{text}");
        return Ok(());
    }
    if path.exists() && !replace {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        if current == text {
            return Err(ux::guided(
                format!("The proxy is already installed at {}", path.display()),
                "gobstopper proxy status",
            ));
        }
        return Err(ux::guided(
            format!(
                "A different proxy LaunchAgent already exists at {}",
                path.display()
            ),
            "gobstopper proxy install --replace",
        ));
    }
    if !cfg!(target_os = "macos") && std::env::var_os(LAUNCHCTL_ENV).is_none() {
        return Err(ux::guided(
            "Starting the proxy at login needs macOS; elsewhere, run it from your service manager",
            "gobstopper proxy serve",
        ));
    }
    if path.exists() {
        // Replacing: stop the old agent first. It may not be loaded.
        let _ = launchctl()
            .args(["bootout", &format!("{}/{LABEL}", domain())])
            .output();
        wait_for(|| !crate::proxy::is_answering(port));
    }
    if crate::proxy::is_answering(port) {
        // A proxy started by hand holds the port; the agent's proxy would
        // fail to bind and restart forever.
        return Err(ux::guided(
            format!(
                "A gobstopper proxy is already running on 127.0.0.1:{port}. Stop it first, or install on another port"
            ),
            format!("gobstopper proxy install --port {}", port.wrapping_add(1)),
        ));
    }
    say_notice(port);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("can't create {}", parent.display()))?;
    }
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("can't create {}", parent.display()))?;
    }
    std::fs::write(&path, &text).with_context(|| format!("can't write {}", path.display()))?;
    let output = launchctl()
        .args(["bootstrap", &domain()])
        .arg(&path)
        .output()
        .context("can't run launchctl")?;
    if !output.status.success() {
        return Err(ux::guided(
            format!(
                "macOS didn't load the proxy LaunchAgent (launchctl exit {}). The file is at {}",
                output.status.code().unwrap_or(-1),
                path.display()
            ),
            "gobstopper proxy uninstall",
        ));
    }
    let style = Style::stdout();
    let answering = wait_for(|| crate::proxy::is_answering(port));
    if answering {
        println!(
            "{} The proxy is running on http://127.0.0.1:{port} and starts at login.",
            style.symbol(Symbol::Ok)
        );
    } else {
        println!(
            "{} The proxy starts at login. It isn't answering yet; its log is {}.",
            style.symbol(Symbol::Ok),
            log.display()
        );
    }
    ux::next_hint(&format!(
        "export ANTHROPIC_BASE_URL=http://127.0.0.1:{port} in your shell profile, then start Claude Code"
    ));
    Ok(())
}

/// Poll `ready` for up to three seconds.
fn wait_for(ready: impl Fn() -> bool) -> bool {
    (0..30).any(|_| {
        if ready() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        false
    })
}

/// `gobstopper proxy uninstall`: stop the agent and remove its file.
pub fn uninstall() -> Result<()> {
    let path = plist_path()?;
    let loaded = launchctl()
        .args(["bootout", &format!("{}/{LABEL}", domain())])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path).with_context(|| format!("can't remove {}", path.display()))?;
    }
    let style = Style::stdout();
    if existed || loaded {
        println!(
            "{} Removed the proxy LaunchAgent. The proxy no longer starts at login.",
            style.symbol(Symbol::Ok)
        );
    } else {
        println!("The proxy LaunchAgent wasn't installed.");
    }
    Ok(())
}

/// Whether a LaunchAgent file for the proxy exists.
pub fn installed() -> bool {
    plist_path().map(|path| path.exists()).unwrap_or(false)
}

/// The command that restarts an installed agent.
pub fn restart_command() -> String {
    format!("launchctl kickstart -k {}/{LABEL}", domain())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_runs_serve_with_the_given_settings_and_logs_to_a_file() {
        let text = plist(
            Path::new("/opt/gob & co/gobstopper"),
            &["--threshold".into(), "256000".into()],
            Path::new("/Users/me/Library/Logs/gobstopper-proxy.log"),
        );
        assert!(text.contains("<key>Label</key><string>sh.gobstopper.proxy</string>"));
        assert!(text.contains(
            "    <string>/opt/gob &amp; co/gobstopper</string>\n    <string>proxy</string>\n    <string>serve</string>\n    <string>--threshold</string>\n    <string>256000</string>\n"
        ));
        assert!(text.contains(
            "<key>StandardErrorPath</key><string>/Users/me/Library/Logs/gobstopper-proxy.log</string>"
        ));
        assert!(!text.contains("/dev/null"));
    }

    #[test]
    fn notice_follows_the_login_item_template() {
        assert_eq!(
            login_item_notice(8260, Style::PLAIN),
            "🔐 macOS will show a notice that gobstopper can open at login.\n   It keeps the request proxy on 127.0.0.1:8260 running so Claude Code and Codex requests stay small. Turn it off any time in System Settings › General › Login Items & Extensions.\n"
        );
        assert!(login_item_notice(8260, Style::ASCII).starts_with("NOTE macOS will show"));
    }
}

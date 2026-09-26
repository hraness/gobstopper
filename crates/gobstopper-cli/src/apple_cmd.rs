//! `gobstopper apple status|install`, and the words gobstopper prints when
//! the Apple scorer or state-card writer can't use Apple's on-device model.
//!
//! The reason copy comes from `apple_foundation::Reason::explain`, so every
//! Hraness product says the same thing and links the same System Settings
//! pane. gobstopper adds its own fallback ("so gobstopper is using the
//! built-in scorer") and its own install step.
//!
//! Nothing here opens a system dialog: `install` asks
//! `apple_foundation::build_tools_check` (which only runs
//! `xcode-select -p`) before it compiles, and the compiler's output is
//! captured, never printed.

use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use apple_foundation::{Availability, Error as AppleError, Reason, ToolsProblem};
use clap::Subcommand;

#[derive(Subcommand, Debug, Clone)]
pub enum AppleCmd {
    /// Say whether Apple's on-device model is ready, and what to do if not.
    ///
    /// Exits 0 when it is ready and 1 when it isn't.
    Status {
        /// Print machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Build the small helper gobstopper uses to talk to Apple's on-device
    /// model (about 10 seconds, once).
    ///
    /// Needs macOS 26 on Apple silicon and Xcode 26 or Apple's command line
    /// tools. It checks for the tools first and never opens the macOS
    /// install dialog. The helper goes to
    /// ~/.local/share/gobstopper/apple-bridge, or to GOBSTOPPER_APPLE_BRIDGE
    /// when that is set.
    Install {
        /// Rebuild even when the helper is already current.
        #[arg(long)]
        force: bool,
        /// Print machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// Which gobstopper feature fell back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Feature {
    Scorer,
    Digest,
}

impl Feature {
    fn fallback(self) -> &'static str {
        match self {
            Self::Scorer => "gobstopper is using the built-in scorer",
            Self::Digest => "gobstopper is writing the built-in state card",
        }
    }

    fn env(self) -> &'static str {
        match self {
            Self::Scorer => "GOBSTOPPER_SCORER",
            Self::Digest => "GOBSTOPPER_DIGEST",
        }
    }
}

/// What to tell the person about one reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Advice {
    /// What is wrong, as the shared copy says it.
    pub summary: String,
    /// The one thing to do about it.
    pub fix: String,
    /// One command that does it, when there is one.
    pub next: Option<String>,
    pub settings_url: Option<&'static str>,
}

const INSTALL_FIX: &str =
    "Install it with gobstopper apple install. It needs Xcode 26 or Apple's command line tools.";
const INSTALL_NEXT: &str = "gobstopper apple install";
const READY_NEXT: &str = "GOBSTOPPER_SCORER=apple gobstopper plan <session>";

fn advice(reason: Reason, feature: Option<Feature>) -> Advice {
    let explained = reason.explain();
    let open = explained.settings_url.map(|url| format!("open {url}"));
    let (fix, next) = match reason {
        Reason::HelperMissing => (INSTALL_FIX.to_owned(), Some(INSTALL_NEXT.to_owned())),
        Reason::DeviceNotEligible => match feature {
            Some(feature) => (
                format!("Unset {} to stop seeing this.", feature.env()),
                Some(format!("unset {}", feature.env())),
            ),
            None => (
                "gobstopper works without it: leave GOBSTOPPER_SCORER and GOBSTOPPER_DIGEST unset."
                    .to_owned(),
                None,
            ),
        },
        _ => (explained.fix, open),
    };
    Advice {
        summary: explained.summary,
        fix,
        next,
        settings_url: explained.settings_url,
    }
}

// TODO(df-0.8): use hraness_cli_kit's audience and style helpers once
// desktop-foundation 0.8.0 ships them. These copy the SPEC § C and § D6
// rules verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Audience {
    Human,
    Agent,
    Quiet,
}

const AGENT_MARKERS: [&str; 6] = [
    "AI_AGENT",
    "CLAUDECODE",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CURSOR_AGENT",
    "GEMINI_CLI",
];

fn detect_audience(env: &dyn Fn(&str) -> Option<String>, stderr_tty: bool) -> Audience {
    match env("HRANESS_AUDIENCE").as_deref() {
        Some("human") => return Audience::Human,
        Some("agent") => return Audience::Agent,
        Some("quiet" | "off") => return Audience::Quiet,
        _ => {}
    }
    if AGENT_MARKERS
        .iter()
        .any(|key| env(key).is_some_and(|v| !v.is_empty()))
    {
        return Audience::Agent;
    }
    if stderr_tty {
        Audience::Human
    } else {
        Audience::Quiet
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Symbol {
    Ok,
    Fail,
    Warn,
    Next,
    Progress,
}

/// How one stream renders symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Style {
    pub color: bool,
    pub ascii: bool,
}

impl Style {
    #[cfg(test)]
    fn plain() -> Self {
        Self {
            color: false,
            ascii: false,
        }
    }

    fn detect(env: &dyn Fn(&str) -> Option<String>, is_tty: bool) -> Self {
        let term_dumb = env("TERM").as_deref() == Some("dumb");
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|key| {
            env(key).is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("utf-8") || v.contains("utf8")
            })
        });
        let ascii = term_dumb || !utf8 || env("HRANESS_ASCII").as_deref() == Some("1");
        let no_color = env("NO_COLOR").is_some_and(|v| !v.is_empty());
        let force = env("FORCE_COLOR").as_deref() == Some("1");
        let color = force || (is_tty && !term_dumb && !no_color);
        Self { color, ascii }
    }

    fn stderr() -> Self {
        Self::detect(&env_var, std::io::stderr().is_terminal())
    }

    fn stdout() -> Self {
        Self::detect(&env_var, std::io::stdout().is_terminal())
    }

    fn symbol(self, symbol: Symbol) -> String {
        let (glyph, ascii, color) = match symbol {
            Symbol::Ok => ("✓", "OK", "32"),
            Symbol::Fail => ("✗", "FAIL", "31"),
            Symbol::Warn => ("⚠", "WARN", "33"),
            Symbol::Next => ("→", "->", "2"),
            Symbol::Progress => ("↻", "...", ""),
        };
        let text = if self.ascii { ascii } else { glyph };
        if self.color && !color.is_empty() {
            format!("\x1b[{color}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Lines for a problem: `symbol headline`, the rest of the summary and the
/// fix indented, then `→ next` unless the audience is quiet.
fn problem_lines(
    style: Style,
    symbol: Symbol,
    headline: &str,
    detail: &[&str],
    next: Option<&str>,
    audience: Audience,
) -> String {
    let mut out = format!("{} {headline}\n", style.symbol(symbol));
    for line in detail.iter().filter(|l| !l.is_empty()) {
        out.push_str(&format!("  {line}\n"));
    }
    if let Some(next) = next.filter(|_| audience != Audience::Quiet) {
        out.push_str(&format!("{} {next}\n", style.symbol(Symbol::Next)));
    }
    out
}

/// Split the first sentence off `summary`: "A. B." → ("A", "B.").
fn first_sentence(summary: &str) -> (&str, &str) {
    match summary.find(". ") {
        Some(end) => (&summary[..end], summary[end + 2..].trim()),
        None => (summary.trim_end_matches('.'), ""),
    }
}

/// The notice printed once when `feature` falls back because Apple's model
/// can't be used, e.g.
///
/// ```text
/// ⚠ Apple Intelligence is off, so gobstopper is using the built-in scorer.
///   Turn it on in System Settings › Apple Intelligence & Siri, then try again.
/// → open x-apple.systempreferences:com.apple.Siri-Settings.extension
/// ```
fn fallback_notice(feature: Feature, reason: Reason, style: Style, audience: Audience) -> String {
    let advice = advice(reason, Some(feature));
    let (head, rest) = first_sentence(&advice.summary);
    let headline = format!("{head}, so {}.", feature.fallback());
    problem_lines(
        style,
        Symbol::Warn,
        &headline,
        &[rest, &advice.fix],
        advice.next.as_deref(),
        audience,
    )
}

/// The notice for a feature turned on off macOS, where there is no model.
fn unsupported_notice(feature: Feature, style: Style, audience: Audience) -> String {
    problem_lines(
        style,
        Symbol::Warn,
        &format!(
            "Apple's on-device model only runs on macOS, so {}.",
            feature.fallback()
        ),
        &[&format!("Unset {} to stop seeing this.", feature.env())],
        Some(&format!("unset {}", feature.env())),
        audience,
    )
}

/// Print to stderr at most once per feature per process (`watch` resolves
/// the scorer every pass).
fn warn_once(feature: Feature, render: impl FnOnce(Style, Audience) -> String) {
    use std::sync::Once;
    static SCORER: Once = Once::new();
    static DIGEST: Once = Once::new();
    let once = match feature {
        Feature::Scorer => &SCORER,
        Feature::Digest => &DIGEST,
    };
    once.call_once(|| {
        let audience = detect_audience(&env_var, std::io::stderr().is_terminal());
        let _ = std::io::stderr().write_all(render(Style::stderr(), audience).as_bytes());
    });
}

/// Say once why `feature` fell back, with the fix.
pub(crate) fn warn_fallback(feature: Feature, reason: Reason) {
    warn_once(feature, |style, audience| {
        fallback_notice(feature, reason, style, audience)
    });
}

/// Say once that `feature` can't work on this operating system.
pub(crate) fn warn_unsupported(feature: Feature) {
    warn_once(feature, |style, audience| {
        unsupported_notice(feature, style, audience)
    });
}

/// A request failed after the model looked ready. Model-level reasons get
/// the full notice (once); anything else gets one line per failure.
pub(crate) fn warn_request_failed(feature: Feature, error: &anyhow::Error) {
    if let Some(reason) = crate::apple::unavailable_reason(error) {
        warn_fallback(feature, reason);
        return;
    }
    let _ = std::io::stderr()
        .write_all(request_failed_line(feature, error, Style::stderr()).as_bytes());
}

fn request_failed_line(feature: Feature, error: &anyhow::Error, style: Style) -> String {
    let why = match error.to_string().as_str() {
        "inference_deadline" => {
            "it didn't answer in time (raise GOBSTOPPER_APPLE_TIMEOUT_MS to wait longer)"
        }
        "bridge_identity_changed" | "bridge_unavailable" => {
            "the helper changed or moved while gobstopper was running"
        }
        "inference_process_failed" => "the helper stopped before answering",
        _ => "its answer couldn't be used",
    };
    let kept = match feature {
        Feature::Scorer => "kept the built-in scores for that batch",
        Feature::Digest => "kept the built-in state card",
    };
    format!(
        "{} Apple's model didn't help this time: {why}. gobstopper {kept}.\n",
        style.symbol(Symbol::Warn)
    )
}

/// The seams `status` and `install` need, so tests never start a compiler,
/// a real bridge, or the developer tools dialog.
pub(crate) trait Host {
    fn resolve(&self) -> Option<PathBuf>;
    fn default_target(&self) -> Option<PathBuf>;
    fn platform(&self) -> apple_foundation::Result<()>;
    fn tools(&self) -> apple_foundation::Result<()>;
    fn is_current(&self, path: &Path) -> bool;
    fn build(&self, path: &Path) -> apple_foundation::Result<PathBuf>;
    fn check(&self, path: &Path) -> Availability;
    fn write_log(&self, path: &Path, text: &str) -> std::io::Result<()>;
}

struct SystemHost;

impl Host for SystemHost {
    fn resolve(&self) -> Option<PathBuf> {
        crate::apple::resolve_bridge()
    }
    fn default_target(&self) -> Option<PathBuf> {
        match std::env::var_os("GOBSTOPPER_APPLE_BRIDGE").filter(|p| !p.is_empty()) {
            Some(path) => Some(PathBuf::from(path)),
            None => crate::apple::default_install_path(),
        }
    }
    fn platform(&self) -> apple_foundation::Result<()> {
        apple_foundation::platform_check()
    }
    fn tools(&self) -> apple_foundation::Result<()> {
        apple_foundation::build_tools_check()
    }
    fn is_current(&self, path: &Path) -> bool {
        apple_foundation::bridge_is_current(path)
    }
    fn build(&self, path: &Path) -> apple_foundation::Result<PathBuf> {
        apple_foundation::ensure_bridge(path)
    }
    fn check(&self, path: &Path) -> Availability {
        crate::apple::check(path)
    }
    fn write_log(&self, path: &Path, text: &str) -> std::io::Result<()> {
        std::fs::write(path, text)
    }
}

/// Rendered output: `stdout` is the result, `stderr` holds progress and
/// hints, `code` is the exit status.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Render {
    pub json: bool,
    pub audience: Audience,
    pub out: Style,
    pub err: Style,
}

pub(crate) fn run(command: &AppleCmd) -> anyhow::Result<()> {
    let audience = detect_audience(&env_var, std::io::stderr().is_terminal());
    let render = |json: bool| Render {
        json: json || audience == Audience::Agent,
        audience,
        out: Style::stdout(),
        err: Style::stderr(),
    };
    let output = match command {
        AppleCmd::Status { json } => status(&SystemHost, render(*json)),
        AppleCmd::Install { force, json } => {
            let r = render(*json);
            install(&SystemHost, *force, r, &mut |line| {
                // Progress appears before the build starts, not after.
                let _ = std::io::stderr().write_all(line.as_bytes());
                let _ = std::io::stderr().flush();
            })
        }
    };
    // A closed pipe (`| head -1`) is not an error worth a panic.
    let _ = std::io::stdout().write_all(output.stdout.as_bytes());
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().write_all(output.stderr.as_bytes());
    if output.code != 0 {
        std::process::exit(output.code);
    }
    Ok(())
}

fn display(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        if let Ok(rest) = path.strip_prefix(PathBuf::from(home)) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

fn json_line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

fn not_mac(render: Render) -> Output {
    failure(
        render,
        "unsupported",
        "Apple's on-device model only runs on macOS.",
        &[],
        None,
    )
}

fn status(host: &dyn Host, render: Render) -> Output {
    let (helper, availability) = match host.resolve() {
        Some(path) => {
            let availability = host.check(&path);
            (Some(path), availability)
        }
        None => match host.platform() {
            Err(AppleError::Unsupported(_)) => return not_mac(render),
            Err(AppleError::Unavailable(reason)) => (None, Availability::unavailable(reason)),
            _ => (None, Availability::unavailable(Reason::HelperMissing)),
        },
    };
    let reason = availability.reason.unwrap_or(Reason::Unavailable);
    let advice = (!availability.available).then(|| advice(reason, None));
    let code = i32::from(!availability.available);
    if render.json {
        let next = match &advice {
            Some(a) => a.next.clone(),
            None => Some(READY_NEXT.to_owned()),
        };
        return Output {
            stdout: json_line(serde_json::json!({
                "available": availability.available,
                "reason": availability.reason.map(Reason::as_str),
                "helper": helper.as_ref().map(|p| p.display().to_string()),
                "message": advice.as_ref().map(|a| a.summary.clone()),
                "fix": advice.as_ref().map(|a| a.fix.clone()),
                "settingsUrl": advice.as_ref().and_then(|a| a.settings_url),
                "next": next,
            })),
            code,
            ..Output::default()
        };
    }
    let helper_line = helper
        .as_ref()
        .map(|p| format!("Helper: {}", display(p)))
        .unwrap_or_default();
    match advice {
        None => Output {
            stdout: format!(
                "{} Apple's on-device model is ready.\n  {helper_line}\n",
                render.out.symbol(Symbol::Ok)
            ),
            stderr: if render.audience == Audience::Human {
                format!("Next: {READY_NEXT}\n")
            } else {
                String::new()
            },
            code,
        },
        Some(advice) => Output {
            stdout: problem_lines(
                render.out,
                Symbol::Warn,
                &advice.summary,
                &[&advice.fix, &helper_line],
                advice.next.as_deref(),
                render.audience,
            ),
            code,
            ..Output::default()
        },
    }
}

fn failure(
    render: Render,
    code: &str,
    headline: &str,
    detail: &[&str],
    next: Option<&str>,
) -> Output {
    if render.json {
        let message = std::iter::once(headline)
            .chain(detail.iter().copied().filter(|d| !d.is_empty()))
            .collect::<Vec<_>>()
            .join(" ");
        return Output {
            stdout: json_line(serde_json::json!({
                "ok": false,
                "error": {"code": code, "message": message, "next": next},
            })),
            code: 1,
            ..Output::default()
        };
    }
    Output {
        stderr: problem_lines(
            render.err,
            Symbol::Fail,
            headline,
            detail,
            next,
            render.audience,
        ),
        code: 1,
        ..Output::default()
    }
}

fn tools_failure(render: Render, problem: &ToolsProblem) -> Output {
    if *problem == ToolsProblem::NotInstalled {
        // SPEC Appendix B, "missing, developer-tools".
        return failure(
            render,
            "developer-tools-missing",
            "gobstopper needs Apple's command line tools. Nothing was installed.",
            &[],
            Some("xcode-select --install"),
        );
    }
    let explained = problem.explain();
    let next = explained
        .command
        .map(str::to_owned)
        .or_else(|| explained.settings_url.map(|url| format!("open {url}")));
    failure(
        render,
        "developer-tools-missing",
        &explained.summary,
        &[&explained.fix],
        next.as_deref(),
    )
}

fn install(host: &dyn Host, force: bool, render: Render, progress: &mut dyn FnMut(&str)) -> Output {
    let Some(target) = host.default_target() else {
        return failure(
            render,
            "no-home",
            "gobstopper can't tell where to put the helper: HOME isn't set.",
            &[],
            Some("GOBSTOPPER_APPLE_BRIDGE=/path/to/apple-bridge gobstopper apple install"),
        );
    };
    match host.platform() {
        Ok(()) => {}
        Err(AppleError::Unsupported(_)) => return not_mac(render),
        Err(AppleError::Unavailable(reason)) => {
            let advice = advice(reason, None);
            return failure(
                render,
                reason.as_str(),
                &advice.summary,
                &[&advice.fix, "Nothing was installed."],
                advice.next.as_deref(),
            );
        }
        Err(other) => {
            return failure(
                render,
                "install-failed",
                &format!("gobstopper couldn't check this Mac: {other}."),
                &[],
                None,
            )
        }
    }
    let already = !force && host.is_current(&target);
    if !already {
        match host.tools() {
            Ok(()) => {}
            Err(AppleError::ToolsMissing(problem)) => return tools_failure(render, &problem),
            Err(AppleError::Unsupported(_)) => return not_mac(render),
            Err(other) => {
                return failure(
                    render,
                    "install-failed",
                    &format!("gobstopper couldn't find Apple's developer tools: {other}."),
                    &[],
                    None,
                )
            }
        }
        if force {
            // `ensure_bridge` returns at once for a current helper; remove
            // the stamp so it rebuilds.
            let _ = std::fs::remove_file(target.with_extension("stamp"));
        }
        if render.audience == Audience::Human && !render.json {
            progress(&format!(
                "{} Building the Apple model helper (one time, about 10 seconds)…\n",
                render.err.symbol(Symbol::Progress)
            ));
        }
        match host.build(&target) {
            Ok(_) => {}
            Err(AppleError::BuildFailed { status, log_tail }) => {
                let log = target.with_file_name("apple-bridge-build.log");
                let saved = host.write_log(&log, &format!("{log_tail}\n")).is_ok();
                let how = match status {
                    Some(code) => format!("swiftc exited with status {code}"),
                    None => "swiftc was stopped".to_owned(),
                };
                let log_line = if saved {
                    format!("Compiler output: {}", display(&log))
                } else {
                    String::new()
                };
                return failure(
                    render,
                    "build-failed",
                    &format!("The Apple model helper didn't build: {how}."),
                    &[
                        "Check that Xcode 26 or later is selected (xcode-select -p), then try again.",
                        &log_line,
                    ],
                    Some(INSTALL_NEXT),
                );
            }
            Err(AppleError::ToolsMissing(problem)) => return tools_failure(render, &problem),
            Err(AppleError::Unavailable(reason)) => {
                let advice = advice(reason, None);
                return failure(
                    render,
                    reason.as_str(),
                    &advice.summary,
                    &[&advice.fix],
                    advice.next.as_deref(),
                );
            }
            Err(other) => {
                return failure(
                    render,
                    "install-failed",
                    &format!("gobstopper couldn't install the helper: {other}."),
                    &[],
                    Some(INSTALL_NEXT),
                )
            }
        }
    }
    let availability = host.check(&target);
    let advice = (!availability.available)
        .then(|| advice(availability.reason.unwrap_or(Reason::Unavailable), None));
    if render.json {
        return Output {
            stdout: json_line(serde_json::json!({
                "ok": true,
                "helper": target.display().to_string(),
                "built": !already,
                "available": availability.available,
                "reason": availability.reason.map(Reason::as_str),
                "next": match &advice {
                    Some(a) => a.next.clone(),
                    None => Some(READY_NEXT.to_owned()),
                },
            })),
            ..Output::default()
        };
    }
    let done = if already {
        format!(
            "{} The Apple model helper is already installed at {}.\n",
            render.out.symbol(Symbol::Ok),
            display(&target)
        )
    } else {
        format!(
            "{} Installed the Apple model helper at {}.\n",
            render.out.symbol(Symbol::Ok),
            display(&target)
        )
    };
    let stderr = match (&advice, render.audience) {
        (Some(advice), audience) => problem_lines(
            render.err,
            Symbol::Warn,
            &advice.summary,
            &[&advice.fix],
            advice.next.as_deref(),
            audience,
        ),
        (None, Audience::Human) => format!("Next: {READY_NEXT}\n"),
        (None, _) => String::new(),
    };
    Output {
        stdout: done,
        stderr,
        code: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    /// Compare with `tests/golden/<name>`; `GOBSTOPPER_BLESS=1` rewrites it.
    fn assert_golden(name: &str, rendered: &str) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(name);
        if std::env::var_os("GOBSTOPPER_BLESS").is_some() {
            std::fs::write(&path, rendered).unwrap();
        }
        let golden = std::fs::read_to_string(&path).unwrap();
        assert_eq!(rendered, golden, "\n--- rendered ---\n{rendered}");
    }

    fn human() -> Render {
        Render {
            json: false,
            audience: Audience::Human,
            out: Style::plain(),
            err: Style::plain(),
        }
    }

    fn quiet() -> Render {
        Render {
            audience: Audience::Quiet,
            ..human()
        }
    }

    fn json() -> Render {
        Render {
            json: true,
            ..human()
        }
    }

    fn reason_name(reason: Reason) -> &'static str {
        reason.as_str()
    }

    #[test]
    fn fallback_notices_match_golden() {
        let mut rendered = String::new();
        for feature in [Feature::Scorer, Feature::Digest] {
            for reason in Reason::ALL {
                rendered.push_str(&format!("[{feature:?} {}]\n", reason_name(reason)));
                rendered.push_str(&fallback_notice(
                    feature,
                    reason,
                    Style::plain(),
                    Audience::Human,
                ));
            }
        }
        for feature in [Feature::Scorer, Feature::Digest] {
            rendered.push_str(&format!("[{feature:?} not macOS]\n"));
            rendered.push_str(&unsupported_notice(
                feature,
                Style::plain(),
                Audience::Human,
            ));
        }
        assert_golden("apple_fallback.txt", &rendered);
    }

    #[test]
    fn quiet_audience_drops_next_and_ascii_replaces_symbols() {
        let notice = fallback_notice(
            Feature::Scorer,
            Reason::AppleIntelligenceNotEnabled,
            Style {
                color: false,
                ascii: true,
            },
            Audience::Quiet,
        );
        assert_eq!(
            notice,
            "WARN Apple Intelligence is off, so gobstopper is using the built-in scorer.\n  Turn it on in System Settings › Apple Intelligence & Siri, then try again.\n"
        );
    }

    #[test]
    fn style_follows_the_cli_contract() {
        let utf8 = env_of(&[("LANG", "en_US.UTF-8")]);
        assert_eq!(
            Style::detect(&utf8, true),
            Style {
                color: true,
                ascii: false
            }
        );
        assert_eq!(
            Style::detect(&utf8, false),
            Style {
                color: false,
                ascii: false
            }
        );
        let no_color = env_of(&[("LANG", "en_US.UTF-8"), ("NO_COLOR", "1")]);
        assert_eq!(
            Style::detect(&no_color, true),
            Style {
                color: false,
                ascii: false
            }
        );
        let dumb = env_of(&[("LANG", "en_US.UTF-8"), ("TERM", "dumb")]);
        assert_eq!(
            Style::detect(&dumb, true),
            Style {
                color: false,
                ascii: true
            }
        );
        let c_locale = env_of(&[("LANG", "C")]);
        assert!(Style::detect(&c_locale, false).ascii);
        let forced = env_of(&[("LANG", "en_US.UTF-8"), ("FORCE_COLOR", "1")]);
        assert!(Style::detect(&forced, false).color);
        let colored = Style {
            color: true,
            ascii: false,
        };
        assert_eq!(colored.symbol(Symbol::Warn), "\x1b[33m⚠\x1b[0m");
        assert_eq!(colored.symbol(Symbol::Progress), "↻");
    }

    #[test]
    fn audience_follows_the_shared_rule() {
        assert_eq!(detect_audience(&env_of(&[]), true), Audience::Human);
        assert_eq!(detect_audience(&env_of(&[]), false), Audience::Quiet);
        assert_eq!(
            detect_audience(&env_of(&[("CLAUDECODE", "1")]), true),
            Audience::Agent
        );
        assert_eq!(
            detect_audience(&env_of(&[("CLAUDECODE", "")]), true),
            Audience::Human
        );
        assert_eq!(
            detect_audience(&env_of(&[("CODEX_HOME", "/x")]), false),
            Audience::Quiet
        );
        assert_eq!(
            detect_audience(
                &env_of(&[("HRANESS_AUDIENCE", "off"), ("CLAUDECODE", "1")]),
                true
            ),
            Audience::Quiet
        );
    }

    #[test]
    fn request_failures_say_what_happened_without_codes() {
        let line = request_failed_line(
            Feature::Scorer,
            &anyhow::anyhow!("inference_deadline"),
            Style::plain(),
        );
        assert_eq!(
            line,
            "⚠ Apple's model didn't help this time: it didn't answer in time (raise GOBSTOPPER_APPLE_TIMEOUT_MS to wait longer). gobstopper kept the built-in scores for that batch.\n"
        );
        let line = request_failed_line(
            Feature::Digest,
            &anyhow::anyhow!("inference_response_invalid"),
            Style::plain(),
        );
        assert_eq!(
            line,
            "⚠ Apple's model didn't help this time: its answer couldn't be used. gobstopper kept the built-in state card.\n"
        );
        assert!(!line.contains('_'));
    }

    struct FakeHost {
        resolved: Option<PathBuf>,
        target: Option<PathBuf>,
        platform: fn() -> apple_foundation::Result<()>,
        tools: fn() -> apple_foundation::Result<()>,
        current: bool,
        build: fn() -> apple_foundation::Result<()>,
        availability: Availability,
        built: Cell<bool>,
        log: RefCell<Option<(PathBuf, String)>>,
    }

    impl Default for FakeHost {
        fn default() -> Self {
            Self {
                resolved: None,
                target: Some(PathBuf::from("/fake/share/gobstopper/apple-bridge")),
                platform: || Ok(()),
                tools: || Ok(()),
                current: false,
                build: || Ok(()),
                availability: Availability::ready(),
                built: Cell::new(false),
                log: RefCell::new(None),
            }
        }
    }

    impl Host for FakeHost {
        fn resolve(&self) -> Option<PathBuf> {
            self.resolved.clone()
        }
        fn default_target(&self) -> Option<PathBuf> {
            self.target.clone()
        }
        fn platform(&self) -> apple_foundation::Result<()> {
            (self.platform)()
        }
        fn tools(&self) -> apple_foundation::Result<()> {
            (self.tools)()
        }
        fn is_current(&self, _: &Path) -> bool {
            self.current
        }
        fn build(&self, path: &Path) -> apple_foundation::Result<PathBuf> {
            self.built.set(true);
            (self.build)().map(|()| path.to_path_buf())
        }
        fn check(&self, _: &Path) -> Availability {
            self.availability.clone()
        }
        fn write_log(&self, path: &Path, text: &str) -> std::io::Result<()> {
            *self.log.borrow_mut() = Some((path.to_path_buf(), text.to_owned()));
            Ok(())
        }
    }

    fn transcript(label: &str, output: &Output) -> String {
        format!(
            "[{label}] exit {}\n--- stdout\n{}--- stderr\n{}",
            output.code, output.stdout, output.stderr
        )
    }

    fn no_progress(_: &str) {}

    #[test]
    fn status_and_install_match_golden() {
        let mut all = String::new();
        let helper = Some(PathBuf::from("/fake/share/gobstopper/apple-bridge"));

        let ready = FakeHost {
            resolved: helper.clone(),
            ..FakeHost::default()
        };
        all.push_str(&transcript("status ready", &status(&ready, human())));
        all.push_str(&transcript("status ready quiet", &status(&ready, quiet())));
        all.push_str(&transcript("status ready json", &status(&ready, json())));

        let off = FakeHost {
            resolved: helper.clone(),
            availability: Availability::unavailable(Reason::AppleIntelligenceNotEnabled),
            ..FakeHost::default()
        };
        all.push_str(&transcript("status off", &status(&off, human())));
        all.push_str(&transcript("status off json", &status(&off, json())));

        let missing = FakeHost::default();
        all.push_str(&transcript("status missing", &status(&missing, human())));

        let intel = FakeHost {
            platform: || Err(AppleError::Unavailable(Reason::DeviceNotEligible)),
            ..FakeHost::default()
        };
        all.push_str(&transcript("status intel", &status(&intel, human())));

        let linux = FakeHost {
            platform: || {
                Err(AppleError::Unsupported(
                    "Apple Foundation Models requires macOS".into(),
                ))
            },
            ..FakeHost::default()
        };
        all.push_str(&transcript("status linux", &status(&linux, human())));
        all.push_str(&transcript("status linux json", &status(&linux, json())));

        let mut seen = Vec::new();
        let fresh = FakeHost::default();
        let out = install(&fresh, false, human(), &mut |line| {
            seen.push(line.to_owned())
        });
        assert!(fresh.built.get());
        assert_eq!(
            seen,
            ["↻ Building the Apple model helper (one time, about 10 seconds)…\n"]
        );
        all.push_str(&transcript("install fresh", &out));
        all.push_str(&transcript(
            "install fresh json",
            &install(&FakeHost::default(), false, json(), &mut no_progress),
        ));

        let current = FakeHost {
            current: true,
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install current",
            &install(&current, false, human(), &mut no_progress),
        ));
        assert!(!current.built.get());

        let no_tools = FakeHost {
            tools: || Err(AppleError::ToolsMissing(ToolsProblem::NotInstalled)),
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install no tools",
            &install(&no_tools, false, human(), &mut no_progress),
        ));
        assert!(!no_tools.built.get(), "never compile without tools");
        all.push_str(&transcript(
            "install no tools json",
            &install(&no_tools, false, json(), &mut no_progress),
        ));

        let old_sdk = FakeHost {
            tools: || {
                Err(AppleError::ToolsMissing(ToolsProblem::SdkTooOld {
                    version: "15.4".into(),
                }))
            },
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install old sdk",
            &install(&old_sdk, false, human(), &mut no_progress),
        ));

        let broken = FakeHost {
            build: || {
                Err(AppleError::BuildFailed {
                    status: Some(1),
                    log_tail: "error: no such module 'FoundationModels'".into(),
                })
            },
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install build failed",
            &install(&broken, false, human(), &mut no_progress),
        ));
        let (log_path, log_text) = broken.log.borrow().clone().unwrap();
        assert_eq!(
            log_path,
            PathBuf::from("/fake/share/gobstopper/apple-bridge-build.log")
        );
        assert_eq!(log_text, "error: no such module 'FoundationModels'\n");

        let old_macos = FakeHost {
            platform: || Err(AppleError::Unavailable(Reason::RequiresMacOS26)),
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install old macos",
            &install(&old_macos, false, human(), &mut no_progress),
        ));
        assert!(!old_macos.built.get());

        let built_but_off = FakeHost {
            availability: Availability::unavailable(Reason::ModelNotReady),
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install model downloading",
            &install(&built_but_off, false, human(), &mut no_progress),
        ));

        let no_home = FakeHost {
            target: None,
            ..FakeHost::default()
        };
        all.push_str(&transcript(
            "install no home",
            &install(&no_home, false, human(), &mut no_progress),
        ));

        assert_golden("apple_cmd.txt", &all);
    }

    #[test]
    fn install_progress_is_for_people_only() {
        let mut seen = Vec::new();
        install(&FakeHost::default(), false, quiet(), &mut |line| {
            seen.push(line.to_owned())
        });
        install(&FakeHost::default(), false, json(), &mut |line| {
            seen.push(line.to_owned())
        });
        assert!(seen.is_empty());
    }

    #[test]
    fn first_sentence_splits_multi_sentence_summaries() {
        assert_eq!(first_sentence("A b. C d."), ("A b", "C d."));
        assert_eq!(first_sentence("A b."), ("A b", ""));
    }
}

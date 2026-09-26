//! The Hraness CLI style contract for gobstopper: who is reading, status
//! symbols with ASCII fallbacks, `Next:` hints, one-sentence errors, usage
//! errors, closed pipes, and the grouped root help.
//!
//! TODO(df-0.8): use detectAudience — replace `detect_audience`, `Style` and
//! the error renderer with the `hraness-cli-kit` crate from desktop-foundation
//! 0.8.0 once it ships. The rules below are copied from that contract.

use std::fmt;
use std::io::IsTerminal as _;

/// Who reads this process's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Audience {
    Human,
    Agent,
    Quiet,
}

/// Exact agent markers. Prefixes never count: `CODEX_HOME` is human
/// configuration.
const AGENT_MARKERS: [&str; 6] = [
    "AI_AGENT",
    "CLAUDECODE",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CURSOR_AGENT",
    "GEMINI_CLI",
];

pub(crate) fn detect_audience(
    env: &dyn Fn(&str) -> Option<String>,
    stderr_is_tty: bool,
) -> Audience {
    match env("HRANESS_AUDIENCE").as_deref() {
        Some("human") => return Audience::Human,
        Some("agent") => return Audience::Agent,
        Some("quiet" | "off") => return Audience::Quiet,
        _ => {}
    }
    if AGENT_MARKERS
        .iter()
        .any(|name| env(name).is_some_and(|value| !value.is_empty()))
    {
        return Audience::Agent;
    }
    if stderr_is_tty {
        Audience::Human
    } else {
        Audience::Quiet
    }
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub(crate) fn audience() -> Audience {
    detect_audience(&process_env, std::io::stderr().is_terminal())
}

/// The shared CLI symbol set.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Symbol {
    Ok,
    Fail,
    Warn,
    Next,
    On,
    Off,
    Skip,
}

/// How one stream renders symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Style {
    color: bool,
    ascii: bool,
}

impl Style {
    pub(crate) fn detect(env: &dyn Fn(&str) -> Option<String>, is_tty: bool) -> Self {
        let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        let dumb = env("TERM").as_deref() == Some("dumb");
        let color = if env("FORCE_COLOR").as_deref() == Some("1") {
            true
        } else {
            is_tty && !dumb && !set("NO_COLOR")
        };
        let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"].iter().any(|name| {
            env(name).is_some_and(|value| {
                let value = value.to_ascii_lowercase();
                value.contains("utf-8") || value.contains("utf8")
            })
        });
        let ascii = dumb || !utf8 || env("HRANESS_ASCII").as_deref() == Some("1");
        Self { color, ascii }
    }
    pub(crate) fn stdout() -> Self {
        Self::detect(&process_env, std::io::stdout().is_terminal())
    }
    pub(crate) fn stderr() -> Self {
        Self::detect(&process_env, std::io::stderr().is_terminal())
    }
    /// Whether this stream uses the ASCII fallbacks.
    pub(crate) fn is_ascii(self) -> bool {
        self.ascii
    }
    /// The symbol, colored when this stream allows it. Only the symbol is
    /// ever colored, never the sentence.
    pub(crate) fn sym(self, symbol: Symbol) -> String {
        let (glyph, ascii, color) = match symbol {
            Symbol::Ok => ("✓", "OK", Some("32")),
            Symbol::Fail => ("✗", "FAIL", Some("31")),
            Symbol::Warn => ("⚠", "WARN", Some("33")),
            Symbol::Next => ("→", "->", Some("2")),
            Symbol::On => ("●", "*", Some("32")),
            Symbol::Off => ("○", "o", None),
            Symbol::Skip => ("–", "-", Some("2")),
        };
        let text = if self.ascii { ascii } else { glyph };
        match color {
            Some(code) if self.color => format!("\x1b[{code}m{text}\x1b[0m"),
            _ => text.to_owned(),
        }
    }
}

/// An error that knows the one command to run next. The message is a full
/// sentence written for a person.
#[derive(Debug)]
pub(crate) struct Guided {
    message: String,
    /// One indented line under the sentence: what to do in Settings or at
    /// the prompt, when the next command alone doesn't say it.
    detail: Option<String>,
    next: Option<String>,
    code: &'static str,
}

impl fmt::Display for Guided {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Guided {}

/// A person-facing error with its next command.
pub(crate) fn guided(message: impl Into<String>, next: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Guided {
        message: message.into(),
        detail: None,
        next: Some(next.into()),
        code: "error",
    })
}

/// Like [`guided`], with a stable `--json` error code.
pub(crate) fn guided_code(
    code: &'static str,
    message: impl Into<String>,
    next: impl Into<String>,
) -> anyhow::Error {
    anyhow::Error::new(Guided {
        message: message.into(),
        detail: None,
        next: Some(next.into()),
        code,
    })
}

/// Like [`guided_code`], with one indented detail line.
pub(crate) fn guided_detail(
    code: &'static str,
    message: impl Into<String>,
    detail: impl Into<String>,
    next: impl Into<String>,
) -> anyhow::Error {
    anyhow::Error::new(Guided {
        message: message.into(),
        detail: Some(detail.into()),
        next: Some(next.into()),
        code,
    })
}

/// Print the one `Next:` hint for a human reader. A quiet reader (a pipe)
/// and an agent get nothing; agents read `--json` output instead.
pub(crate) fn next_hint(command: &str) {
    if audience() == Audience::Human {
        eprintln!("Next: {command}");
    }
}

/// First letter up, ending in a period; the product name and commands keep
/// their case.
fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut out = String::with_capacity(text.len() + 1);
    if text.starts_with("gobstopper ") || text.starts_with('`') || text.starts_with('-') {
        out.push_str(text);
    } else {
        let mut chars = text.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if !out.ends_with(['.', '?', '!']) {
        out.push('.');
    }
    out
}

struct Parts {
    message: String,
    detail: Option<String>,
    next: Option<String>,
    code: &'static str,
}

fn parts(error: &anyhow::Error) -> Parts {
    let guided = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<Guided>());
    let top = error.to_string();
    // `downcast_ref` also sees through `.context(...)`, so compare the
    // outermost text to keep a caller's context in front of the message.
    let message = match guided {
        Some(guided) if top == guided.message => guided.message.clone(),
        Some(guided) => format!("{}: {}", top.trim_end_matches('.'), guided.message),
        None => {
            let root = error.root_cause().to_string();
            if root == top || top.contains(&root) {
                top
            } else {
                format!("{}: {}", top.trim_end_matches('.'), root)
            }
        }
    };
    Parts {
        message: sentence(&message),
        detail: guided.and_then(|guided| guided.detail.clone()),
        next: guided.and_then(|guided| guided.next.clone()),
        code: guided.map_or("error", |guided| guided.code),
    }
}

/// The human rendering: `✗ sentence` and, when known, `→ next command`.
pub(crate) fn render_error(error: &anyhow::Error, style: Style) -> String {
    let Parts {
        message,
        detail,
        next,
        ..
    } = parts(error);
    let mut out = format!("{} {message}", style.sym(Symbol::Fail));
    if let Some(detail) = detail {
        out.push_str(&format!("\n  {detail}"));
    }
    if let Some(next) = next {
        out.push_str(&format!("\n{} {next}", style.sym(Symbol::Next)));
    }
    out
}

pub(crate) fn render_json_error(error: &anyhow::Error) -> serde_json::Value {
    let Parts {
        message,
        detail,
        next,
        code,
    } = parts(error);
    let message = match detail {
        Some(detail) => format!("{message} {detail}"),
        None => message,
    };
    serde_json::json!({"ok": false, "error": {"code": code, "message": message, "next": next}})
}

/// Report a failed command on stderr. A failed command never writes to
/// stdout, so a `--json` reader never mistakes an error for a result.
/// `--json` or an agent reader gets the error as one JSON object; everyone
/// else, and every command whose output belongs to a protocol peer (`mcp`,
/// `hook`, `watch`, `proxy serve|run`), gets the human lines.
pub(crate) fn report_error(error: &anyhow::Error, json: bool, protocol: bool) {
    if !protocol && (json || audience() == Audience::Agent) {
        eprintln!("{}", render_json_error(error));
    } else {
        eprintln!("{}", render_error(error, Style::stderr()));
    }
}

/// Write to stdout, ignoring a closed pipe: help and usage text may be
/// piped to `head` before gobstopper knows which command runs.
pub(crate) fn write_stdout(text: &str) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// `gobstopper detect | head -1` ends quietly: restore the default SIGPIPE
/// action so a closed pipe stops the process instead of panicking inside
/// `println!`. Only read-only listing commands do this; servers and
/// commands that talk to child processes keep Rust's ignored SIGPIPE, so a
/// peer that hangs up is an error they handle, not a silent exit.
pub(crate) fn restore_sigpipe() {
    #[cfg(unix)]
    // SAFETY: called once at the top of `main`, before any thread starts;
    // SIG_DFL is a valid disposition for SIGPIPE.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// The command path a usage error belongs to (`proxy serve`), read from the
/// arguments as far as they name known subcommands.
fn command_path(root: &clap::Command, args: &[String]) -> Vec<String> {
    let mut path = Vec::new();
    let mut current = root;
    for arg in args.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        match current.find_subcommand(arg) {
            Some(sub) => {
                path.push(sub.get_name().to_owned());
                current = sub;
            }
            None => break,
        }
    }
    path
}

fn quoted(value: Option<&clap::error::ContextValue>) -> Option<String> {
    use clap::error::ContextValue;
    match value? {
        ContextValue::String(text) => Some(text.clone()),
        ContextValue::Strings(list) => list.first().cloned(),
        _ => None,
    }
}

/// Handle a clap parse failure: help and version print to stdout and exit 0;
/// usage errors print one sentence and the per-command help to run, and
/// exit 2. Returns the exit code.
pub(crate) fn clap_failure(error: clap::Error, root: &clap::Command, args: &[String]) -> i32 {
    use clap::error::{ContextKind, ErrorKind};
    match error.kind() {
        ErrorKind::DisplayHelp
        | ErrorKind::DisplayVersion
        | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            // A group such as `gobstopper proxy` with no subcommand shows
            // its help as an answer, not an error.
            write_stdout(&error.render().to_string());
            return 0;
        }
        _ => {}
    }
    let path = command_path(root, args);
    let help = if path.is_empty() {
        "gobstopper --help".to_owned()
    } else {
        format!("gobstopper {} --help", path.join(" "))
    };
    let message = match error.kind() {
        ErrorKind::InvalidSubcommand => {
            let name = quoted(error.get(ContextKind::InvalidSubcommand)).unwrap_or_default();
            match quoted(error.get(ContextKind::SuggestedSubcommand)) {
                Some(suggestion) => {
                    format!("Unknown command \"{name}\". Did you mean \"{suggestion}\"?")
                }
                None => format!("Unknown command \"{name}\"."),
            }
        }
        ErrorKind::UnknownArgument => {
            let name = quoted(error.get(ContextKind::InvalidArg)).unwrap_or_default();
            match quoted(error.get(ContextKind::SuggestedArg)) {
                Some(suggestion) => {
                    format!("Unknown option \"{name}\". Did you mean \"{suggestion}\"?")
                }
                None => format!("Unknown option \"{name}\"."),
            }
        }
        ErrorKind::MissingRequiredArgument => {
            let names = match error.get(ContextKind::InvalidArg) {
                Some(clap::error::ContextValue::Strings(list)) => list.join(", "),
                other => quoted(other).unwrap_or_default(),
            };
            format!("Missing {names}.")
        }
        _ => {
            let rendered = error.render().to_string();
            let first = rendered
                .lines()
                .next()
                .unwrap_or_default()
                .trim_start_matches("error: ")
                .to_owned();
            sentence(&first)
        }
    };
    if audience() == Audience::Agent {
        eprintln!(
            "{}",
            serde_json::json!({"ok": false, "error": {"code": "usage", "message": message, "next": help}})
        );
    } else {
        let style = Style::stderr();
        eprintln!(
            "{} {message}\n{} {help}",
            style.sym(Symbol::Fail),
            style.sym(Symbol::Next)
        );
    }
    2
}

/// What bare `gobstopper` prints: at most 25 lines, stdout, exit 0.
pub(crate) fn bare_text() -> String {
    format!(
        "Gobstopper makes long coding sessions smaller.\n\
         \n\
         Start here\n\
         \x20 gobstopper detect              List your Claude Code and Codex sessions\n\
         \x20 gobstopper plan <session>      Preview a compaction; changes nothing\n\
         \x20 gobstopper proxy run -- claude Compact requests while you work\n\
         \x20 gobstopper proxy install       Start the proxy at login\n\
         \n\
         Everyday\n\
         \x20 gobstopper apply <session>     Write a smaller copy; keeps the original\n\
         \x20 gobstopper proxy status        See what the proxy has saved\n\
         \n\
         All commands: gobstopper --help · Advanced: gobstopper help advanced\n\
         gobstopper {}\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// Root `--help`: grouped, at most 60 lines. Every visible command appears
/// here or in [`ADVANCED`]; a test keeps the lists in step with the parser.
pub(crate) const ROOT_HELP: &str = "\
Gobstopper makes long coding sessions smaller.

Usage: gobstopper [options] <command>

Start here
  detect            List Claude Code and Codex sessions and their size
  plan              Preview a compaction; changes nothing
  proxy             Compact live requests: run, serve, install, status
  apply             Write a smaller copy of a session; keeps the original

Sessions and snapshots
  verify            Check a transcript for problems that would break resume
  fork              Copy a session under a new id
  undo              Restore a session from a saved snapshot
  snapshot          Save the current transcript without compacting
  history           List a session's saved snapshots
  vault             List saved snapshots
  show              Summarize one snapshot
  diff              Compare two snapshots
  recall            Search the summaries saved with snapshots
  prune             Remove old snapshots (a dry run unless --yes)
  export            Print a session's transcript
  events            Show past compactions and what they saved

Setup
  watch             Prepare compacted copies as sessions grow
  auth              Store or check the TypeSafe key for the jev scorer
  apple             Set up and check Apple's on-device model for scoring
  mcp               Let an agent inspect sessions and snapshots
  presets           List the presets in your config
  explain           Show the cost model behind the defaults
  install-hooks     Write provider hook settings to a new file to review
  uninstall-hooks   Write hook settings without Gobstopper's hooks

Options
  -h, --help        Print help; `gobstopper <command> --help` for one command
  -V, --version     Print the version
  --codex-home      Codex folder ($CODEX_HOME or ~/.codex)
  --claude-home     Claude Code folder ($CLAUDE_CONFIG_DIR or ~/.claude)
  --codex-bin       Codex CLI to use (default: codex on PATH)

Advanced and integration commands: gobstopper help advanced
";

/// `gobstopper help advanced`: commands for evaluation, integrations and
/// maintainers, hidden from root help.
pub(crate) const ADVANCED: &str = "\
Advanced and integration commands

Evaluation
  eval              Replay a session through every strategy on temporary copies
  eval-study        Compare how much of a session each strategy keeps
  bench             Compare every strategy across all sessions (CSV)
  tune              Show the trigger and floor the tuner picks for a session
  cache-edits       Print the Claude cache_edits tool ids for a session

Snapshot content
  search-snapshot   Find matching records in one snapshot
  read-snapshot     Read a page of archived text from one snapshot

Integrations
  report            JSON report of sessions and savings for AI Charts
  policy-check      Return the compaction action for numbers you supply
  plugin            Check or inspect a strategy plugin manifest
  native-operations Show recorded native compaction operations
  native-reconcile  Close an operation Codex already finished

Run `gobstopper <command> --help` for details.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
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
                &env_of(&[("HRANESS_AUDIENCE", "human"), ("AI_AGENT", "1")]),
                false
            ),
            Audience::Human
        );
        assert_eq!(
            detect_audience(&env_of(&[("HRANESS_AUDIENCE", "off")]), true),
            Audience::Quiet
        );
    }

    #[test]
    fn style_respects_no_color_term_and_locale() {
        let utf8 = Style::detect(&env_of(&[("LANG", "en_US.UTF-8")]), true);
        assert_eq!(utf8.sym(Symbol::Ok), "\x1b[32m✓\x1b[0m");
        let no_color = Style::detect(&env_of(&[("LANG", "en_US.UTF-8"), ("NO_COLOR", "1")]), true);
        assert_eq!(no_color.sym(Symbol::Fail), "✗");
        let piped = Style::detect(&env_of(&[("LANG", "en_US.UTF-8")]), false);
        assert_eq!(piped.sym(Symbol::Warn), "⚠");
        let dumb = Style::detect(&env_of(&[("LANG", "en_US.UTF-8"), ("TERM", "dumb")]), true);
        assert_eq!(dumb.sym(Symbol::Fail), "FAIL");
        assert_eq!(dumb.sym(Symbol::Next), "->");
        let c_locale = Style::detect(&env_of(&[("LANG", "C")]), false);
        assert_eq!(c_locale.sym(Symbol::On), "*");
        let forced = Style::detect(
            &env_of(&[("LANG", "en_US.UTF-8"), ("FORCE_COLOR", "1")]),
            false,
        );
        assert_eq!(forced.sym(Symbol::Off), "○");
        assert_eq!(forced.sym(Symbol::On), "\x1b[32m●\x1b[0m");
    }

    #[test]
    fn errors_render_one_sentence_and_one_next_step() {
        let plain = Style {
            color: false,
            ascii: false,
        };
        let error = guided(
            "The proxy isn't running on port 8260",
            "gobstopper proxy serve",
        );
        assert_eq!(
            render_error(&error, plain),
            "✗ The proxy isn't running on port 8260.\n→ gobstopper proxy serve"
        );
        let wrapped = error.context("checking the proxy");
        assert_eq!(
            render_error(&wrapped, plain),
            "✗ Checking the proxy: The proxy isn't running on port 8260.\n→ gobstopper proxy serve"
        );
        let denied = guided_detail(
            "keychain-denied",
            "Gobstopper can't store your TypeSafe key: the keychain request was denied",
            "Run it again and choose Always Allow when macOS asks.",
            "gobstopper auth jev",
        );
        assert_eq!(
            render_error(&denied, plain),
            "✗ Gobstopper can't store your TypeSafe key: the keychain request was denied.\n  Run it again and choose Always Allow when macOS asks.\n→ gobstopper auth jev"
        );
        let plain_error = anyhow::anyhow!("session 'x' not found");
        assert_eq!(
            render_error(&plain_error, plain),
            "✗ Session 'x' not found."
        );
        assert_eq!(
            render_json_error(&guided_code(
                "proxy-down",
                "Proxy down.",
                "gobstopper proxy serve"
            ))
            .to_string(),
            r#"{"ok":false,"error":{"code":"proxy-down","message":"Proxy down.","next":"gobstopper proxy serve"}}"#
        );
    }

    #[test]
    fn bare_and_root_help_fit_the_contract() {
        let bare = bare_text();
        assert!(bare.lines().count() <= 25);
        assert!(
            bare.lines().all(|line| line.chars().count() <= 80),
            "{bare}"
        );
        assert!(ROOT_HELP.lines().count() <= 60);
        assert!(ROOT_HELP.lines().all(|line| line.chars().count() <= 80));
        assert!(ADVANCED.lines().all(|line| line.chars().count() <= 80));
    }
}

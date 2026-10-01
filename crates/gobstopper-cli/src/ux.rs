//! The Hraness CLI style contract for gobstopper, on top of
//! `hraness-cli-kit` from desktop-foundation: who is reading, status symbols
//! with ASCII fallbacks, `Next:` hints, one-sentence errors, usage errors,
//! closed pipes, and the grouped root help.

use hraness_cli_kit::style::CliError;
pub(crate) use hraness_cli_kit::Audience;
#[cfg(test)]
use hraness_cli_kit::Style;
use std::fmt;

/// Who is reading this process's output.
pub(crate) fn audience() -> Audience {
    hraness_cli_kit::audience::detect_current()
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

/// First letter up, ending in a period; the product name, commands and
/// options keep their case.
fn sentence(text: &str) -> String {
    hraness_cli_kit::style::sentence(text, &["gobstopper ", "-"])
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

fn cli_error(error: &anyhow::Error) -> CliError {
    let Parts {
        message,
        detail,
        next,
        code,
    } = parts(error);
    let mut cli = CliError::new(code, message);
    cli.detail = detail;
    cli.next = next;
    cli
}

/// The human rendering: `✗ sentence` and, when known, `→ next command`.
#[cfg(test)]
fn render_error(error: &anyhow::Error, style: Style) -> String {
    cli_error(error).render_human(style).trim_end().to_owned()
}

/// `{"ok":false,"error":{"code","message","next"}}` on one line.
#[cfg(test)]
fn render_json_error(error: &anyhow::Error) -> String {
    cli_error(error).render_json()
}

/// Report a failed command. `--json` or an agent reader gets one
/// `{"ok":false,"error":{…}}` object on stdout, so a JSON reader always has
/// one document to parse. Everyone else, and every command whose stdout
/// belongs to a protocol peer (`mcp`, `hook`, `watch`, `proxy serve|run`),
/// gets the human lines on stderr.
pub(crate) fn report_error(error: &anyhow::Error, json: bool, protocol: bool) {
    let audience = if protocol {
        Audience::Quiet
    } else {
        audience()
    };
    cli_error(error).report(json && !protocol, audience);
}

/// Write to stdout, ignoring a closed pipe: help and usage text may be
/// piped to `head` before gobstopper knows which command runs.
pub(crate) fn write_stdout(text: &str) {
    hraness_cli_kit::style::write_stdout(text);
}

/// `gobstopper detect | head -1` ends quietly: restore the default SIGPIPE
/// action so a closed pipe stops the process instead of panicking inside
/// `println!`. Only read-only listing commands do this; servers and
/// commands that talk to child processes keep Rust's ignored SIGPIPE, so a
/// peer that hangs up is an error they handle, not a silent exit.
pub(crate) fn restore_sigpipe() {
    hraness_cli_kit::style::restore_default_sigpipe();
}

/// The command line, with help wrapped at 100 columns at every level.
pub(crate) fn command() -> clap::Command {
    hraness_cli_kit::clap::cap_help_width(<crate::Cli as clap::CommandFactory>::command(), 100)
}

/// Handle a clap parse failure: help and version print to stdout and exit 0;
/// usage errors print one sentence and the per-command help to run, and
/// exit 2, or the JSON error on stdout for `--json` and agents. `status` is
/// a `proxy` command, so a misspelled `status` suggests it.
pub(crate) fn clap_failure(error: clap::Error, root: &clap::Command, args: &[String]) -> i32 {
    let options = hraness_cli_kit::clap::UsageOptions::default()
        .cli("gobstopper")
        .alias("status", "proxy status")
        .alias("install", "proxy install");
    hraness_cli_kit::clap::exit_on_parse_error(error, root, args, &options)
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
  proxy             Compact live requests and manage startup
  context           Reserve more context for a difficult phase
  data              Inspect local sessions, tool calls, and usage
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
  update            Update Gobstopper or change automatic-update settings
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
  --no-update       Skip automatic update checks for this invocation
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

    #[test]
    fn errors_render_one_sentence_and_one_next_step() {
        let plain = Style::PLAIN;
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
            )),
            r#"{"ok":false,"error":{"code":"proxy-down","message":"Proxy down.","next":"gobstopper proxy serve"}}"#
        );
    }

    #[test]
    fn every_help_line_fits_100_columns() {
        let over = hraness_cli_kit::clap::help_lines_over(&command(), 100);
        assert!(
            over.is_empty(),
            "help lines over 100 columns:\n{}",
            over.iter()
                .map(|(path, line)| format!("{path}: {line}"))
                .collect::<Vec<_>>()
                .join("\n")
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

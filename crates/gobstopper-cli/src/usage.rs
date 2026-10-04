//! `gobstopper usage`: token use across your coding agents, from aicharts.
//!
//! aicharts keeps a daily record on this computer (`aicharts history`).
//! Gobstopper runs a fixed set of its report and scheduling commands and never
//! asks it to enroll or publish, so nothing here sends usage anywhere. The
//! proxy's own request observations (`gobstopper data`) are a separate
//! measurement and are never added to these totals.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where people get aicharts when it is missing.
pub(crate) const GET_AICHARTS: &str = "https://aicharts.io/usage";
/// The aicharts history commands Gobstopper forwards. Publishing, enrollment
/// and every other aicharts command stay out of reach.
const COMMANDS: [&str; 5] = ["report", "status", "enable", "disable", "collect"];

pub(crate) const HELP: &str = "\
Your token use across coding agents, by day, agent, provider and model. The
numbers come from aicharts, which keeps a daily record on this computer and
uploads nothing.

Usage: gobstopper usage [report] [--days N | --since DATE --until DATE]
                        [--client ID ...] [--csv]
       gobstopper usage status | enable | disable | collect

DATE is YYYY-MM-DD in UTC; a report covers at most 366 days.

Commands
  report     Token use from the record (default: the last 30 days)
  status     Whether aicharts collects, what the record holds, when it ran
  enable     Collect four times a day on this computer
  disable    Stop collecting; the record stays
  collect    Read your agents' session files into the record now

With --json, report prints the aicharts report format, which you can open at
aicharts.io/usage/details without uploading it. Agents can read the same
record through `aicharts mcp`. What the proxy itself saw, including request
timing and compaction: gobstopper data. Don't add the two together; they
count some of the same requests.

Examples
  gobstopper usage
  gobstopper usage report --days 7 --client claude
  gobstopper usage report --since 2026-09-01 --until 2026-09-30 --csv > september.csv
";

/// The aicharts arguments for `gobstopper usage ARGS`, or a sentence
/// explaining why the request is refused.
pub(crate) fn plan(args: &[String], json: bool) -> Result<Vec<String>, String> {
    let (command, rest) = match args.split_first() {
        None => ("report", &[][..]),
        Some((first, rest)) if COMMANDS.contains(&first.as_str()) => (first.as_str(), rest),
        Some((first, _)) if first.starts_with('-') => ("report", args),
        Some((first, _)) => {
            return Err(format!(
                "gobstopper usage has no \"{first}\" command. Use report, status, enable, disable or collect."
            ));
        }
    };
    let mut argv = vec!["history".to_owned(), command.to_owned()];
    argv.extend(rest.iter().cloned());
    if json && !argv.iter().any(|arg| arg == "--json") {
        argv.push("--json".to_owned());
    }
    Ok(argv)
}

fn executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// `GOBSTOPPER_AICHARTS` when it names an absolute executable, else the first
/// `aicharts` on PATH, else `~/.local/bin/aicharts`, where the installers put it.
pub(crate) fn locate(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "aicharts.exe"
    } else {
        "aicharts"
    };
    if let Some(explicit) = env("GOBSTOPPER_AICHARTS").map(PathBuf::from) {
        return (explicit.is_absolute() && executable(&explicit)).then_some(explicit);
    }
    if let Some(path) = env("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if directory.is_absolute() && executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    let local = PathBuf::from(env("HOME")?)
        .join(".local")
        .join("bin")
        .join(name);
    (local.is_absolute() && executable(&local)).then_some(local)
}

fn missing(json: bool) -> i32 {
    let message = "Usage reports come from aicharts, which isn't installed on this computer.";
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": false,
                "error": {"code": "aicharts_missing", "message": message, "next": GET_AICHARTS},
            })
        );
    } else {
        eprintln!("{message}\nGet it at {GET_AICHARTS}, then run gobstopper usage again.");
    }
    1
}

/// Runs `aicharts history ...` on Gobstopper's terminal and returns its exit
/// code.
pub(crate) fn run(args: &[String], json: bool) -> i32 {
    if matches!(args, [flag] if flag == "--help" || flag == "-h") {
        print!("{HELP}");
        return 0;
    }
    let argv = match plan(args, json) {
        Ok(argv) => argv,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    let Some(program) = locate(|name| std::env::var_os(name)) else {
        return missing(json);
    };
    // A delegated aicharts call stays quiet about its own updates and support
    // offers; Gobstopper owns this conversation.
    match Command::new(&program)
        .args(&argv)
        .env("HRANESS_SUPPORT_AUDIENCE", "off")
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => missing(json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn plain_usage_reports_and_options_go_to_the_report() {
        assert_eq!(plan(&[], false).unwrap(), args(&["history", "report"]));
        assert_eq!(
            plan(&args(&["--days", "7", "--csv"]), false).unwrap(),
            args(&["history", "report", "--days", "7", "--csv"])
        );
        assert_eq!(
            plan(&args(&["enable"]), true).unwrap(),
            args(&["history", "enable", "--json"])
        );
        assert_eq!(
            plan(&args(&["report", "--json"]), true).unwrap(),
            args(&["history", "report", "--json"])
        );
    }

    #[test]
    fn publishing_and_other_aicharts_commands_are_out_of_reach() {
        for refused in [
            "publish",
            "enroll",
            "setup",
            "sync",
            "upload",
            "autosubmit",
            "mcp",
        ] {
            assert!(plan(&args(&[refused]), false).is_err(), "{refused}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn locate_prefers_the_explicit_path_then_path_then_local_bin() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("gobstopper-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let local = root.join("home/.local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for path in [bin.join("aicharts"), local.join("aicharts")] {
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let home = root.join("home");
        let env = |path: Option<&Path>, explicit: Option<&Path>| {
            let path = path.map(|path| path.as_os_str().to_owned());
            let explicit = explicit.map(|explicit| explicit.as_os_str().to_owned());
            let home = home.as_os_str().to_owned();
            move |name: &str| match name {
                "PATH" => path.clone(),
                "GOBSTOPPER_AICHARTS" => explicit.clone(),
                "HOME" => Some(home.clone()),
                _ => None,
            }
        };
        assert_eq!(locate(env(Some(&bin), None)), Some(bin.join("aicharts")));
        assert_eq!(locate(env(None, None)), Some(local.join("aicharts")));
        assert_eq!(
            locate(env(Some(&bin), Some(&local.join("aicharts")))),
            Some(local.join("aicharts"))
        );
        assert_eq!(
            locate(env(Some(&bin), Some(Path::new("relative/aicharts")))),
            None
        );
        std::fs::set_permissions(bin.join("aicharts"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert_eq!(
            locate(env(Some(&bin), None)),
            Some(local.join("aicharts")),
            "not executable"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

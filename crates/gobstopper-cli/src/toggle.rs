//! `gobstopper on` and `gobstopper off`: one switch for the proxy service and
//! the saved client settings that route Codex and Claude Code through it.
//!
//! `off` unroutes clients first, so new sessions go straight to their
//! provider, then removes the startup service. It records the service port
//! and settings, and any Claude Code base URL it removed, so `on` restores
//! the same setup. Each edited settings file keeps a backup beside it.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const STATE_SCHEMA: u32 = 1;
const PROVIDER_LINE: &str = "model_provider = \"gobstopper\"";
const OFF_MARK: &str = "# turned off by `gobstopper off`";

/// What `off` saved so `on` can put it back.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct Paused {
    schema: u32,
    port: Option<u16>,
    serve_args: Option<Vec<String>>,
    claude_base_url: Option<String>,
}

pub(crate) fn off(
    codex_home: &Path,
    claude_home: &Path,
    allow_dependent_caller: bool,
) -> Result<()> {
    let service = crate::proxy_agent::installed_service()?;
    let mut paused = load_paused()?.unwrap_or_default();
    paused.schema = STATE_SCHEMA;
    let codex = codex_home.join("config.toml");
    let codex_text = read_text(&codex)?;
    let codex_port = codex_text.as_deref().and_then(codex_proxy_port);
    // The service is the authority; without one, trust the port Codex was
    // pointed at, so a stopped proxy on a custom port is still recognized.
    let port = service
        .as_ref()
        .map(|(port, _)| *port)
        .or(paused.port)
        .or(codex_port)
        .unwrap_or(crate::proxy::DEFAULT_PORT);
    let mut ports = vec![port];
    ports.extend(codex_port);
    ports.push(crate::proxy::DEFAULT_PORT);
    ports.sort_unstable();
    ports.dedup();
    for port in &ports {
        crate::proxy_caller::refuse_routed(*port, allow_dependent_caller)?;
    }

    // Plan every edit before writing anything, so a damaged file stops the
    // whole command with nothing changed.
    let codex_next = match &codex_text {
        Some(text) => codex_off(text)?,
        None => None,
    };
    let claude = claude_home.join("settings.json");
    let claude_text = read_text(&claude)?;
    let claude_next = match &claude_text {
        Some(text) => claude_off(text, &ports)?,
        None => None,
    };
    if let Some((_, url)) = &claude_next {
        paused.claude_base_url = Some(url.clone());
    }
    paused.port = Some(port);
    if let Some((_, serve_args)) = &service {
        paused.serve_args = Some(serve_args.clone());
    }
    // Saved first: the removed URL must survive any later failure.
    save_paused(&paused)?;

    let mut report = Vec::new();
    report.push(match (&codex_text, codex_next) {
        (Some(text), Some(next)) => {
            let backup = replace_file(&codex, text, &next, "off")?;
            row(
                "Codex",
                format!(
                    "talks to its provider directly (backup: {})",
                    backup.display()
                ),
            )
        }
        (Some(_), None) => row("Codex", "was not routed through the proxy"),
        (None, _) => row("Codex", "no config.toml; nothing to change"),
    });
    report.push(match (&claude_text, claude_next) {
        (Some(text), Some((next, _))) => {
            let backup = replace_file(&claude, text, &next, "off")?;
            row(
                "Claude Code",
                format!("talks to Anthropic directly (backup: {})", backup.display()),
            )
        }
        (Some(_), None) => row(
            "Claude Code",
            "was not routed through the proxy by settings.json",
        ),
        (None, _) => row("Claude Code", "no settings.json; nothing to change"),
    });

    match service {
        Some((port, _)) => {
            crate::proxy_agent::uninstall().context(
                "Codex and Claude Code no longer use the proxy, but its service is still installed",
            )?;
            report.push(row(
                "Proxy",
                format!("stopped; it no longer starts at login (was on port {port})"),
            ));
        }
        None => report.push(row("Proxy", "no startup service was installed")),
    }

    println!("Gobstopper is off.");
    for line in report {
        println!("{line}");
    }
    shell_hints(port, "remove");
    println!("Restart Codex and Claude Code sessions that are already open; they keep the route they started with.");
    println!("Turn it back on with `gobstopper on`.");
    Ok(())
}

pub(crate) fn on(codex_home: &Path, claude_home: &Path) -> Result<()> {
    let mut paused = load_paused()?.unwrap_or_default();
    let service = crate::proxy_agent::installed_service()?;
    let codex = codex_home.join("config.toml");
    let codex_text = read_text(&codex)?;
    let port = service
        .as_ref()
        .map(|(port, _)| *port)
        .or(paused.port)
        .or_else(|| codex_text.as_deref().and_then(codex_proxy_port))
        .unwrap_or(crate::proxy::DEFAULT_PORT);

    // Plan every edit before touching the service, so a damaged file stops
    // the whole command with nothing changed.
    let codex_plan = codex_text.as_deref().map(codex_on).transpose()?;
    let claude = claude_home.join("settings.json");
    let claude_text = read_text(&claude)?.unwrap_or_else(|| "{}\n".into());
    // A URL saved for an older port would point Claude Code at nothing.
    let claude_url = paused.claude_base_url.as_ref().map(|url| {
        if crate::proxy_caller::loopback_service_url(url, port) {
            url.clone()
        } else {
            format!("http://127.0.0.1:{port}")
        }
    });
    let claude_next = match &claude_url {
        Some(url) => claude_on(&claude_text, url)?,
        None => None,
    };

    let mut report = Vec::new();
    match service {
        Some(_) => {
            crate::proxy_agent::repair(false)?;
            report.push(row("Proxy", format!("already installed on port {port}")));
        }
        None => {
            let serve_args = paused.serve_args.clone().unwrap_or_default();
            crate::proxy_agent::install(&serve_args, port, false, false)?;
            report.push(row(
                "Proxy",
                format!("running on port {port} and starts at login"),
            ));
        }
    }

    report.push(match (&codex_text, codex_plan) {
        (Some(_), Some(CodexOn::Already)) => row("Codex", "already routed through the proxy"),
        (Some(text), Some(CodexOn::Changed(next))) => {
            let backup = replace_file(&codex, text, &next, "on")?;
            row(
                "Codex",
                format!("routed through the proxy (backup: {})", backup.display()),
            )
        }
        (_, Some(CodexOn::OtherProvider(name))) => row(
            "Codex",
            format!("uses model_provider \"{name}\"; left unchanged"),
        ),
        (_, Some(CodexOn::NoProvider)) => row(
            "Codex",
            "has no [model_providers.gobstopper] table; see docs/proxy.md#codex to add one",
        ),
        _ => row(
            "Codex",
            "no config.toml; see docs/proxy.md#codex to route it",
        ),
    });
    if let Some(warning) = read_text(&codex)?
        .as_deref()
        .and_then(|text| codex_port_warning(text, port))
    {
        report.push(row("", warning));
    }

    report.push(match (&claude_url, claude_next) {
        (Some(url), Some(next)) => {
            replace_file(&claude, &claude_text, &next, "on")?;
            row("Claude Code", format!("routed through the proxy ({url})"))
        }
        (Some(_), None) => row(
            "Claude Code",
            "settings.json already sets ANTHROPIC_BASE_URL; left unchanged",
        ),
        (None, _) => row(
            "Claude Code",
            "unchanged; route it with `gobstopper proxy launch` or ANTHROPIC_BASE_URL",
        ),
    });
    paused.schema = STATE_SCHEMA;
    paused.claude_base_url = None;
    save_paused(&paused)?;

    println!("Gobstopper is on.");
    for line in report {
        println!("{line}");
    }
    println!(
        "New Codex and Claude Code sessions use the proxy; restart open sessions to switch them."
    );
    println!("Turn it off with `gobstopper off`.");
    Ok(())
}

fn row(label: &str, text: impl AsRef<str>) -> String {
    format!("  {label:<12} {}", text.as_ref())
}

/// Shell exports win over saved settings and cannot be edited from here.
fn shell_hints(port: u16, verb: &str) {
    for key in ["ANTHROPIC_BASE_URL", "OPENAI_BASE_URL"] {
        if let Ok(value) = std::env::var(key) {
            if crate::proxy_caller::loopback_service_url(&value, port) {
                println!(
                    "Your shell exports {key}={value}; {verb} it from your shell profile too."
                );
            }
        }
    }
}

// --- Codex config.toml -----------------------------------------------------

#[derive(Debug, PartialEq)]
enum CodexOn {
    Already,
    Changed(String),
    OtherProvider(String),
    NoProvider,
}

/// The value of `model_provider` when `line` sets only that key.
fn provider_of(line: &str) -> Option<String> {
    let table: toml::Table = toml::from_str(line).ok()?;
    if table.len() != 1 {
        return None;
    }
    table.get("model_provider")?.as_str().map(str::to_owned)
}

/// For each line of `text` (split inclusively on `\n`), whether it is a
/// table header. Lines inside a multi-line string never are.
fn table_headers(text: &str) -> Vec<bool> {
    let mut open: Option<&str> = None;
    text.split_inclusive('\n')
        .map(|line| {
            let header = open.is_none() && line.trim_start().starts_with('[');
            let mut rest = line;
            loop {
                let next = match open {
                    Some(delimiter) => rest.find(delimiter).map(|at| (at, delimiter)),
                    None => ["\"\"\"", "'''"]
                        .into_iter()
                        .filter_map(|delimiter| rest.find(delimiter).map(|at| (at, delimiter)))
                        .min(),
                };
                let Some((at, delimiter)) = next else { break };
                // A comment ends the scan outside strings.
                if open.is_none() && rest[..at].contains('#') {
                    break;
                }
                open = if open.is_some() {
                    None
                } else {
                    Some(delimiter)
                };
                rest = &rest[at + delimiter.len()..];
            }
            header
        })
        .collect()
}

/// Comment out the top-level `model_provider = "gobstopper"`. `None` when
/// Codex is not routed through the proxy.
fn codex_off(text: &str) -> Result<Option<String>> {
    let parsed: toml::Table = toml::from_str(text)
        .context("~/.codex/config.toml is not valid TOML; nothing was changed")?;
    if parsed.get("model_provider").and_then(toml::Value::as_str) != Some("gobstopper") {
        return Ok(None);
    }
    let mut out = String::with_capacity(text.len() + OFF_MARK.len() + 4);
    let mut top = true;
    let mut changed = false;
    let headers = table_headers(text);
    for (line, header) in text.split_inclusive('\n').zip(headers) {
        top &= !header;
        let body = line.trim_end_matches(['\n', '\r']);
        if top && !changed && provider_of(body).as_deref() == Some("gobstopper") {
            out.push_str(&format!("# {}  {OFF_MARK}", body.trim()));
            out.push_str(&line[body.len()..]);
            changed = true;
        } else {
            out.push_str(line);
        }
    }
    let check: toml::Table = toml::from_str(&out)?;
    if !changed || check.contains_key("model_provider") {
        bail!(
            "could not find the model_provider line in ~/.codex/config.toml; nothing was changed"
        );
    }
    Ok(Some(out))
}

fn codex_on(text: &str) -> Result<CodexOn> {
    let parsed: toml::Table = toml::from_str(text)
        .context("~/.codex/config.toml is not valid TOML; nothing was changed")?;
    match parsed.get("model_provider").and_then(toml::Value::as_str) {
        Some("gobstopper") => return Ok(CodexOn::Already),
        Some(other) => return Ok(CodexOn::OtherProvider(other.to_owned())),
        None => {}
    }
    let has_table = parsed
        .get("model_providers")
        .and_then(toml::Value::as_table)
        .is_some_and(|providers| providers.contains_key("gobstopper"));
    if !has_table {
        return Ok(CodexOn::NoProvider);
    }
    let mut out = String::with_capacity(text.len() + PROVIDER_LINE.len() + 1);
    let mut top = true;
    let mut changed = false;
    let headers = table_headers(text);
    for (line, header) in text.split_inclusive('\n').zip(headers) {
        if top && header && !changed {
            // No commented line to restore: add one above the first table.
            out.push_str(PROVIDER_LINE);
            out.push('\n');
            changed = true;
        }
        top &= !header;
        let body = line.trim_end_matches(['\n', '\r']);
        let restored = body
            .trim_start()
            .strip_prefix('#')
            .filter(|_| top && !changed)
            .and_then(provider_of)
            .filter(|name| name == "gobstopper");
        if restored.is_some() {
            out.push_str(PROVIDER_LINE);
            out.push_str(&line[body.len()..]);
            changed = true;
        } else {
            out.push_str(line);
        }
    }
    if !changed {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(PROVIDER_LINE);
        out.push('\n');
    }
    let check: toml::Table = toml::from_str(&out)?;
    if check.get("model_provider").and_then(toml::Value::as_str) != Some("gobstopper") {
        bail!("could not route Codex through the proxy; ~/.codex/config.toml was not changed");
    }
    Ok(CodexOn::Changed(out))
}

fn codex_proxy_url(text: &str) -> Option<String> {
    let parsed: toml::Table = toml::from_str(text).ok()?;
    parsed
        .get("model_providers")?
        .get("gobstopper")?
        .get("base_url")?
        .as_str()
        .map(str::to_owned)
}

/// The loopback port Codex's gobstopper provider points at.
fn codex_proxy_port(text: &str) -> Option<u16> {
    let url = codex_proxy_url(text)?;
    let authority = url.split_once("://")?.1.split(['/', '?', '#']).next()?;
    let port = authority.rsplit_once(':')?.1.parse().ok()?;
    crate::proxy_caller::loopback_service_url(&url, port).then_some(port)
}

/// A warning when the gobstopper provider points somewhere other than the
/// running service.
fn codex_port_warning(text: &str, port: u16) -> Option<String> {
    let url = codex_proxy_url(text)?;
    (!crate::proxy_caller::loopback_service_url(&url, port)).then(|| {
        format!("Codex's gobstopper base_url is {url}, but the proxy listens on port {port}")
    })
}

// --- Claude Code settings.json --------------------------------------------

/// Remove `env.ANTHROPIC_BASE_URL` when it points at the proxy. Returns the
/// new text and the removed URL.
fn claude_off(text: &str, ports: &[u16]) -> Result<Option<(String, String)>> {
    let mut value: Value = serde_json::from_str(text)
        .context("~/.claude/settings.json is not valid JSON; nothing was changed")?;
    let Some(env) = value.get_mut("env").and_then(Value::as_object_mut) else {
        return Ok(None);
    };
    let Some(url) = env
        .get("ANTHROPIC_BASE_URL")
        .and_then(Value::as_str)
        .filter(|url| {
            ports
                .iter()
                .any(|port| crate::proxy_caller::loopback_service_url(url, *port))
        })
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    env.shift_remove("ANTHROPIC_BASE_URL");
    if env.is_empty() {
        value.as_object_mut().map(|root| root.shift_remove("env"));
    }
    Ok(Some((pretty(&value)?, url)))
}

/// Put back the URL `off` removed. `None` when settings already set one.
fn claude_on(text: &str, url: &str) -> Result<Option<String>> {
    let mut value: Value = serde_json::from_str(text)
        .context("~/.claude/settings.json is not valid JSON; nothing was changed")?;
    let root = value
        .as_object_mut()
        .context("~/.claude/settings.json is not a JSON object; nothing was changed")?;
    let env = root
        .entry("env")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .context("settings.json env is not an object; nothing was changed")?;
    if env.contains_key("ANTHROPIC_BASE_URL") {
        return Ok(None);
    }
    env.insert("ANTHROPIC_BASE_URL".into(), Value::String(url.into()));
    Ok(Some(pretty(&value)?))
}

fn pretty(value: &Value) -> Result<String> {
    Ok(serde_json::to_string_pretty(value)? + "\n")
}

// --- files -----------------------------------------------------------------

fn read_text(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Write `next` over `path` if it still holds `expected`, keeping the old
/// bytes beside it as `<file>.before-gobstopper-<action>`, so the copy taken
/// by `off` still holds your routed settings after `on`. Returns its path.
fn replace_file(path: &Path, expected: &str, next: &str, action: &str) -> Result<PathBuf> {
    if read_text(path)?.as_deref() != Some(expected) {
        bail!(
            "{} changed while Gobstopper was editing it; nothing was written",
            path.display()
        );
    }
    let name = path
        .file_name()
        .context("settings file has no name")?
        .to_string_lossy();
    let backup = path.with_file_name(format!("{name}.before-gobstopper-{action}"));
    let temporary = path.with_file_name(format!(".{name}.gobstopper-{}.tmp", std::process::id()));
    if path.exists() {
        fs::copy(path, &backup).with_context(|| format!("back up {}", path.display()))?;
        fs::copy(path, &temporary)?; // carries the file's permissions
    }
    let written = fs::write(&temporary, next).and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("write {}", path.display()));
    }
    Ok(backup)
}

fn paused_path() -> Result<PathBuf> {
    let root = crate::proxy_agent::service_root()?;
    Ok(root.parent().unwrap_or(&root).join("paused.json"))
}

fn load_paused() -> Result<Option<Paused>> {
    let path = paused_path()?;
    let Some(text) = read_text(&path)? else {
        return Ok(None);
    };
    let paused: Paused = serde_json::from_str(&text)
        .with_context(|| format!("{} is damaged; delete it to start fresh", path.display()))?;
    if paused.schema != STATE_SCHEMA {
        bail!("{} uses an unsupported schema", path.display());
    }
    Ok(Some(paused))
}

fn save_paused(paused: &Paused) -> Result<()> {
    let path = paused_path()?;
    fs::create_dir_all(path.parent().context("state folder")?)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, pretty(&serde_json::to_value(paused)?)?)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTED: &str = "approval_policy = \"on-request\"\nmodel = \"gpt\"\nmodel_provider = \"gobstopper\"\n\n[model_providers.gobstopper]\nname = \"gobstopper\"\nbase_url = \"http://127.0.0.1:18360/backend-api/codex\"\n";

    #[test]
    fn codex_off_then_on_round_trips() {
        let off = codex_off(ROUTED).unwrap().unwrap();
        assert!(off.contains(&format!("# model_provider = \"gobstopper\"  {OFF_MARK}\n")));
        let parsed: toml::Table = toml::from_str(&off).unwrap();
        assert!(!parsed.contains_key("model_provider"));
        assert!(parsed["model_providers"].get("gobstopper").is_some());
        assert_eq!(codex_off(&off).unwrap(), None);
        assert_eq!(codex_on(&off).unwrap(), CodexOn::Changed(ROUTED.into()));
        assert_eq!(codex_on(ROUTED).unwrap(), CodexOn::Already);
    }

    #[test]
    fn codex_on_inserts_above_first_table_and_respects_other_providers() {
        let bare = "model = \"gpt\"\n\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        let CodexOn::Changed(next) = codex_on(bare).unwrap() else {
            panic!("expected a change");
        };
        assert_eq!(next, format!("model = \"gpt\"\n\n{PROVIDER_LINE}\n[model_providers.gobstopper]\nbase_url = \"x\"\n"));
        let other = "model_provider = \"openai\"\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        assert_eq!(
            codex_on(other).unwrap(),
            CodexOn::OtherProvider("openai".into())
        );
        assert_eq!(codex_on("model = \"gpt\"\n").unwrap(), CodexOn::NoProvider);
    }

    #[test]
    fn codex_leaves_provider_lines_inside_tables_alone() {
        let text = "[profiles.fast]\nmodel_provider = \"gobstopper\"\n";
        assert_eq!(codex_off(text).unwrap(), None);
        assert!(codex_off("model_provider = [").is_err());
    }

    #[test]
    fn codex_ignores_brackets_inside_multi_line_strings() {
        let text = "notes = \"\"\"\n[not a table]\n\"\"\"\nmodel_provider = \"gobstopper\"\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        let off = codex_off(text).unwrap().unwrap();
        assert!(off.starts_with("notes = \"\"\"\n[not a table]\n\"\"\"\n# model_provider"));
        let bare = "notes = '''\n[x]''' # [y]\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        let CodexOn::Changed(next) = codex_on(bare).unwrap() else {
            panic!("expected a change");
        };
        assert!(next.contains(&format!("# [y]\n{PROVIDER_LINE}\n[model_providers")));
        assert_eq!(table_headers("a = 1\n[t]\n[[u]]\n"), [false, true, true]);
    }

    #[test]
    fn codex_port_warning_flags_a_stale_base_url() {
        assert_eq!(codex_port_warning(ROUTED, 18360), None);
        assert!(codex_port_warning(ROUTED, 8260).is_some());
        assert_eq!(codex_proxy_port(ROUTED), Some(18360));
        let remote = "[model_providers.gobstopper]\nbase_url = \"https://example.com:18360/v1\"\n";
        assert_eq!(codex_proxy_port(remote), None);
    }

    #[test]
    fn claude_off_then_on_round_trips() {
        let text = "{\n  \"env\": {\n    \"ANTHROPIC_BASE_URL\": \"http://127.0.0.1:8260\"\n  },\n  \"model\": \"opus\"\n}\n";
        let (off, url) = claude_off(text, &[8260]).unwrap().unwrap();
        assert_eq!(url, "http://127.0.0.1:8260");
        assert_eq!(off, "{\n  \"model\": \"opus\"\n}\n");
        assert_eq!(claude_off(&off, &[8260]).unwrap(), None);
        let on = claude_on(&off, &url).unwrap().unwrap();
        let value: Value = serde_json::from_str(&on).unwrap();
        assert_eq!(value["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8260");
        assert_eq!(claude_on(&on, &url).unwrap(), None);
    }

    #[test]
    fn claude_off_keeps_other_base_urls() {
        let text = "{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://gateway.example\",\"X\":\"1\"}}";
        assert_eq!(claude_off(text, &[8260]).unwrap(), None);
        let text = "{\"env\":{\"ANTHROPIC_BASE_URL\":\"http://127.0.0.1:9999\"}}";
        assert_eq!(claude_off(text, &[8260]).unwrap(), None);
    }

    #[test]
    fn replace_file_keeps_a_backup_and_refuses_a_moved_target() {
        let dir = std::env::temp_dir().join(format!("gobstopper-toggle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "a\n").unwrap();
        let backup = replace_file(&path, "a\n", "b\n", "off").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "a\n");
        assert!(replace_file(&path, "a\n", "c\n", "off").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "b\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}

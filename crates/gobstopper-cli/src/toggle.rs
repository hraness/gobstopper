//! `gobstopper on` and `gobstopper off`: one switch for the proxy service and
//! the saved client settings that route Codex and Claude Code through it.
//!
//! `off` unroutes clients first, so new sessions go straight to their
//! provider, then removes the startup service. Codex remembers the provider
//! each session started with, so `off` also points the gobstopper provider
//! itself at OpenAI; resumed sessions then skip the stopped proxy too. It records the service port
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
    let serve_args = service
        .as_ref()
        .map(|(_, args)| args.as_slice())
        .or(paused.serve_args.as_deref())
        .unwrap_or_default();
    let upstreams = Upstreams::from_serve_args(serve_args);
    let codex_next = match &codex_text {
        Some(text) => codex_off(text, &upstreams)?,
        None => None,
    };
    let codex_stale = codex_next
        .as_ref()
        .map(|next| next.text.as_str())
        .or(codex_text.as_deref())
        .and_then(codex_proxy_url)
        .filter(|url| loopback_port(url).is_some());
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
            let backup = replace_file(&codex, text, &next.text, "off")?;
            let which = match (next.unrouted, next.repointed) {
                (true, true) => "new and resumed sessions skip the proxy",
                (true, false) => "new sessions skip the proxy",
                _ => "resumed sessions that used the proxy skip it",
            };
            row("Codex", format!("{which} (backup: {})", backup.display()))
        }
        (Some(_), None) => row("Codex", "was not routed through the proxy"),
        (None, _) => row("Codex", "no config.toml; nothing to change"),
    });
    if let Some(url) = codex_stale {
        report.push(row(
            "",
            format!("Codex's gobstopper base_url is still {url}; resumed sessions that use it will fail until you change it"),
        ));
    }
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
        .or_else(|| codex_text.as_deref().and_then(marked_port))
        .unwrap_or(crate::proxy::DEFAULT_PORT);

    // Plan every edit before touching the service, so a damaged file stops
    // the whole command with nothing changed.
    let codex_plan = codex_text
        .as_deref()
        .map(|text| codex_on(text, port))
        .transpose()?;
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
        (Some(text), Some(CodexOn::OtherProvider(name, Some(next)))) => {
            let backup = replace_file(&codex, text, &next, "on")?;
            row(
                "Codex",
                format!("uses model_provider \"{name}\"; its gobstopper provider points at the proxy again (backup: {})", backup.display()),
            )
        }
        (_, Some(CodexOn::OtherProvider(name, _))) => row(
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
    /// Another provider is selected; the text restores `off`'s base_url edit.
    OtherProvider(String, Option<String>),
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

/// Where the proxy forwards Codex requests.
#[derive(Debug, PartialEq)]
struct Upstreams {
    chatgpt: String,
    openai: String,
}

impl Upstreams {
    fn from_serve_args(args: &[String]) -> Self {
        let mut upstreams = Upstreams {
            chatgpt: "https://chatgpt.com".into(),
            openai: "https://api.openai.com".into(),
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((flag, value)) => (flag, Some(value.to_owned())),
                None => (arg.as_str(), None),
            };
            let slot = match flag {
                "--chatgpt-upstream" => &mut upstreams.chatgpt,
                "--openai-upstream" => &mut upstreams.openai,
                _ => continue,
            };
            if let Some(value) = inline.or_else(|| args.next().cloned()) {
                *slot = value.trim_end_matches('/').to_owned();
            }
        }
        upstreams
    }
}

#[derive(Debug, PartialEq)]
struct CodexOff {
    text: String,
    /// The default provider no longer routes new sessions to the proxy.
    unrouted: bool,
    /// The gobstopper provider points where the proxy forwarded to.
    repointed: bool,
}

/// Comment out the top-level `model_provider = "gobstopper"` and point the
/// gobstopper provider's loopback `base_url` where the proxy forwarded it,
/// so sessions that started through the proxy work without it. `None` when
/// neither applies.
fn codex_off(text: &str, upstreams: &Upstreams) -> Result<Option<CodexOff>> {
    let parsed: toml::Table = toml::from_str(text)
        .context("~/.codex/config.toml is not valid TOML; nothing was changed")?;
    let unrouted = parsed.get("model_provider").and_then(toml::Value::as_str) == Some("gobstopper");
    let commented = if unrouted {
        comment_provider(text)?
    } else {
        text.to_owned()
    };
    let out = rewrite_base_url(&commented, |line, url| {
        let direct = direct_url(url, upstreams)?;
        (!line.contains(OFF_MARK))
            .then(|| format!("base_url = \"{direct}\"  {OFF_MARK}; was {url}"))
    });
    toml::from_str::<toml::Table>(&out)
        .context("could not edit ~/.codex/config.toml; nothing was changed")?;
    let repointed = out != commented;
    Ok((unrouted || repointed).then_some(CodexOff {
        text: out,
        unrouted,
        repointed,
    }))
}

/// The proxy port recorded in a `base_url` line that `off` rewrote.
fn marked_port(text: &str) -> Option<u16> {
    text.lines()
        .filter_map(|line| line.split_once(OFF_MARK)?.1.strip_prefix("; was "))
        .find_map(|url| loopback_port(url.trim()))
}

fn comment_provider(text: &str) -> Result<String> {
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
    Ok(out)
}

/// Replace the `base_url = "..."` line inside `[model_providers.gobstopper]`
/// with what `edit` returns for that line and its URL.
fn rewrite_base_url(text: &str, edit: impl Fn(&str, &str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len() + 96);
    let mut inside = false;
    let headers = table_headers(text);
    for (line, header) in text.split_inclusive('\n').zip(headers) {
        let body = line.trim_end_matches(['\n', '\r']);
        if header {
            inside = is_gobstopper_table(body);
        }
        let url = (inside && !header)
            .then(|| toml::from_str::<toml::Table>(body).ok())
            .flatten()
            .filter(|table| table.len() == 1)
            .and_then(|table| table.get("base_url")?.as_str().map(str::to_owned));
        match url.and_then(|url| edit(body, &url)) {
            Some(next) => {
                out.push_str(&body[..body.len() - body.trim_start().len()]);
                out.push_str(&next);
                out.push_str(&line[body.len()..]);
            }
            None => out.push_str(line),
        }
    }
    out
}

fn is_gobstopper_table(header: &str) -> bool {
    let Ok(table) = toml::from_str::<toml::Table>(header) else {
        return false;
    };
    let providers = table.get("model_providers").and_then(toml::Value::as_table);
    table.len() == 1
        && providers.is_some_and(|providers| {
            providers.len() == 1
                && providers
                    .get("gobstopper")
                    .and_then(toml::Value::as_table)
                    .is_some_and(toml::Table::is_empty)
        })
}

/// Split a URL into its authority and path.
fn url_parts(url: &str) -> Option<(&str, &str)> {
    let rest = url.split_once("://")?.1;
    let at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some((&rest[..at], &rest[at..]))
}

/// The proxy port of a loopback proxy URL.
fn loopback_port(url: &str) -> Option<u16> {
    let port = url_parts(url)?.0.rsplit_once(':')?.1.parse().ok()?;
    crate::proxy_caller::loopback_service_url(url, port).then_some(port)
}

/// The URL the proxy forwards a loopback Codex `base_url` to.
fn direct_url(url: &str, upstreams: &Upstreams) -> Option<String> {
    loopback_port(url)?;
    let path = url_parts(url)?.1;
    let upstream = if path.starts_with("/backend-api/") {
        &upstreams.chatgpt
    } else if path == "/v1" || path.starts_with("/v1/") {
        &upstreams.openai
    } else {
        return None;
    };
    Some(format!("{upstream}{path}"))
}

/// Route Codex through the proxy on `port`: restore the `base_url` that
/// `off` pointed at OpenAI, then select the gobstopper provider.
fn codex_on(text: &str, port: u16) -> Result<CodexOn> {
    let parsed: toml::Table = toml::from_str(text)
        .context("~/.codex/config.toml is not valid TOML; nothing was changed")?;
    let restored = rewrite_base_url(text, |line, url| {
        let path = url_parts(url)?.1;
        line.contains(OFF_MARK)
            .then(|| format!("base_url = \"http://127.0.0.1:{port}{path}\""))
    });
    match parsed.get("model_provider").and_then(toml::Value::as_str) {
        Some("gobstopper") | None => {}
        Some(other) => {
            let next = (restored != text).then_some(restored);
            return Ok(CodexOn::OtherProvider(other.to_owned(), next));
        }
    }
    let next = if parsed.contains_key("model_provider") {
        restored
    } else {
        let has_table = parsed
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .is_some_and(|providers| providers.contains_key("gobstopper"));
        if !has_table {
            return Ok(CodexOn::NoProvider);
        }
        select_provider(&restored)?
    };
    Ok(if next == text {
        CodexOn::Already
    } else {
        CodexOn::Changed(next)
    })
}

/// Restore the commented `model_provider` line, or add one above the first
/// table.
fn select_provider(text: &str) -> Result<String> {
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
    Ok(out)
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
    loopback_port(&codex_proxy_url(text)?)
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
    fn switch_codex_off_then_on_round_trips() {
        let off = codex_off(ROUTED, &Upstreams::from_serve_args(&[]))
            .unwrap()
            .unwrap()
            .text;
        assert!(off.contains(&format!("# model_provider = \"gobstopper\"  {OFF_MARK}\n")));
        let parsed: toml::Table = toml::from_str(&off).unwrap();
        assert!(!parsed.contains_key("model_provider"));
        assert_eq!(
            parsed["model_providers"]["gobstopper"]["base_url"].as_str(),
            Some("https://chatgpt.com/backend-api/codex")
        );
        assert_eq!(
            codex_off(&off, &Upstreams::from_serve_args(&[])).unwrap(),
            None
        );
        assert_eq!(
            codex_on(&off, 18360).unwrap(),
            CodexOn::Changed(ROUTED.into())
        );
        assert_eq!(codex_on(ROUTED, 18360).unwrap(), CodexOn::Already);
    }

    #[test]
    fn switch_codex_off_repoints_sessions_pinned_to_the_provider() {
        // Codex resumes a session with the provider it started with, so a
        // commented-out default alone leaves those sessions on the proxy.
        let pinned = ROUTED.replacen("model_provider", "# model_provider", 1);
        let off = codex_off(&pinned, &Upstreams::from_serve_args(&[]))
            .unwrap()
            .unwrap()
            .text;
        assert_eq!(
            codex_proxy_url(&off).as_deref(),
            Some("https://chatgpt.com/backend-api/codex")
        );
        assert!(off.contains(&format!(
            "{OFF_MARK}; was http://127.0.0.1:18360/backend-api/codex\n"
        )));
        assert_eq!(
            codex_off(&off, &Upstreams::from_serve_args(&[])).unwrap(),
            None
        );
        // `on` follows the proxy to a new port.
        let CodexOn::Changed(on) = codex_on(&off, 8260).unwrap() else {
            panic!("expected a change");
        };
        assert_eq!(
            codex_proxy_url(&on).as_deref(),
            Some("http://127.0.0.1:8260/backend-api/codex")
        );

        let key = "[model_providers.gobstopper]\nbase_url = \"http://localhost:8260/v1\"\n";
        let off = codex_off(key, &Upstreams::from_serve_args(&[]))
            .unwrap()
            .unwrap()
            .text;
        assert_eq!(
            codex_proxy_url(&off).as_deref(),
            Some("https://api.openai.com/v1")
        );
        // Remote, unknown-path, and other providers' URLs stay as written.
        for text in [
            "[model_providers.gobstopper]\nbase_url = \"https://gateway.example/v1\"\n",
            "[model_providers.gobstopper]\nbase_url = \"http://127.0.0.1:8260/other\"\n",
            "[model_providers.local]\nbase_url = \"http://127.0.0.1:8260/v1\"\n",
        ] {
            assert_eq!(
                codex_off(text, &Upstreams::from_serve_args(&[])).unwrap(),
                None,
                "{text}"
            );
        }
        // A base_url someone set by hand is not restored by `on`.
        let manual = "model_provider = \"gobstopper\"\n[model_providers.gobstopper]\nbase_url = \"https://chatgpt.com/backend-api/codex\"\n";
        assert_eq!(codex_on(manual, 8260).unwrap(), CodexOn::Already);
    }

    #[test]
    fn switch_codex_off_follows_custom_upstreams_and_reports_what_changed() {
        let args: Vec<String> = [
            "--port",
            "8260",
            "--openai-upstream",
            "https://gateway.example/",
            "--chatgpt-upstream=https://cg.example",
        ]
        .map(String::from)
        .into();
        let upstreams = Upstreams::from_serve_args(&args);
        assert_eq!(upstreams.openai, "https://gateway.example");
        assert_eq!(upstreams.chatgpt, "https://cg.example");
        let text = "model_provider = \"gobstopper\"\n[model_providers.gobstopper]\n  base_url = \"http://127.0.0.1:8260/v1\"\n";
        let off = codex_off(text, &upstreams).unwrap().unwrap();
        assert!(off.unrouted && off.repointed);
        assert!(off
            .text
            .contains("\n  base_url = \"https://gateway.example/v1\"  "));
        assert_eq!(marked_port(&off.text), Some(8260));
        // An unmapped path is unrouted but not repointed, and says so.
        let odd = text.replace("/v1", "/backend-api");
        let off = codex_off(&odd, &upstreams).unwrap().unwrap();
        assert!(off.unrouted && !off.repointed);
    }

    #[test]
    fn switch_codex_on_restores_base_url_under_another_default_provider() {
        let text = "model_provider = \"openai\"\n[profiles.g]\nmodel_provider = \"gobstopper\"\n[model_providers.gobstopper]\nbase_url = \"http://127.0.0.1:8260/backend-api/codex\"\n";
        let off = codex_off(text, &Upstreams::from_serve_args(&[]))
            .unwrap()
            .unwrap();
        assert!(!off.unrouted && off.repointed);
        assert_eq!(
            codex_on(&off.text, 8260).unwrap(),
            CodexOn::OtherProvider("openai".into(), Some(text.into()))
        );
    }

    #[test]
    fn switch_codex_on_inserts_above_first_table_and_respects_other_providers() {
        let bare = "model = \"gpt\"\n\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        let CodexOn::Changed(next) = codex_on(bare, 8260).unwrap() else {
            panic!("expected a change");
        };
        assert_eq!(next, format!("model = \"gpt\"\n\n{PROVIDER_LINE}\n[model_providers.gobstopper]\nbase_url = \"x\"\n"));
        let other = "model_provider = \"openai\"\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        assert_eq!(
            codex_on(other, 8260).unwrap(),
            CodexOn::OtherProvider("openai".into(), None)
        );
        assert_eq!(
            codex_on("model = \"gpt\"\n", 8260).unwrap(),
            CodexOn::NoProvider
        );
    }

    #[test]
    fn switch_codex_leaves_provider_lines_inside_tables_alone() {
        let text = "[profiles.fast]\nmodel_provider = \"gobstopper\"\n";
        assert_eq!(
            codex_off(text, &Upstreams::from_serve_args(&[])).unwrap(),
            None
        );
        assert!(codex_off("model_provider = [", &Upstreams::from_serve_args(&[])).is_err());
    }

    #[test]
    fn switch_codex_ignores_brackets_inside_multi_line_strings() {
        let text = "notes = \"\"\"\n[not a table]\n\"\"\"\nmodel_provider = \"gobstopper\"\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        let off = codex_off(text, &Upstreams::from_serve_args(&[]))
            .unwrap()
            .unwrap();
        assert!(off
            .text
            .starts_with("notes = \"\"\"\n[not a table]\n\"\"\"\n# model_provider"));
        let bare = "notes = '''\n[x]''' # [y]\n[model_providers.gobstopper]\nbase_url = \"x\"\n";
        let CodexOn::Changed(next) = codex_on(bare, 8260).unwrap() else {
            panic!("expected a change");
        };
        assert!(next.contains(&format!("# [y]\n{PROVIDER_LINE}\n[model_providers")));
        assert_eq!(table_headers("a = 1\n[t]\n[[u]]\n"), [false, true, true]);
    }

    #[test]
    fn switch_codex_port_warning_flags_a_stale_base_url() {
        assert_eq!(codex_port_warning(ROUTED, 18360), None);
        assert!(codex_port_warning(ROUTED, 8260).is_some());
        assert_eq!(codex_proxy_port(ROUTED), Some(18360));
        let remote = "[model_providers.gobstopper]\nbase_url = \"https://example.com:18360/v1\"\n";
        assert_eq!(codex_proxy_port(remote), None);
    }

    #[test]
    fn switch_claude_off_then_on_round_trips() {
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
    fn switch_claude_off_keeps_other_base_urls() {
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

//! Per-launch routing. No user configuration changes and no child retries.
use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const ANTHROPIC: &str = "https://api.anthropic.com";
const OPENAI: &str = "https://api.openai.com/v1";
const CHATGPT: &str = "https://chatgpt.com/backend-api/codex";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Client {
    Claude,
    Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CodexAuth {
    Chatgpt,
    ApiKey,
}

#[derive(Args)]
pub struct LaunchOptions {
    /// Supported client to launch. Existing authentication is retained.
    #[arg(long, value_enum)]
    client: Client,
    /// Codex endpoint dialect; this never reads or replaces credentials.
    #[arg(long, value_enum, required_if_eq("client", "codex"))]
    codex_auth: Option<CodexAuth>,
    /// Managed service port by default. Claude may fall back directly; Codex requires a healthy proxy.
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    /// Show only the selected route; do not start a client.
    #[arg(long)]
    print: bool,
    /// Client arguments. Routing, profiles and alternate settings cannot override this launch.
    #[arg(last = true)]
    args: Vec<String>,
}

struct ClientConfig {
    custom_provider: bool,
    configured_proxy: bool,
    scoped_headers: bool,
}

fn scope_header(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("x-gobstopper-scope")
}

fn claude_scoped_headers(headers: &str) -> bool {
    headers
        .lines()
        .any(|line| scope_header(line.split_once(':').map_or(line, |(name, _)| name)))
}

fn codex_scoped_headers(config: &toml::Value) -> bool {
    ["http_headers", "env_http_headers"].iter().any(|field| {
        config
            .get(field)
            .and_then(toml::Value::as_table)
            .is_some_and(|headers| headers.keys().any(|name| scope_header(name)))
    })
}

fn refuse_configuration_layer(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => bail!("system or managed client configuration cannot be verified by launch; no client was launched"),
    }
}

#[cfg(any(target_os = "macos", test))]
fn bounded_presence_probe(
    probe: impl FnOnce() -> Result<bool> + Send + 'static,
    timeout: std::time::Duration,
) -> Result<bool> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("client-policy-presence".into())
        .spawn(move || {
            let _ = sender.send(probe());
        })
        .context("could not inspect managed client policy; no client was launched")?;
    receiver
        .recv_timeout(timeout)
        .context("managed client policy inspection did not finish; no client was launched")?
}

#[cfg(target_os = "macos")]
fn managed_codex_preferences_present() -> Result<bool> {
    use std::ffi::{c_char, c_void};
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            value: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFPreferencesCopyAppValue(
            key: *const c_void,
            application: *const c_void,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }
    bounded_presence_probe(
        || {
            // Use the effective application-domain lookup used by Codex, rather
            // than guessing which plist supplied an MDM preference. No payload
            // is converted, logged, or returned to the launching thread.
            unsafe {
                let domain = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"com.openai.codex".as_ptr(),
                    0x0800_0100,
                );
                let key = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"config_toml_base64".as_ptr(),
                    0x0800_0100,
                );
                if domain.is_null() || key.is_null() {
                    if !domain.is_null() {
                        CFRelease(domain);
                    }
                    if !key.is_null() {
                        CFRelease(key);
                    }
                    bail!(
                        "managed client policy lookup could not initialize; no client was launched"
                    );
                }
                let value = CFPreferencesCopyAppValue(key, domain);
                let present = !value.is_null();
                if present {
                    CFRelease(value);
                }
                CFRelease(key);
                CFRelease(domain);
                Ok(present)
            }
        },
        std::time::Duration::from_secs(2),
    )
}

fn check_codex_external_layers(home: &Path, codex: &Path) -> Result<()> {
    #[cfg(unix)]
    for path in ["/etc/codex/config.toml", "/etc/codex/managed_config.toml"] {
        refuse_configuration_layer(Path::new(path))?;
    }
    // Check both the selected and conventional home: supported clients have
    // used both locations for the non-Unix legacy managed layer.
    refuse_configuration_layer(&codex.join("managed_config.toml"))?;
    refuse_configuration_layer(&home.join(".codex/managed_config.toml"))?;
    #[cfg(windows)]
    {
        let directory = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        refuse_configuration_layer(&directory.join("OpenAI/Codex/config.toml"))?;
        refuse_configuration_layer(&directory.join("OpenAI/Codex/managed_config.toml"))?;
    }
    #[cfg(target_os = "macos")]
    if managed_codex_preferences_present()? {
        bail!("managed Codex preferences cannot be verified by launch; no client was launched");
    }
    Ok(())
}

fn direct_endpoint(auth: CodexAuth) -> &'static str {
    match auth {
        CodexAuth::Chatgpt => CHATGPT,
        CodexAuth::ApiKey => OPENAI,
    }
}

fn read_config(path: &Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("cannot inspect client configuration; no client was launched"),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 {
        bail!("client configuration must be a bounded regular file; no client was launched");
    }
    let mut bytes = Vec::new();
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        bail!("client configuration changed to a non-regular file");
    }
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("client configuration exceeds its size limit");
    }
    Ok(Some(bytes))
}

fn configured_paths(client: Client) -> Result<Vec<PathBuf>> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("home directory is not set")?;
    let home = PathBuf::from(home);
    let (env_name, folder, file) = match client {
        Client::Claude => ("CLAUDE_CONFIG_DIR", ".claude", "settings.json"),
        Client::Codex => ("CODEX_HOME", ".codex", "config.toml"),
    };
    let root = std::env::var_os(env_name)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(folder));
    if client == Client::Codex {
        check_codex_external_layers(&home, &root)?;
    }
    let mut paths = vec![root.join(file)];
    let cwd = std::env::current_dir()?;
    // Check project settings too. Refuse ambiguous routing instead of silently
    // overriding a project provider or moving its credentials to another host.
    let ancestors: Vec<_> = cwd.ancestors().collect();
    if ancestors.len() > 64 {
        bail!("client project configuration nesting exceeds its limit");
    }
    for parent in ancestors.into_iter().rev() {
        paths.push(parent.join(folder).join(file));
        if client == Client::Claude {
            paths.push(parent.join(folder).join("settings.local.json"));
        }
    }
    if client == Client::Claude {
        #[cfg(target_os = "macos")]
        paths.push(PathBuf::from(
            "/Library/Application Support/ClaudeCode/managed-settings.json",
        ));
        #[cfg(target_os = "linux")]
        paths.push(PathBuf::from("/etc/claude-code/managed-settings.json"));
        #[cfg(windows)]
        for key in ["ProgramFiles", "ProgramData"] {
            if let Some(root) = std::env::var_os(key) {
                paths.push(PathBuf::from(root).join("ClaudeCode/managed-settings.json"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    Ok(paths)
}

fn check_base(value: Option<&str>, direct: &str, proxy: &str) -> Result<()> {
    if value.is_some_and(|value| {
        !value.is_empty()
            && value.trim_end_matches('/') != direct
            && value.trim_end_matches('/') != proxy
    }) {
        bail!("custom client upstream is configured; launch preserves it by refusing to change routes");
    }
    Ok(())
}

fn route_environment(key: &str) -> Result<Option<String>> {
    match std::env::var(key) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => bail!("client route environment is not valid text; no client was launched"),
    }
}

fn setting_environment<'a>(value: &'a Value, key: &'a str) -> impl Iterator<Item = &'a Value> {
    // Windows environment names are case-insensitive. Conservatively inspect
    // every spelling and layer on all hosts rather than risk losing policy.
    value["env"]
        .as_object()
        .into_iter()
        .flat_map(|env| env.iter())
        .filter(move |(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
}

fn check_environment(value: &Value, proxy: &str) -> Result<()> {
    for base in setting_environment(value, "ANTHROPIC_BASE_URL") {
        check_base(base.as_str(), ANTHROPIC, proxy)?;
    }
    for key in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_CLIENT_DATA_URL",
    ] {
        if setting_environment(value, key)
            .any(|value| value != "0" && value != "false" && value != false && value != "")
        {
            bail!("an alternate Claude provider is configured; launch cannot change its route");
        }
    }
    Ok(())
}

fn check_child_args(client: Client, args: &[String]) -> Result<()> {
    let forbidden: &[&str] = match client {
        Client::Claude => &[
            "--settings",
            "--setting-sources",
            "--client-data-url",
            "--bare",
            "--safe-mode",
            "--restricted",
            "--desktop",
            "--cloud",
            "--add-dir",
            "--worktree",
            "-w",
            "--cwd",
            "--directory",
        ],
        Client::Codex => &[
            "--profile",
            "-p",
            "--remote",
            "--oss",
            "--local-provider",
            "--cd",
            "-C",
        ],
    };
    for (index, arg) in args.iter().enumerate() {
        if arg == "--" {
            break;
        }
        if arg.starts_with('-')
            && !arg.starts_with("--")
            && arg.len() > 2
            && !(client == Client::Codex && arg.starts_with("-c"))
        {
            bail!("pass short client options separately; combined options cannot be verified by launch");
        }
        if match client {
            Client::Claude => matches!(
                arg.as_str(),
                "auth" | "install" | "update" | "gateway" | "attach" | "respawn"
            ),
            Client::Codex => matches!(
                arg.as_str(),
                "app" | "app-server" | "remote-control" | "login" | "logout" | "cloud"
            ),
        } {
            bail!("launch supports local client sessions, not service, account or remote commands");
        }
        if forbidden.iter().any(|flag| {
            arg == flag
                || arg.starts_with(&format!("{flag}="))
                || (flag.len() == 2 && arg.starts_with(flag))
        }) {
            bail!("client routing or alternate settings arguments conflict with safe launch");
        }
        if client == Client::Codex {
            let setting = if arg == "-c" || arg == "--config" {
                args.get(index + 1).map(String::as_str)
            } else {
                arg.strip_prefix("--config=")
                    .or_else(|| arg.strip_prefix("-c").filter(|value| !value.is_empty()))
            };
            if let Some(setting) = setting {
                let key = setting.split('=').next().unwrap_or_default().trim();
                if key.is_empty()
                    || !key.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                    })
                {
                    bail!("unsupported client configuration key; no client was launched");
                }
            }
            if setting.is_some_and(|value| {
                [
                    "model_provider",
                    "model_providers",
                    "profile",
                    "features.remote",
                    "chatgpt_base_url",
                    "openai_base_url",
                ]
                .iter()
                .any(|prefix| value.trim_start().starts_with(prefix))
            }) {
                bail!("client provider overrides conflict with safe launch");
            }
        }
    }
    Ok(())
}

fn inspect_client(opts: &LaunchOptions, port: u16) -> Result<ClientConfig> {
    check_child_args(opts.client, &opts.args)?;
    if std::env::var_os("GOBSTOPPER_SCOPE").is_some() {
        bail!("an agent context reservation is active; launch cannot change that session's route");
    }
    let proxy = format!("http://127.0.0.1:{port}");
    let mut provider = "openai".to_string();
    let mut configured_proxy = false;
    let mut scoped_headers = false;
    let mut scoped_providers = BTreeSet::new();
    let mut providers = toml::map::Map::new();
    let mut config_bytes = 0usize;
    for path in configured_paths(opts.client)? {
        let Some(bytes) = read_config(&path)? else {
            continue;
        };
        config_bytes = config_bytes.saturating_add(bytes.len());
        if config_bytes > 4 * 1024 * 1024 {
            bail!("combined client configuration exceeds its size limit");
        }
        match opts.client {
            Client::Claude => {
                let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                    anyhow::anyhow!("Claude settings could not be parsed; no client was launched")
                })?;
                check_environment(&value, &proxy)?;
                for headers in setting_environment(&value, "ANTHROPIC_CUSTOM_HEADERS") {
                    scoped_headers |=
                        claude_scoped_headers(headers.as_str().context(
                            "Claude custom headers must be text; no client was launched",
                        )?);
                }
                configured_proxy |=
                    setting_environment(&value, "ANTHROPIC_BASE_URL").any(|value| {
                        value
                            .as_str()
                            .is_some_and(|base| base.trim_end_matches('/') == proxy)
                    });
            }
            Client::Codex => {
                let value: toml::Value = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| toml::from_str(text).ok())
                    .context("Codex settings could not be parsed; no client was launched")?;
                if value.get("profile").is_some() {
                    bail!("a Codex profile is configured; launch cannot infer its effective route");
                }
                if value.get("chatgpt_base_url").is_some() || value.get("openai_base_url").is_some()
                {
                    bail!("a custom OpenAI route is configured; launch cannot replace it");
                }
                if let Some(selected) = value.get("model_provider") {
                    provider = selected
                        .as_str()
                        .context("invalid configured model provider")?
                        .to_string();
                }
                if let Some(table) = value.get("model_providers").and_then(toml::Value::as_table) {
                    for (key, value) in table {
                        // Client configuration layers may recursively merge header
                        // maps. Remember scoped declarations from every layer, even
                        // when a later partial table replaces our local projection.
                        if codex_scoped_headers(value) {
                            scoped_providers.insert(key.clone());
                        }
                        if let Some(existing) =
                            providers.get_mut(key).and_then(toml::Value::as_table_mut)
                        {
                            if let Some(overlay) = value.as_table() {
                                existing.extend(overlay.clone());
                            }
                        } else {
                            providers.insert(key.clone(), value.clone());
                        }
                    }
                }
            }
        }
    }
    match opts.client {
        Client::Claude => {
            if let Some(headers) = route_environment("ANTHROPIC_CUSTOM_HEADERS")? {
                scoped_headers |= claude_scoped_headers(&headers);
            }
            if opts.codex_auth.is_some() {
                bail!("--codex-auth is only valid with --client codex");
            }
            check_base(
                route_environment("ANTHROPIC_BASE_URL")?.as_deref(),
                ANTHROPIC,
                &proxy,
            )?;
            configured_proxy |= route_environment("ANTHROPIC_BASE_URL")?
                .is_some_and(|base| base.trim_end_matches('/') == proxy);
            for key in [
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
                "CLAUDE_CODE_CLIENT_DATA_URL",
            ] {
                if route_environment(key)?
                    .is_some_and(|value| !matches!(value.as_str(), "" | "0" | "false"))
                {
                    bail!("an alternate Claude provider is configured; launch cannot change its route");
                }
            }
        }
        Client::Codex => {
            scoped_headers = scoped_providers.contains(&provider);
            if provider == "openai" {
                bail!("Codex launch requires an existing explicit custom proxy provider; direct fallback cannot be verified. Run Codex normally with its existing settings or repair the proxy");
            }
            let auth = opts
                .codex_auth
                .context("--codex-auth is required for Codex")?;
            let direct = direct_endpoint(auth);
            let proxy = format!(
                "{proxy}{}",
                if auth == CodexAuth::Chatgpt {
                    "/backend-api/codex"
                } else {
                    "/v1"
                }
            );
            check_base(
                route_environment("OPENAI_BASE_URL")?.as_deref(),
                OPENAI,
                &format!("http://127.0.0.1:{port}/v1"),
            )?;
            configured_proxy |= route_environment("OPENAI_BASE_URL")?.is_some_and(|base| {
                base.trim_end_matches('/') == format!("http://127.0.0.1:{port}/v1")
            });
            if provider != "openai" {
                if !provider
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                    || provider.is_empty()
                {
                    bail!("unsupported configured provider identity");
                }
                let config = providers
                    .get(&provider)
                    .context("selected Codex provider is not configured")?;
                let base = config
                    .get("base_url")
                    .and_then(toml::Value::as_str)
                    .context("selected Codex provider has no explicit route")?;
                if base.is_empty() {
                    bail!("selected Codex provider has no explicit route");
                }
                check_base(Some(base), direct, &proxy)?;
                configured_proxy = base.trim_end_matches('/') == proxy;
                if config.get("auth").is_some() || config.get("experimental_bearer_token").is_some()
                {
                    bail!("custom Codex authentication cannot be safely redirected by launch");
                }
                if config
                    .get("wire_api")
                    .and_then(toml::Value::as_str)
                    .is_some_and(|api| api != "responses")
                {
                    bail!("launch requires the Codex Responses protocol");
                }
                let openai = config
                    .get("requires_openai_auth")
                    .and_then(toml::Value::as_bool)
                    == Some(true);
                let env_key = config.get("env_key").and_then(toml::Value::as_str);
                if !openai && env_key.is_none() {
                    bail!("selected Codex authentication source is not supported by launch");
                }
                if openai && env_key.is_some() {
                    bail!("configured Codex provider combines incompatible authentication sources");
                }
                if auth == CodexAuth::Chatgpt && (!openai || env_key.is_some()) {
                    bail!("configured Codex authentication does not match --codex-auth chatgpt");
                }
            }
        }
    }
    Ok(ClientConfig {
        custom_provider: provider != "openai",
        configured_proxy,
        scoped_headers,
    })
}

fn command(opts: &LaunchOptions, config: &ClientConfig, port: u16, proxied: bool) -> Command {
    let mut command = Command::new(match opts.client {
        Client::Claude => "claude",
        Client::Codex => "codex",
    });
    match opts.client {
        Client::Claude => {
            let base = if proxied {
                format!("http://127.0.0.1:{port}")
            } else {
                ANTHROPIC.into()
            };
            // CLI settings override settings.json's env as well as inherited
            // environment. Merely removing ANTHROPIC_BASE_URL is insufficient.
            command.env("ANTHROPIC_BASE_URL", &base).args([
                "--settings",
                &json!({"env":{"ANTHROPIC_BASE_URL":base}}).to_string(),
            ]);
        }
        Client::Codex => {
            // Keep the selected provider's complete effective table, including
            // lower-layer authentication, headers and context policy. Never
            // synthesize a provider or change its URL or environment.
            debug_assert!(proxied && config.custom_provider && config.configured_proxy);
            command.arg("--no-daemon");
        }
    }
    command.args(&opts.args);
    command
}

pub fn run(opts: &LaunchOptions) -> Result<()> {
    let target = crate::proxy_agent::launch_target(opts.port)?;
    let config = inspect_client(opts, target.port)?;
    if opts.client == Client::Codex && !(config.custom_provider && config.configured_proxy) {
        bail!("Codex launch requires an existing explicit custom proxy provider; direct fallback cannot be verified. Run Codex normally with its existing settings or repair the proxy");
    }
    let ready = super::fetch_control_with_timeout(
        target.port,
        super::READY_PATH,
        std::time::Duration::from_secs(2),
    )
    .ok();
    let proxied = ready.as_ref().is_some_and(|ready| {
        ready["ready"] == true
            && ready["official_upstreams"] == true
            && ready["instance_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
            && ready["pid"].as_u64().is_some_and(|pid| pid != 0)
            && target
                .service_id
                .as_ref()
                .is_none_or(|id| ready["service_id"].as_str() == Some(id))
    });
    if opts.client == Client::Codex && !proxied {
        bail!("Codex direct fallback cannot be verified. Run Codex normally with its existing settings or repair the proxy; no client was launched");
    }
    // A live identity mismatch or custom provider is not an unavailable proxy.
    if let Some(ready) = &ready {
        if ready["official_upstreams"] != true
            || target
                .service_id
                .as_ref()
                .is_some_and(|id| ready["service_id"].as_str() != Some(id))
        {
            bail!("proxy route or service identity differs from the configured service; no client was launched");
        }
    }
    if !proxied
        && (config.scoped_headers
            || target.context_constrained
            || ready
                .as_ref()
                .is_some_and(|ready| ready["context_constrained"] == true))
    {
        bail!("proxy context constraints cannot be preserved by direct launch; no client was launched");
    }
    if !proxied && ready.is_none() && config.configured_proxy && target.service_id.is_none() {
        bail!("the unavailable proxy has no managed upstream or context policy to verify; no client was launched");
    }
    if opts.print {
        println!(
            "{}",
            json!({"client":match opts.client { Client::Claude=>"claude",Client::Codex=>"codex" },"route":if opts.client==Client::Codex{"existing"}else if proxied{"proxy"}else{"direct"},"proxy_ready":proxied,"proxy_port":target.port,"persistent_settings_changed":false})
        );
        return Ok(());
    }
    super::log(if opts.client == Client::Codex {
        "launch: proxy healthy; starting Codex with its existing settings"
    } else if proxied {
        "launch: using the healthy local proxy"
    } else {
        "launch: proxy unavailable; starting client with its provider directly"
    });
    let status = command(opts, &config, target.port, proxied)
        .status()
        .context("start selected client")?;
    // Never restart/replay a failed client: its requests may have reached the
    // provider. This fallback applies only before one child is started.
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(client: Client, auth: Option<CodexAuth>) -> LaunchOptions {
        LaunchOptions {
            client,
            codex_auth: auth,
            port: Some(18360),
            print: false,
            args: vec!["--help".into()],
        }
    }
    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }
    #[test]
    fn managed_presence_probe_is_bounded_and_preserves_unknown_outcomes() {
        use std::time::Duration;
        assert!(bounded_presence_probe(|| Ok(true), Duration::from_secs(1)).unwrap());
        assert!(!bounded_presence_probe(|| Ok(false), Duration::from_secs(1)).unwrap());
        assert!(bounded_presence_probe(|| bail!("lookup failed"), Duration::from_secs(1)).is_err());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        assert!(bounded_presence_probe(
            move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(false)
            },
            Duration::from_millis(20)
        )
        .is_err());
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
    }
    #[test]
    fn scoped_header_detection_is_case_insensitive_and_keeps_values_private() {
        for headers in [
            "X-GoBsToPpEr-ScOpE: private",
            "x-extra: normal\r\n  x-gobstopper-scope : private\r\n",
            "X-Gobstopper-Scope",
        ] {
            assert!(claude_scoped_headers(headers));
        }
        assert!(!claude_scoped_headers("x-extra: X-Gobstopper-Scope"));
        for field in ["http_headers", "env_http_headers"] {
            let value: toml::Value =
                toml::from_str(&format!("[{field}]\nX-GoBsToPpEr-ScOpE = 'private'\n")).unwrap();
            assert!(codex_scoped_headers(&value));
        }
    }
    #[test]
    fn claude_direct_route_overrides_settings_and_environment_without_auth_changes() {
        let config = ClientConfig {
            custom_provider: false,
            configured_proxy: false,
            scoped_headers: false,
        };
        for proxied in [false, true] {
            let command = command(&opts(Client::Claude, None), &config, 18360, proxied);
            let expected = if proxied {
                "http://127.0.0.1:18360"
            } else {
                ANTHROPIC
            };
            let arguments = args(&command);
            assert_eq!(arguments[0], "--settings");
            assert_eq!(
                serde_json::from_str::<Value>(&arguments[1]).unwrap(),
                json!({"env":{"ANTHROPIC_BASE_URL":expected}})
            );
            assert_eq!(
                command.get_envs().collect::<Vec<_>>(),
                vec![(
                    std::ffi::OsStr::new("ANTHROPIC_BASE_URL"),
                    Some(std::ffi::OsStr::new(expected))
                )]
            );
            assert_eq!(arguments[2], "--help");
        }
    }
    #[test]
    fn codex_launch_uses_one_process_and_preserves_existing_provider_authentication() {
        let config = ClientConfig {
            custom_provider: true,
            configured_proxy: true,
            scoped_headers: true,
        };
        for auth in [CodexAuth::Chatgpt, CodexAuth::ApiKey] {
            let command = command(&opts(Client::Codex, Some(auth)), &config, 18360, true);
            assert_eq!(args(&command), ["--no-daemon", "--help"]);
            assert_eq!(command.get_envs().count(), 0);
        }
    }
    #[test]
    fn routing_conflicts_cannot_be_smuggled_through_child_arguments() {
        for input in [
            vec!["-hpwork"],
            vec!["-c", "openai_base_url=\"https://private.invalid\""],
            vec!["--profile", "work"],
            vec!["-pwork"],
            vec!["--config=model_provider=elsewhere"],
            vec!["-cmodel_providers.foo.base_url=x"],
            vec!["exec", "-c", "model_provider=foo"],
            vec!["--cd", "/other/project"],
            vec!["-c", "\"model_provider\"=\"elsewhere\""],
            vec![
                "-c",
                "model_providers . gobstopper . base_url=\"elsewhere\"",
            ],
        ] {
            assert!(
                check_child_args(
                    Client::Codex,
                    &input.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()
                )
                .is_err(),
                "{input:?}"
            );
        }
        for input in [
            vec!["-pw"],
            vec!["--settings=x"],
            vec!["--setting-sources", "project"],
            vec!["--client-data-url=http://elsewhere"],
        ] {
            assert!(check_child_args(
                Client::Claude,
                &input.iter().map(|arg| arg.to_string()).collect::<Vec<_>>()
            )
            .is_err());
        }
        assert!(check_child_args(
            Client::Codex,
            &[
                "-c".into(),
                "model_reasoning_effort=\"high\"".into(),
                "--help".into()
            ]
        )
        .is_ok());
        assert!(check_child_args(
            Client::Claude,
            &["--".into(), "--settings is documentation text".into()]
        )
        .is_ok());
    }
    #[test]
    fn alternate_provider_guards_refuse_custom_hosts_and_cloud_routes() {
        let proxy = "http://127.0.0.1:18360";
        assert!(check_environment(&json!({"env":{"ANTHROPIC_BASE_URL":proxy}}), proxy).is_ok());
        assert!(check_environment(
            &json!({"env":{"ANTHROPIC_BASE_URL":"https://example.invalid"}}),
            proxy
        )
        .is_err());
        assert!(check_environment(&json!({"env":{"CLAUDE_CODE_USE_BEDROCK":"1"}}), proxy).is_err());
        assert!(
            check_environment(&json!({"env":{"CLAUDE_CODE_USE_VERTEX":"false"}}), proxy).is_ok()
        );
        assert!(check_base(
            Some("https://api.anthropic.com.evil.invalid"),
            ANTHROPIC,
            proxy
        )
        .is_err());
        assert!(check_base(
            Some("http://127.0.0.1:18360/__gobstopper/s/private"),
            ANTHROPIC,
            proxy
        )
        .is_err());
    }
}

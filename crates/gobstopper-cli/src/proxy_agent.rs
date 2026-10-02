//! Per-user proxy service installation and repair. Only manifest-owned files
//! and jobs are changed. Service managers supervise the proxy; status checks
//! prove the instance identity without sending a model request.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const LABEL: &str = "sh.gobstopper.proxy";
const LEGACY_LABEL: &str = "io.hraness.gobstopper.proxy";
const MAX_FILE: u64 = 256 * 1024;
const SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Platform {
    Macos,
    Linux,
    Windows,
}
impl Platform {
    fn current() -> Result<Self> {
        if cfg!(target_os = "macos") {
            Ok(Self::Macos)
        } else if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "windows") {
            Ok(Self::Windows)
        } else {
            bail!("automatic startup is unsupported on this platform; use gobstopper proxy serve")
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Manifest {
    schema: u32,
    platform: Platform,
    service_id: String,
    #[serde(default)]
    state_dir: Option<PathBuf>,
    executable: PathBuf,
    serve_args: Vec<String>,
    port: u16,
    definition_sha256: String,
    rendered_definition: String,
    #[serde(default)]
    registered_sha256: Option<String>,
    #[serde(default)]
    task_user: Option<String>,
    #[serde(default)]
    working_directory: Option<PathBuf>,
    #[serde(default)]
    environment: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    schema: u32,
    previous: Option<Manifest>,
    candidate: Manifest,
    #[serde(default)]
    legacy: Option<LegacySnapshot>,
}

#[derive(Serialize, Deserialize)]
struct LegacySnapshot {
    path: PathBuf,
    label: String,
    bytes: Vec<u8>,
    arguments: Vec<String>,
    #[serde(default)]
    restoration: Option<LegacyRestoration>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyRestoration {
    bytes: Vec<u8>,
    arguments: Vec<String>,
}

struct Paths {
    root: PathBuf,
    manifest: PathBuf,
    definition: PathBuf,
    log: PathBuf,
}
fn home() -> Result<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .context("home directory is not set")
}
fn paths(platform: Platform) -> Result<Paths> {
    let home = home()?;
    let root = match platform {
        Platform::Windows => std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Gobstopper/service"),
        _ => std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("gobstopper/service"),
    };
    let definition = match platform {
        Platform::Macos => home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist")),
        Platform::Linux => root
            .parent()
            .and_then(Path::parent)
            .context("configuration root")?
            .join("systemd/user/gobstopper-proxy.service"),
        Platform::Windows => root.join("task.xml"),
    };
    let log = match platform {
        Platform::Macos => home.join("Library/Logs/gobstopper-proxy.log"),
        _ => root.join("proxy.log"),
    };
    Ok(Paths {
        manifest: root.join("manifest.json"),
        root,
        definition,
        log,
    })
}

pub fn log_path() -> Result<PathBuf> {
    Ok(paths(Platform::current()?)?.log)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn unix_quote(text: &str) -> String {
    // systemd ExecStart parsing is not a shell; both specifier and variable
    // expansion must be escaped independently of argument quoting.
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}
fn windows_quote(text: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in text.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

fn definition(manifest: &Manifest, log: &Path) -> String {
    let mut args = vec!["proxy".to_owned(), "serve".to_owned()];
    args.extend(manifest.serve_args.clone());
    // The service ID is a normal serve option as Windows scheduled tasks do
    // not support per-action environment variables. It contains no secret.
    args.extend(["--service-id".into(), manifest.service_id.clone()]);
    if let Some(root) = &manifest.state_dir {
        args.extend(["--service-state-dir".into(), root.display().to_string()]);
    }
    match manifest.platform {
        Platform::Macos => {
            let arguments = std::iter::once(manifest.executable.display().to_string())
                .chain(args)
                .map(|s| format!("    <string>{}</string>\n", xml(&s)))
                .collect::<String>();
            let mut extra = manifest
                .working_directory
                .as_ref()
                .map(|p| {
                    format!(
                        "<key>WorkingDirectory</key><string>{}</string>\n",
                        xml(&p.display().to_string())
                    )
                })
                .unwrap_or_default();
            if !manifest.environment.is_empty() {
                extra.push_str("<key>EnvironmentVariables</key><dict>");
                for (key, value) in &manifest.environment {
                    extra.push_str(&format!(
                        "<key>{}</key><string>{}</string>",
                        xml(key),
                        xml(value)
                    ));
                }
                extra.push_str("</dict>\n");
            }
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n{extra}<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>\n{arguments}</array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n<key>ThrottleInterval</key><integer>30</integer>\n<key>ExitTimeOut</key><integer>30</integer>\n<key>StandardOutPath</key><string>{}</string>\n<key>StandardErrorPath</key><string>{}</string>\n</dict></plist>\n", xml(&log.display().to_string()), xml(&log.display().to_string()))
        }
        Platform::Linux => {
            let command = std::iter::once(manifest.executable.display().to_string())
                .chain(args)
                .map(|s| unix_quote(&s))
                .collect::<Vec<_>>()
                .join(" ");
            format!("# Managed by Gobstopper; edit settings through proxy install --replace.\n[Unit]\nDescription=Gobstopper local inference proxy\nStartLimitIntervalSec=300\nStartLimitBurst=5\n\n[Service]\nType=simple\nExecStart={command}\nRestart=on-failure\nRestartSec=10\nTimeoutStopSec=30\nUMask=0077\n\n[Install]\nWantedBy=default.target\n")
        }
        Platform::Windows => {
            let arguments = args
                .iter()
                .map(|s| windows_quote(s))
                .collect::<Vec<_>>()
                .join(" ");
            let user = xml(manifest.task_user.as_deref().unwrap_or(""));
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><RegistrationInfo><Description>Gobstopper local inference proxy</Description></RegistrationInfo><Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers><Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>true</AllowHardTerminate><StartWhenAvailable>true</StartWhenAvailable><RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable><Enabled>true</Enabled><Hidden>false</Hidden><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>5</Count></RestartOnFailure></Settings><Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>{}</Arguments></Exec></Actions></Task>\n", xml(&manifest.executable.display().to_string()), xml(&arguments))
        }
    }
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_FILE {
        bail!(
            "service file must be a bounded regular file: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        bail!("service file is too large");
    }
    Ok(Some(bytes))
}
fn load(p: &Paths) -> Result<Option<Manifest>> {
    let Some(bytes) = read_file(&p.manifest)? else {
        return Ok(None);
    };
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .context("service manifest is damaged; files were preserved")?;
    if manifest.schema != SCHEMA {
        bail!(
            "unsupported service manifest schema {}; files were preserved",
            manifest.schema
        );
    }
    if manifest.service_id.is_empty()
        || !manifest.service_id.bytes().all(|b| b.is_ascii_hexdigit())
        || manifest.port == 0
    {
        bail!("service manifest contains an invalid identity");
    }
    Ok(Some(manifest))
}

/// Diagnostics follow the installed endpoint without contacting the service
/// manager. An explicit port also works when the saved configuration is damaged.
pub fn status_port(explicit: Option<u16>) -> Result<u16> {
    if let Some(port) = explicit {
        if port == 0 {
            bail!("proxy status requires a nonzero port");
        }
        return Ok(port);
    }
    let platform = Platform::current()?;
    installed_status_port(&paths(platform)?, platform)
}

fn installed_status_port(p: &Paths, platform: Platform) -> Result<u16> {
    let Some(manifest) = load(p)? else {
        return Ok(crate::proxy::DEFAULT_PORT);
    };
    if manifest.platform != platform {
        bail!("service manifest belongs to another platform; use proxy status --port to inspect an explicit endpoint");
    }
    Ok(manifest.port)
}

pub(crate) struct LaunchTarget {
    pub port: u16,
    pub service_id: Option<String>,
    pub context_constrained: bool,
}

/// Read configuration only. Launching a client never invokes a service manager,
/// repairs a service, or mutates its definition or pending operation.
pub(crate) fn launch_target(explicit: Option<u16>) -> Result<LaunchTarget> {
    let platform = Platform::current()?;
    configured_launch_target(load(&paths(platform)?)?, platform, explicit)
}

fn configured_launch_target(
    manifest: Option<Manifest>,
    platform: Platform,
    explicit: Option<u16>,
) -> Result<LaunchTarget> {
    if explicit == Some(0) {
        bail!("proxy launch requires a nonzero port");
    }
    let Some(manifest) = manifest else {
        return Ok(LaunchTarget {
            port: explicit.unwrap_or(crate::proxy::DEFAULT_PORT),
            service_id: None,
            context_constrained: false,
        });
    };
    if manifest.platform != platform {
        bail!("proxy service manifest belongs to another platform");
    }
    let port = explicit.unwrap_or(manifest.port);
    if port != manifest.port {
        bail!("proxy launch port differs from the managed service; use the configured port");
    }
    for (index, arg) in manifest.serve_args.iter().enumerate() {
        for (flag, expected) in [
            ("--anthropic-upstream", "https://api.anthropic.com"),
            ("--openai-upstream", "https://api.openai.com"),
            ("--chatgpt-upstream", "https://chatgpt.com"),
        ] {
            let value = if arg == flag {
                manifest.serve_args.get(index + 1).map(String::as_str)
            } else {
                arg.strip_prefix(&format!("{flag}="))
            };
            if value.is_some_and(|value| value.trim_end_matches('/') != expected)
                || (arg == flag && value.is_none())
            {
                bail!("custom proxy upstream is configured; launch cannot redirect its authentication");
            }
        }
    }
    let context_constrained = manifest.serve_args.iter().any(|arg| {
        [
            "--context-window",
            "--client-context-window",
            "--adaptive-context",
            "--strict",
        ]
        .iter()
        .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
    });
    Ok(LaunchTarget {
        port,
        service_id: Some(manifest.service_id),
        context_constrained,
    })
}

struct ServiceLock {
    _file: File,
}
impl ServiceLock {
    fn acquire(p: &Paths) -> Result<Self> {
        fs::create_dir_all(&p.root)?;
        let path = p.root.join("operation.lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
            bail!("service lock is not a regular file");
        }
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options
            .open(path)
            .context("another service operation may be running")?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the descriptor is open and retained by ServiceLock.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                bail!("another service operation is running");
            }
        }
        Ok(Self { _file: file })
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_FILE {
        bail!("service configuration exceeds its size limit");
    }
    let parent = path.parent().context("service file has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".gobstopper-{}.tmp", unique_id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        #[cfg(not(windows))]
        fs::rename(&temporary, path)?;
        #[cfg(windows)]
        replace_windows(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
#[cfg(windows)]
fn replace_windows(source: &Path, dest: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(old: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let old: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let new: Vec<_> = dest.as_os_str().encode_wide().chain(Some(0)).collect();
    // Atomic replacement of our small service configuration only. This does
    // not relax provider-transcript or vault publication guards on Windows.
    if unsafe { MoveFileExW(old.as_ptr(), new.as_ptr(), 0x1 | 0x8) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
pub(crate) fn unique_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    digest(
        format!(
            "{}:{:?}:{}",
            std::process::id(),
            std::time::SystemTime::now(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        )
        .as_bytes(),
    )[..32]
        .to_owned()
}

fn manager(platform: Platform) -> PathBuf {
    match platform {
        Platform::Macos => "/bin/launchctl".into(),
        Platform::Linux => "/usr/bin/systemctl".into(),
        Platform::Windows => std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32/schtasks.exe"),
    }
}
fn run_manager(platform: Platform, args: &[String]) -> Result<Vec<u8>> {
    run_bounded(&manager(platform), args)
}
#[derive(Debug)]
struct ManagerExit(i32);
impl std::fmt::Display for ManagerExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "service manager command failed (exit {})", self.0)
    }
}
impl std::error::Error for ManagerExit {}

fn run_bounded(program: &Path, args: &[String]) -> Result<Vec<u8>> {
    run_bounded_input(program, args, None)
}
fn run_bounded_input(program: &Path, args: &[String], input: Option<&[u8]>) -> Result<Vec<u8>> {
    if input.is_some_and(|bytes| bytes.len() as u64 > MAX_FILE) {
        bail!("service manager input exceeded the limit");
    }
    let mut child = Command::new(program)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("cannot run {}", program.display()))?;
    let mut stdout = child.stdout.take().context("service manager stdout")?;
    // Drain concurrently so an unexpectedly verbose child cannot fill its pipe.
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = (&mut stdout).take(MAX_FILE + 1).read_to_end(&mut data);
        data
    });
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().expect("piped manager stdin");
        let bytes = bytes.to_vec();
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            if let Some(writer) = writer {
                let _ = writer.join();
            }
            bail!("service manager timed out; inspect proxy doctor before retrying");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let bytes = reader.join().unwrap_or_default();
    let written = writer.map(|writer| {
        writer
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("service manager input writer failed")))
    });
    if !status.success() {
        return Err(ManagerExit(status.code().unwrap_or(-1)).into());
    }
    if let Some(written) = written {
        written.context("service manager input failed")?;
    }
    if bytes.len() as u64 > MAX_FILE {
        bail!("service manager output exceeded the limit");
    }
    Ok(bytes)
}
#[cfg(unix)]
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}
#[cfg(not(unix))]
fn domain() -> String {
    "gui".into()
}
fn task_name(m: &Manifest) -> String {
    format!(
        "Gobstopper-Proxy-{}",
        m.task_user.as_deref().unwrap_or("unknown")
    )
}
fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).into()).collect()
}
fn manager_args(m: &Manifest, p: &Paths, action: &str) -> Vec<String> {
    match (m.platform, action) {
        (Platform::Macos, "start") => vec![
            "bootstrap".into(),
            domain(),
            p.definition.display().to_string(),
        ],
        (Platform::Macos, "stop") => vec!["bootout".into(), format!("{}/{LABEL}", domain())],
        (Platform::Macos, _) => vec!["print".into(), format!("{}/{LABEL}", domain())],
        (Platform::Linux, "start") => {
            strings(&["--user", "enable", "--now", "gobstopper-proxy.service"])
        }
        (Platform::Linux, "stop") => {
            strings(&["--user", "disable", "--now", "gobstopper-proxy.service"])
        }
        (Platform::Linux, _) => strings(&[
            "--user",
            "show",
            "gobstopper-proxy.service",
            "--property=LoadState,FragmentPath,ActiveState",
        ]),
        (Platform::Windows, "start") => vec!["/Run".into(), "/TN".into(), task_name(m)],
        (Platform::Windows, "stop") => vec!["/End".into(), "/TN".into(), task_name(m)],
        (Platform::Windows, _) => vec!["/Query".into(), "/TN".into(), task_name(m), "/XML".into()],
    }
}
fn registered(m: &Manifest, p: &Paths) -> Result<Option<Vec<u8>>> {
    match run_manager(m.platform, &manager_args(m, p, "status")) {
        Ok(bytes) => {
            if m.platform == Platform::Linux
                && String::from_utf8_lossy(&bytes)
                    .lines()
                    .any(|l| l == "LoadState=not-found")
            {
                return Ok(None);
            }
            if m.platform == Platform::Linux {
                // systemctl's printable argv[] joins arguments with spaces and
                // cannot establish argument boundaries. Ask the manager for
                // the typed D-Bus property, which preserves the argv array.
                let exec = run_bounded(
                    Path::new("/usr/bin/busctl"),
                    &strings(&[
                        "--user",
                        "--json=short",
                        "get-property",
                        "org.freedesktop.systemd1",
                        "/org/freedesktop/systemd1/unit/gobstopper_2dproxy_2eservice",
                        "org.freedesktop.systemd1.Service",
                        "ExecStart",
                    ]),
                )?;
                let exec: Value = serde_json::from_slice(&exec)
                    .context("systemd ExecStart is not a structured busctl response")?;
                return Ok(Some(serde_json::to_vec(&json!({
                    "unit": String::from_utf8(bytes).context("systemd unit status is not UTF-8")?,
                    "exec_start": exec,
                }))?));
            }
            Ok(Some(bytes))
        }
        Err(error)
            if m.platform == Platform::Macos
                && error
                    .downcast_ref::<ManagerExit>()
                    .is_some_and(|e| e.0 == 113) =>
        {
            Ok(None)
        }
        // schtasks uses exit 1 for both missing tasks and access failures.
        // Query the complete current-user task listing to establish absence;
        // failure to query that list remains an error, never permission to act.
        Err(error)
            if m.platform == Platform::Windows
                && error
                    .downcast_ref::<ManagerExit>()
                    .is_some_and(|e| e.0 == 1) =>
        {
            let list = run_manager(m.platform, &strings(&["/Query", "/FO", "CSV", "/NH"]))?;
            if String::from_utf8_lossy(&list).contains(&task_name(m)) {
                return Err(error);
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
fn service_arguments(m: &Manifest) -> Vec<String> {
    let mut args = vec![
        m.executable.display().to_string(),
        "proxy".into(),
        "serve".into(),
    ];
    args.extend(m.serve_args.clone());
    args.extend(["--service-id".into(), m.service_id.clone()]);
    if let Some(root) = &m.state_dir {
        args.extend(["--service-state-dir".into(), root.display().to_string()]);
    }
    args
}
fn launchd_arguments(text: &str) -> Option<Vec<String>> {
    let mut lines = text.lines().map(str::trim);
    lines.find(|l| *l == "arguments = {")?;
    let mut out = Vec::new();
    for line in lines {
        if line == "}" {
            return Some(out);
        }
        out.push(line.to_owned());
    }
    None
}
fn verify_job(m: &Manifest, p: &Paths) -> Result<()> {
    let Some(bytes) = registered(m, p)? else {
        return Ok(());
    };
    verify_job_bytes(m, p, &bytes)
}
fn verify_job_bytes(m: &Manifest, p: &Paths, bytes: &[u8]) -> Result<()> {
    match m.platform {
        Platform::Windows => {
            if m.registered_sha256.as_deref() != Some(digest(bytes).as_str()) {
                bail!("scheduled task differs from the owned definition; refusing to change it");
            }
        }
        Platform::Macos => {
            let text = String::from_utf8_lossy(bytes);
            let field = |key: &str, value: &str| {
                text.lines().any(|l| l.trim() == format!("{key} = {value}"))
            };
            if !field("program", &m.executable.display().to_string())
                || !field("path", &p.definition.display().to_string())
                || launchd_arguments(&text) != Some(service_arguments(m))
            {
                bail!("loaded LaunchAgent differs from the owned proxy; no process was stopped");
            }
        }
        Platform::Linux => {
            let value: Value = serde_json::from_slice(bytes)
                .context("systemd job identity is not a structured response")?;
            let text = value["unit"]
                .as_str()
                .context("systemd unit status is missing")?;
            let exec = &value["exec_start"];
            let commands = exec["data"].as_array();
            let command = commands.and_then(|commands| {
                (commands.len() == 1)
                    .then(|| commands[0].as_array())
                    .flatten()
            });
            if !text
                .lines()
                .any(|l| l == format!("FragmentPath={}", p.definition.display()))
                || exec["type"] != "a(sasbttttuii)"
                || command.is_none_or(|command| {
                    command.len() != 10
                        || command[0] != m.executable.display().to_string()
                        || command[1] != json!(service_arguments(m))
                        || command[2] != false
                })
            {
                bail!("loaded systemd unit differs from the owned proxy; no process was stopped");
            }
        }
    }
    Ok(())
}
fn verify_owned(m: &Manifest, p: &Paths, allow_missing: bool) -> Result<()> {
    if m.platform != Platform::current()? {
        bail!("service manifest belongs to another platform");
    }
    verify_definition(m, p, allow_missing)?;
    verify_job(m, p)?;
    Ok(())
}
fn verify_definition(m: &Manifest, p: &Paths, allow_missing: bool) -> Result<()> {
    match read_file(&p.definition)? {
        Some(bytes) if digest(&bytes) == m.definition_sha256 => {}
        None if allow_missing => {}
        _ => bail!(
            "service definition was changed outside Gobstopper; refusing to replace or remove it"
        ),
    }
    if digest(m.rendered_definition.as_bytes()) != m.definition_sha256 {
        bail!("manifest settings do not match their owned definition; configuration was preserved");
    }
    Ok(())
}

fn activate(m: &mut Manifest, p: &Paths) -> Result<()> {
    activate_expected(m, p, None)
}

fn activate_expected(m: &mut Manifest, p: &Paths, version: Option<&str>) -> Result<()> {
    require_clear_drain(p)?;
    match m.platform {
        Platform::Linux => {
            run_manager(m.platform, &strings(&["--user", "daemon-reload"]))?;
            if let Err(error) = run_manager(
                m.platform,
                &strings(&["--user", "reset-failed", "gobstopper-proxy.service"]),
            ) {
                // Some managers have not loaded a newly written unit yet.
                // Only a fresh, explicit not-found result permits proceeding;
                // permission and manager failures must not be hidden.
                if registered(m, p)?.is_some() {
                    return Err(error);
                }
            }
        }
        Platform::Windows => {
            run_manager(
                m.platform,
                &[
                    "/Create".into(),
                    "/TN".into(),
                    task_name(m),
                    "/XML".into(),
                    p.definition.display().to_string(),
                    "/F".into(),
                ],
            )?;
            m.registered_sha256 = Some(digest(
                &registered(m, p)?.context("created task is missing")?,
            ));
            write_atomic(&p.manifest, &serde_json::to_vec_pretty(m)?)?;
        }
        Platform::Macos => {}
    }
    run_manager(m.platform, &manager_args(m, p, "start"))?;
    if let Some(version) = version {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if status(m.port).is_ok_and(|live| {
                matches_identity(m, &live)
                    && live["draining"] == false
                    && live["version"] == version
            }) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("upgraded proxy did not become healthy at version {version}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    wait_ready(m)
}
const DRAIN_WAIT_SECS: u64 = 600;

/// A stop dispatch without a durable acknowledgement is intentionally not
/// retried or reopened: a surviving manager child may still execute it later.
#[derive(Clone, Serialize, Deserialize)]
struct DrainOperation {
    schema: u32,
    service_id: String,
    executable: PathBuf,
    definition_sha256: String,
    port: u16,
    pid: u64,
    instance_id: String,
    owner: String,
    epoch: u64,
    protocol: u64,
    stage: String,
}
/// Managed definitions pin this absolute directory so startup and the
/// controller consult the same journal despite login-environment differences.
pub fn check_startup_drain(service_id: Option<&str>, root: Option<&Path>, port: u16) -> Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    let id =
        service_id.context("a managed service identity is required for its state directory")?;
    if !root.is_absolute() {
        bail!("service state directory must be absolute");
    }
    let p = Paths {
        root: root.to_path_buf(),
        manifest: root.join("manifest.json"),
        definition: root.join("unused"),
        log: root.join("unused"),
    };
    let m =
        load(&p)?.context("managed service manifest is missing; startup preserved the drain")?;
    if m.service_id != id
        || m.port != port
        || m.state_dir.as_deref() != Some(root)
        || m.executable != std::env::current_exe()?.canonicalize()?
    {
        bail!("managed service startup identity differs from its saved manifest");
    }
    check_startup_journal(&m, &p)
}
fn check_startup_journal(m: &Manifest, p: &Paths) -> Result<()> {
    if let Some(op) = load_drain(m, p)? {
        if op.protocol > 1 || op.stage != "waiting" {
            bail!("an unresolved service stop prevents startup inference; inspect proxy doctor and reconcile its journal");
        }
    }
    Ok(())
}
fn require_clear_drain(p: &Paths) -> Result<()> {
    if read_file(&drain_path(p))?.is_some() {
        bail!("a recorded service stop has not been reconciled; no service was started");
    }
    Ok(())
}
fn drain_path(p: &Paths) -> PathBuf {
    p.root.join("drain-operation.json")
}
fn save_drain(p: &Paths, operation: &DrainOperation) -> Result<()> {
    write_atomic(&drain_path(p), &serde_json::to_vec_pretty(operation)?)
}
fn clear_drain(p: &Paths) -> Result<()> {
    match fs::remove_file(drain_path(p)) {
        Ok(()) => {
            #[cfg(unix)]
            File::open(&p.root)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn load_drain(m: &Manifest, p: &Paths) -> Result<Option<DrainOperation>> {
    let Some(bytes) = read_file(&drain_path(p))? else {
        return Ok(None);
    };
    let op: DrainOperation = serde_json::from_slice(&bytes)
        .context("drain journal is damaged; admission was preserved")?;
    if op.schema != 1
        || op.service_id != m.service_id
        || op.executable != m.executable
        || op.port != m.port
        || op.definition_sha256 != m.definition_sha256
        || !matches!(
            op.stage.as_str(),
            "waiting" | "commit_intent" | "committed" | "stop_started" | "stop_acknowledged"
        )
    {
        bail!("drain journal identity or stage differs; service was preserved");
    }
    Ok(Some(op))
}
fn drain_body(op: &DrainOperation) -> Value {
    json!({"instance_id":op.instance_id,"pid":op.pid,"owner":op.owner,"epoch":op.epoch,"wait_secs":DRAIN_WAIT_SECS})
}
fn same_drain_process(m: &Manifest, op: &DrainOperation, live: &Value) -> bool {
    matches_identity(m, live)
        && live["pid"] == op.pid
        && (op.protocol == 0 || live["instance_id"] == op.instance_id)
}
fn validate_lease_reply(op: &DrainOperation, reply: &Value) -> Result<()> {
    if reply["protocol"] != 1
        || reply["instance_id"] != op.instance_id
        || reply["pid"] != op.pid
        || reply["owner"] != op.owner
        || reply["epoch"] != op.epoch
        || reply["active_inference"].as_u64().is_none()
    {
        bail!("drain lease acknowledgement has a different identity or invalid activity count");
    }
    Ok(())
}
fn lease_control(m: &Manifest, op: &DrainOperation, action: &str) -> Result<Value> {
    let mut body = drain_body(op);
    if action == "acquire" {
        body["epoch"] = json!(op
            .epoch
            .checked_sub(1)
            .context("invalid initial drain epoch")?);
    }
    let reply = service_request(
        m,
        &format!("drain-lease/{action}"),
        &serde_json::to_vec(&body)?,
    )?;
    validate_lease_reply(op, &reply)?;
    Ok(reply)
}
fn stopped_port(port: u16) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // A failed status request alone is never evidence of a stopped process.
        if ensure_free(port).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("owned proxy did not release its port; drain journal and configuration were preserved");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn acquire_committed_drain(
    m: &Manifest,
    p: &Paths,
    live: &Value,
    prior: Option<DrainOperation>,
) -> Result<DrainOperation> {
    let protocol = match live.get("drain_control") {
        None => 0, // 0.8.0 and earlier only support the idle-only drain.
        Some(control) if control["protocol"] == 1 && control["startup_guard"] == true => 1,
        // Definitions written before startup journal protection retain the
        // older idle-only behavior until install --replace updates them.
        Some(control) if control["protocol"] == 1 && control["startup_guard"] == false => 0,
        _ => bail!("unsupported service drain protocol; no stop attempted"),
    };
    let previous = prior.filter(|op| same_drain_process(m, op, live));
    let mut op = if let Some(op) = previous {
        op
    } else {
        DrainOperation {
            schema: 1,
            service_id: m.service_id.clone(),
            executable: m.executable.clone(),
            definition_sha256: m.definition_sha256.clone(),
            port: m.port,
            pid: live["pid"].as_u64().context("proxy PID is missing")?,
            instance_id: live["instance_id"].as_str().unwrap_or("").to_owned(),
            owner: unique_id(),
            epoch: if protocol == 1 {
                live["drain_control"]["epoch"]
                    .as_u64()
                    .and_then(|e| e.checked_add(1))
                    .context("invalid drain epoch")?
            } else {
                0
            },
            protocol,
            stage: "waiting".into(),
        }
    };
    if op.protocol != protocol || (protocol == 1 && op.instance_id.is_empty()) {
        bail!("drain protocol changed for the recorded process");
    }
    if protocol == 0 {
        if live["keep_awake"]["active_inference"].as_u64() != Some(0) {
            bail!("this older proxy only supports idle upgrades; active inference was preserved");
        }
        op.stage = "commit_intent".into();
        save_drain(p, &op)?;
        service_control(m, true)?;
        op.stage = "committed".into();
        save_drain(p, &op)?;
        return Ok(op);
    }
    // A lost commit acknowledgement may already have installed the permanent
    // barrier. Inspect using its private token before touching admission.
    let mut reply = if live["drain_control"]["phase"] == "committed" {
        lease_control(m, &op, "inspect")?
    } else if live["drain_control"]["phase"] == "open" {
        op.epoch = live["drain_control"]["epoch"]
            .as_u64()
            .and_then(|e| e.checked_add(1))
            .context("invalid drain epoch")?;
        op.stage = "waiting".into();
        save_drain(p, &op)?;
        lease_control(m, &op, "acquire")?
    } else {
        lease_control(m, &op, "inspect")?
    };
    let deadline = Instant::now() + Duration::from_secs(DRAIN_WAIT_SECS);
    let result = (|| {
        if reply["phase"] == "waiting" && reply["active_inference"] != 0 {
            eprintln!("Gobstopper is holding new inference while active requests finish (up to {DRAIN_WAIT_SECS}s).");
        }
        loop {
            if reply["phase"] == "committed" {
                if reply["active_inference"] != 0 {
                    bail!("committed drain reported active inference");
                }
                op.stage = "committed".into();
                save_drain(p, &op)?;
                return Ok(());
            }
            if reply["phase"] != "waiting" {
                bail!("proxy no longer owns the waiting drain");
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for inference; no process stopped");
            }
            if reply["active_inference"] == 0 {
                // These reads can be slow. Commit rechecks remaining lifetime,
                // exact incarnation, token and active==0 under admission's lock.
                verify_owned(m, p, true)?;
                verify_job(m, p)?;
                op.stage = "commit_intent".into();
                save_drain(p, &op)?;
                reply = lease_control(m, &op, "commit")?;
            } else {
                std::thread::sleep(Duration::from_millis(500));
                reply = lease_control(m, &op, "renew")?;
            }
        }
    })();
    if let Err(error) = result {
        if op.stage == "waiting" && lease_control(m, &op, "release").is_ok() {
            clear_drain(p)?;
        }
        // A commit request without an acknowledgement must be reconciled on
        // repair. It cannot safely be treated as a waiting cancellation.
        return Err(error);
    }
    Ok(op)
}
fn stop(m: &Manifest, p: &Paths) -> Result<()> {
    let prior = load_drain(m, p)?;
    if prior.as_ref().is_some_and(|op| op.stage == "stop_started") {
        bail!("an external service stop has no durable acknowledgement; admission remains closed. Inspect proxy doctor and reconcile the recorded manager operation before restarting; elapsed time or controller death is not proof it is safe");
    }
    if prior
        .as_ref()
        .is_some_and(|op| op.stage == "stop_acknowledged")
    {
        stopped_port(m.port)?;
        clear_drain(p)?;
        return Ok(());
    }
    if registered(m, p)?.is_none() {
        ensure_free(m.port)?;
        clear_drain(p)?;
        return Ok(());
    }
    verify_job(m, p)?;
    let live = status(m.port).context("the registered proxy is not responsive; cannot prove held admission, so no process was stopped")?;
    if !matches_identity(m, &live) {
        bail!("running proxy identity differs from the managed service; no process stopped");
    }
    let mut op = acquire_committed_drain(m, p, &live, prior)?;
    // The non-expiring barrier still belongs to this process immediately
    // before external dispatch. Public status alone cannot prove ownership.
    let live = status(m.port)?;
    if !same_drain_process(m, &op, &live) {
        bail!("proxy process changed before stop; no process stopped");
    }
    if op.protocol == 1 {
        let lease = lease_control(m, &op, "inspect")?;
        if lease["phase"] != "committed" || lease["active_inference"] != 0 {
            bail!("proxy did not confirm a committed idle drain; no process stopped");
        }
    } else if live["draining"] != true || live["keep_awake"]["active_inference"] != 0 {
        bail!("legacy owned proxy is not drained and idle; no process stopped");
    }
    verify_job(m, p)?;
    op.stage = "stop_started".into();
    save_drain(p, &op)?;
    // No resume on any manager error: the manager may already have accepted
    // the stop even when its caller times out or loses the acknowledgement.
    run_manager(m.platform, &manager_args(m, p, "stop"))?;
    op.stage = "stop_acknowledged".into();
    save_drain(p, &op)?;
    stopped_port(m.port)?;
    clear_drain(p)?;
    Ok(())
}

fn service_control(m: &Manifest, drain: bool) -> Result<()> {
    if m.service_id.is_empty()
        || !m
            .service_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        bail!("service identity is not a valid control header");
    }
    let action = if drain { "drain" } else { "resume" };
    let value = service_request(m, action, &[])?;
    if value["drained"] != drain {
        bail!("proxy did not confirm the requested admission state");
    }
    Ok(())
}
fn service_request(m: &Manifest, action: &str, body: &[u8]) -> Result<Value> {
    if m.service_id.is_empty()
        || !m
            .service_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        bail!("service identity is not a valid control header");
    }
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, m.port));
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_millis(300))?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(socket,
        "POST /gobstopper/service/{action} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nx-gobstopper-service-id: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        m.port, m.service_id, body.len())?;
    socket.write_all(body)?;
    let mut bytes = Vec::new();
    socket.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE || !bytes.starts_with(b"HTTP/1.1 200 ") {
        bail!("proxy service control was refused or unavailable; inspect proxy doctor");
    }
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid service control response")?;
    Ok(serde_json::from_slice(&bytes[split + 4..])?)
}

#[cfg(test)]
fn verify_control_reply(bytes: &[u8], drain: bool) -> Result<()> {
    if bytes.len() as u64 > MAX_FILE || !bytes.starts_with(b"HTTP/1.1 200 ") {
        bail!(
            "proxy service control was refused or unavailable; retry after active requests finish"
        );
    }
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid service control response")?;
    let value: Value = serde_json::from_slice(&bytes[split + 4..])?;
    if value["drained"] != drain {
        bail!("proxy did not confirm the requested admission state");
    }
    Ok(())
}

fn status(port: u16) -> Result<Value> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_millis(300))?;
    socket.set_read_timeout(Some(Duration::from_millis(300)))?;
    socket.set_write_timeout(Some(Duration::from_millis(300)))?;
    write!(
        socket,
        "GET /gobstopper/status HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = Vec::new();
    socket.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE || !bytes.starts_with(b"HTTP/1.1 200 ") {
        bail!("unexpected status response");
    }
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("invalid status response")?;
    let value: Value = serde_json::from_slice(&bytes[split + 4..])?;
    if value["name"] != "gobstopper-proxy" || value["port"] != port {
        bail!("port is occupied by another application");
    }
    Ok(value)
}
fn matches_identity(m: &Manifest, value: &Value) -> bool {
    value["name"] == "gobstopper-proxy"
        && value["port"] == m.port
        && value["service_id"] == m.service_id
        && value["executable"]
            .as_str()
            .is_some_and(|p| Path::new(p) == m.executable)
        && value["pid"].as_u64().is_some_and(|pid| pid > 0)
}
fn needs_version_restart(m: &Manifest, live: &Value, current_executable: &Path) -> bool {
    m.executable == current_executable && live["version"] != env!("CARGO_PKG_VERSION")
}
fn ready_identity(m: &Manifest, live: &Value, current: &Path) -> bool {
    matches_identity(m, live)
        && live["draining"] == false
        && !needs_version_restart(m, live, current)
}
fn wait_ready(m: &Manifest) -> Result<()> {
    let current = std::env::current_exe()?.canonicalize()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if status(m.port).is_ok_and(|v| ready_identity(m, &v, &current)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("service started but the expected proxy identity is not healthy; run gobstopper proxy doctor");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn ensure_free(port: u16) -> Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .with_context(|| format!("127.0.0.1:{port} is occupied; no process was stopped"))?;
    drop(listener);
    Ok(())
}
fn legacy_files() -> Result<Vec<PathBuf>> {
    let dir = home()?.join("Library/LaunchAgents");
    Ok([LABEL, LEGACY_LABEL]
        .into_iter()
        .map(|label| dir.join(format!("{label}.plist")))
        .filter(|p| p.exists())
        .collect())
}
fn current_user_sid() -> Result<String> {
    let program = manager(Platform::Windows).with_file_name("whoami.exe");
    let output = run_bounded(&program, &strings(&["/user", "/fo", "csv", "/nh"]))?;
    String::from_utf8_lossy(&output)
        .split(',')
        .next_back()
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| {
            s.starts_with("S-1-")
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
        })
        .context("could not establish the current Windows user SID")
}

fn pending_path(p: &Paths) -> PathBuf {
    p.root.join("pending-operation.json")
}
fn clear_pending(p: &Paths) -> Result<()> {
    if pending_path(p).exists() {
        fs::remove_file(pending_path(p))?;
    }
    #[cfg(unix)]
    File::open(&p.root)?.sync_all()?;
    Ok(())
}
fn write_pending(p: &Paths, previous: Option<&Manifest>, candidate: &Manifest) -> Result<()> {
    write_atomic(
        &pending_path(p),
        &serde_json::to_vec_pretty(&Pending {
            schema: SCHEMA,
            previous: previous.cloned(),
            candidate: candidate.clone(),
            legacy: None,
        })?,
    )
}
fn same_manifest(a: Option<&Manifest>, b: Option<&Manifest>) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}
fn completed_registration(candidate: &Manifest, current: &Manifest) -> bool {
    // Windows normalizes a task's XML during registration. activate() persists
    // that queried identity before starting the task; a crash can leave the
    // pending snapshot one field behind. Accept only that initial assignment.
    if candidate.platform != Platform::Windows
        || candidate.registered_sha256.is_some()
        || current.registered_sha256.as_deref().is_none_or(|hash| {
            hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        return false;
    }
    let mut before_registration = current.clone();
    before_registration.registered_sha256 = None;
    same_manifest(Some(candidate), Some(&before_registration))
}
fn recover_pending(p: &Paths) -> Result<bool> {
    let Some(bytes) = read_file(&pending_path(p))? else {
        return Ok(false);
    };
    let pending: Pending = serde_json::from_slice(&bytes)
        .context("service operation journal is damaged; files preserved")?;
    if pending.schema != SCHEMA {
        bail!("service operation journal uses an unsupported schema");
    }
    if let Some(legacy) = pending.legacy.as_ref() {
        return recover_legacy(p, &pending.candidate, legacy);
    }
    let previous = pending.previous.as_ref();
    let current = load(p)?;
    let candidate = current
        .as_ref()
        .filter(|current| completed_registration(&pending.candidate, current))
        .unwrap_or(&pending.candidate);
    if !same_manifest(current.as_ref(), Some(candidate))
        && !same_manifest(current.as_ref(), previous)
    {
        bail!("service manifest changed outside the pending operation; files preserved");
    }
    if let Some(bytes) = read_file(&p.definition)? {
        let hash = digest(&bytes);
        if hash != candidate.definition_sha256
            && previous.is_none_or(|m| m.definition_sha256 != hash)
        {
            bail!("service definition changed outside the pending operation; files preserved");
        }
    }
    // Resolve drain state even when the old job has already disappeared. An
    // absent listener must never bypass an uncertain external stop journal.
    if drain_path(p).exists() {
        let owner = if load_drain(candidate, p).is_ok() {
            candidate
        } else if let Some(old) = previous.filter(|old| load_drain(old, p).is_ok()) {
            old
        } else {
            bail!("drain journal belongs to neither recorded service identity");
        };
        stop(owner, p)?;
    }
    // A registered job must match a recorded identity, including any initial
    // Windows registration digest durably saved in the current manifest.
    if registered(candidate, p)?.is_some() {
        let owned = if verify_job(candidate, p).is_ok() {
            candidate
        } else if let Some(old) = previous.filter(|m| verify_job(m, p).is_ok()) {
            old
        } else {
            bail!("loaded service does not belong to the pending operation; no process stopped");
        };
        if let Ok(live) = status(owned.port) {
            if !matches_identity(owned, &live)
                || (live["drain_control"]["protocol"] != 1
                    && live["keep_awake"]["active_inference"].as_u64() != Some(0))
            {
                bail!("pending operation recovery is waiting for the owned idle proxy; no process stopped");
            }
        } else {
            ensure_free(owned.port)?;
        }
        stop(owned, p)?;
    }
    let mut target = previous.unwrap_or(candidate).clone();
    if target.platform != Platform::current()?
        || digest(target.rendered_definition.as_bytes()) != target.definition_sha256
        || !target.executable.is_file()
    {
        bail!("pending service snapshot cannot be restored; files preserved");
    }
    ensure_free(target.port)?;
    write_atomic(&p.definition, target.rendered_definition.as_bytes())?;
    write_atomic(&p.manifest, &serde_json::to_vec_pretty(&target)?)?;
    activate(&mut target, p)?;
    clear_pending(p)?;
    println!("Interrupted service operation recovered; the owned proxy is healthy.");
    Ok(true)
}

fn parse_legacy(bytes: &[u8]) -> Result<Value> {
    let json = run_bounded_input(
        Path::new("/usr/bin/plutil"),
        &strings(&["-convert", "json", "-o", "-", "-"]),
        Some(bytes),
    )?;
    serde_json::from_slice(&json).context("legacy service is not a supported plist")
}

// Supported legacy settings contain JSON-compatible plist values. Stable
// serialization lets recovery verify the prepared definition independently of
// plutil's formatting, while the immutable original retains its exact bytes.
fn plist_value(value: &Value) -> Result<String> {
    fn text(value: &str) -> Result<String> {
        if value.chars().any(|c| {
            (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || matches!(c, '\u{fffe}' | '\u{ffff}')
        }) {
            bail!("legacy setting contains a character unsupported by XML plists");
        }
        Ok(xml(value).replace('\r', "&#13;"))
    }
    Ok(match value {
        Value::String(s) => format!("<string>{}</string>", text(s)?),
        Value::Bool(v) => format!("<{v}/>"),
        Value::Number(v) => {
            let tag = if v.is_i64() || v.is_u64() {
                "integer"
            } else {
                "real"
            };
            format!("<{tag}>{v}</{tag}>")
        }
        Value::Array(values) => format!(
            "<array>{}</array>",
            values
                .iter()
                .map(plist_value)
                .collect::<Result<Vec<_>>>()?
                .join("")
        ),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            let entries = entries
                .into_iter()
                .map(|(key, value)| Ok(format!("<key>{}</key>{}", text(key)?, plist_value(value)?)))
                .collect::<Result<Vec<_>>>()?;
            format!("<dict>{}</dict>", entries.join(""))
        }
        Value::Null => bail!("legacy setting contains a null unsupported by plists"),
    })
}

fn prepare_legacy_restoration(
    original: &Value,
    args: &[String],
    effective: &[String],
) -> Result<Option<LegacyRestoration>> {
    if args.len() < 3
        || args[1..3] != ["proxy", "serve"]
        || original["ProgramArguments"] != json!(args)
    {
        bail!("legacy snapshot arguments differ from its original definition");
    }
    if effective == &args[3..] {
        return Ok(None);
    }
    // Migration may add only the observed tail default; it cannot change an
    // existing option or authorize unrelated execution through the journal.
    let tail = effective
        .last()
        .and_then(|v| v.parse::<u64>().ok())
        .context("invalid prepared legacy arguments")?;
    if legacy_effective_args(&args[3..], &json!({"keep_tail_percent":tail}))? != effective {
        bail!("prepared legacy arguments change unrecorded settings");
    }
    let mut arguments = args[..3].to_vec();
    arguments.extend_from_slice(effective);
    let mut restored = original.clone();
    restored["ProgramArguments"] = json!(arguments);
    let bytes = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">{}</plist>\n", plist_value(&restored)?).into_bytes();
    Ok(Some(LegacyRestoration { bytes, arguments }))
}

fn validate_legacy_restoration(
    candidate: &Manifest,
    snapshot: &LegacySnapshot,
    original: &Value,
) -> Result<()> {
    if original["Label"] != snapshot.label
        || original["ProgramArguments"] != json!(snapshot.arguments)
    {
        bail!("legacy snapshot identity differs from its original definition");
    }
    let expected =
        prepare_legacy_restoration(original, &snapshot.arguments, &candidate.serve_args)?;
    if let Some(prepared) = &snapshot.restoration {
        if expected.as_ref() != Some(prepared) {
            bail!("prepared legacy restoration differs from the recorded migration");
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LegacyIdentity {
    Original,
    Restoration,
}
#[derive(Debug)]
struct LoadedLegacy {
    identity: LegacyIdentity,
    pid: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum LegacyRecoveryPlan {
    CompleteCandidate,
    RestartOriginal,
    Preserve(LegacyIdentity),
    Restore(LegacyIdentity),
}
fn legacy_recovery_plan(
    new_job: bool,
    loaded: Option<LegacyIdentity>,
    candidate_healthy: bool,
    prepared: bool,
) -> Result<LegacyRecoveryPlan> {
    if new_job && loaded.is_some() {
        bail!("both migration service identities are loaded; processes and files preserved");
    }
    if new_job && candidate_healthy {
        return Ok(LegacyRecoveryPlan::CompleteCandidate);
    }
    Ok(match loaded {
        Some(LegacyIdentity::Original) if prepared => LegacyRecoveryPlan::RestartOriginal,
        Some(identity) => LegacyRecoveryPlan::Preserve(identity),
        None => LegacyRecoveryPlan::Restore(if prepared {
            LegacyIdentity::Restoration
        } else {
            LegacyIdentity::Original
        }),
    })
}

fn legacy_job_identity(snapshot: &LegacySnapshot, bytes: &[u8]) -> Result<LoadedLegacy> {
    let text = String::from_utf8_lossy(bytes);
    let args = launchd_arguments(&text).context("legacy job has no exact argument list")?;
    let identity = if args == snapshot.arguments {
        LegacyIdentity::Original
    } else if snapshot
        .restoration
        .as_ref()
        .is_some_and(|prepared| args == prepared.arguments)
    {
        LegacyIdentity::Restoration
    } else {
        bail!("legacy job differs from the migration snapshot; no process stopped");
    };
    if !text
        .lines()
        .any(|line| line.trim() == format!("path = {}", snapshot.path.display()))
        || !text
            .lines()
            .any(|line| line.trim() == format!("program = {}", args[0]))
    {
        bail!(
            "legacy job path or executable differs from the migration snapshot; no process stopped"
        );
    }
    let pid = text
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("pid = ")
                .and_then(|pid| pid.parse::<u64>().ok())
        })
        .filter(|pid| *pid > 0);
    Ok(LoadedLegacy { identity, pid })
}

fn legacy_job(snapshot: &LegacySnapshot) -> Result<Option<LoadedLegacy>> {
    let output = match run_manager(
        Platform::Macos,
        &["print".into(), format!("{}/{}", domain(), snapshot.label)],
    ) {
        Ok(output) => output,
        Err(error)
            if error
                .downcast_ref::<ManagerExit>()
                .is_some_and(|e| e.0 == 113) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    legacy_job_identity(snapshot, &output).map(Some)
}

fn verify_legacy_files(p: &Paths, candidate: &Manifest, legacy: &LegacySnapshot) -> Result<()> {
    let current = load(p)?;
    if current
        .as_ref()
        .is_some_and(|m| !same_manifest(Some(m), Some(candidate)))
    {
        bail!("migration manifest changed outside the recorded operation");
    }
    for path in [&p.definition, &legacy.path] {
        if let Some(bytes) = read_file(path)? {
            let is_legacy = path == &legacy.path
                && (bytes == legacy.bytes
                    || legacy
                        .restoration
                        .as_ref()
                        .is_some_and(|r| bytes == r.bytes));
            let is_candidate =
                path == &p.definition && digest(&bytes) == candidate.definition_sha256;
            if !is_legacy && !is_candidate {
                bail!("migration definition was externally changed; files preserved");
            }
        }
    }
    Ok(())
}

fn legacy_ready(
    candidate: &Manifest,
    legacy: &LegacySnapshot,
    job: &LoadedLegacy,
    live: &Value,
) -> bool {
    if live["name"] != "gobstopper-proxy" || live["port"] != candidate.port || job.pid.is_none() {
        return false;
    }
    let tail = legacy
        .restoration
        .as_ref()
        .and_then(|r| r.arguments.last())
        .and_then(|v| v.parse::<u64>().ok());
    // Pre-manifest releases did not expose process identity. Their observed
    // setting still has to match if the journal records a known value.
    if job.identity == LegacyIdentity::Original {
        return live.get("pid").is_none_or(|pid| pid.as_u64() == job.pid)
            && tail.is_none_or(|tail| live["keep_tail_percent"].as_u64() == Some(tail));
    }
    live["pid"].as_u64() == job.pid
        && live["executable"]
            .as_str()
            .is_some_and(|s| Path::new(s) == candidate.executable)
        && live["keep_tail_percent"].as_u64() == tail
        && live["service_id"].as_str().is_none_or(str::is_empty)
}

fn legacy_restart_guard(candidate: &Manifest, job: &LoadedLegacy, live: &Value) -> Result<()> {
    if job.identity != LegacyIdentity::Original
        || job.pid.is_none()
        || live["name"] != "gobstopper-proxy"
        || live["port"] != candidate.port
        || live.get("pid").is_some_and(|pid| pid.as_u64() != job.pid)
    {
        bail!("legacy process identity cannot be confirmed; process and journal preserved");
    }
    let connections = live["active_connections"]
        .as_u64()
        .context("legacy activity is unknown; process and journal preserved")?;
    let inference = live
        .get("keep_awake")
        .map(|power| {
            power["active_inference"]
                .as_u64()
                .context("legacy inference activity is unknown; process and journal preserved")
        })
        .transpose()?
        .unwrap_or(0);
    if connections > 1 || inference > 0 {
        bail!("legacy proxy is active; process and journal preserved; pause clients then run proxy repair to preserve settings on future restarts");
    }
    Ok(())
}
fn recover_legacy(p: &Paths, candidate: &Manifest, legacy: &LegacySnapshot) -> Result<bool> {
    if drain_path(p).exists() {
        stop(candidate, p)?;
    }
    require_clear_drain(p)?;
    if Platform::current()? != Platform::Macos
        || ![LABEL, LEGACY_LABEL].contains(&legacy.label.as_str())
        || legacy.path
            != home()?
                .join("Library/LaunchAgents")
                .join(format!("{}.plist", legacy.label))
    {
        bail!("migration snapshot target is invalid");
    }
    if candidate.platform != Platform::Macos
        || candidate.schema != SCHEMA
        || digest(candidate.rendered_definition.as_bytes()) != candidate.definition_sha256
    {
        bail!("migration candidate snapshot is invalid");
    }
    validate_legacy_restoration(candidate, legacy, &parse_legacy(&legacy.bytes)?)?;
    if Path::new(&legacy.arguments[0]).canonicalize()? != candidate.executable {
        bail!("legacy executable differs from the migration candidate; files preserved");
    }
    verify_legacy_files(p, candidate, legacy)?;
    let registered_job = registered(candidate, p)?;
    let new_job = registered_job
        .as_ref()
        .is_some_and(|bytes| verify_job_bytes(candidate, p, bytes).is_ok());
    if registered_job.is_some() && !new_job && legacy.label != LABEL {
        bail!("an unrelated job owns the new service label; files preserved");
    }
    let mut loaded_legacy = if new_job && legacy.label == LABEL {
        None
    } else {
        legacy_job(legacy)?
    };
    let plan = legacy_recovery_plan(
        new_job,
        loaded_legacy.as_ref().map(|job| job.identity),
        new_job && status(candidate.port).is_ok_and(|v| matches_identity(candidate, &v)),
        legacy.restoration.is_some(),
    )?;
    if plan == LegacyRecoveryPlan::CompleteCandidate {
        // A completed startup whose caller died before recording success
        // needs no restart, even if its model requests are now active.
        verify_legacy_files(p, candidate, legacy)?;
        if read_file(&p.definition)?.is_none() {
            write_atomic(&p.definition, candidate.rendered_definition.as_bytes())?;
        }
        verify_definition(candidate, p, false)?;
        if load(p)?.is_none() {
            write_atomic(&p.manifest, &serde_json::to_vec_pretty(candidate)?)?;
        }
        if legacy.path != p.definition && legacy.path.exists() {
            fs::remove_file(&legacy.path)?;
        }
        clear_pending(p)?;
        println!("Interrupted migration completed; the expected proxy is healthy.");
        return Ok(true);
    }
    if new_job {
        ensure_free(candidate.port)?;
        stop(candidate, p)?;
    }
    if plan == LegacyRecoveryPlan::RestartOriginal {
        let observed = loaded_legacy
            .as_ref()
            .context("original legacy job is missing")?;
        let live = status(candidate.port)
            .context("legacy proxy must be healthy before recovery can restart it")?;
        legacy_restart_guard(candidate, observed, &live)?;
        let fresh = legacy_job(legacy)?
            .context("legacy job disappeared before restart; journal preserved")?;
        if fresh.identity != observed.identity || fresh.pid != observed.pid {
            bail!("legacy job changed before restart; process and journal preserved");
        }
        verify_legacy_files(p, candidate, legacy)?;
        run_manager(
            Platform::Macos,
            &["bootout".into(), format!("{}/{}", domain(), legacy.label)],
        )?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while ensure_free(candidate.port).is_err() {
            if Instant::now() >= deadline {
                bail!("legacy job did not release its port; journal retained");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        loaded_legacy = None;
    }
    let expected = match plan {
        LegacyRecoveryPlan::RestartOriginal => LegacyIdentity::Restoration,
        LegacyRecoveryPlan::Preserve(identity) | LegacyRecoveryPlan::Restore(identity) => identity,
        LegacyRecoveryPlan::CompleteCandidate => unreachable!("completed recovery returned above"),
    };
    if loaded_legacy.is_none() {
        ensure_free(candidate.port)?;
    }
    // Recheck after manager/health operations before changing any recorded file.
    verify_legacy_files(p, candidate, legacy)?;
    if legacy.path != p.definition && p.definition.exists() {
        fs::remove_file(&p.definition)?;
    }
    let restore_bytes = if expected == LegacyIdentity::Restoration {
        &legacy
            .restoration
            .as_ref()
            .context("prepared restoration is missing")?
            .bytes
    } else {
        &legacy.bytes
    };
    if read_file(&legacy.path)?.as_deref() != Some(restore_bytes.as_slice()) {
        write_atomic(&legacy.path, restore_bytes)?;
    }
    if p.manifest.exists() {
        fs::remove_file(&p.manifest)?;
    }
    if loaded_legacy.is_none() {
        run_manager(
            Platform::Macos,
            &[
                "bootstrap".into(),
                domain(),
                legacy.path.display().to_string(),
            ],
        )?;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if legacy_job(legacy)?.as_ref().is_some_and(|job| {
            job.identity == expected
                && status(candidate.port)
                    .is_ok_and(|live| legacy_ready(candidate, legacy, job, &live))
        }) {
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "legacy service restored but readiness not confirmed; migration journal retained"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    clear_pending(p)?;
    println!("Migration recovered; the recorded legacy service is healthy and its known settings are preserved.");
    Ok(true)
}

/// Install once; identical repeated setup checks the live service and repairs
/// a missing job. `replace` changes settings only for an owned installation.
pub fn install(serve_args: &[String], port: u16, replace: bool, print: bool) -> Result<()> {
    if port == 0 {
        bail!("startup service requires a fixed nonzero port");
    }
    let platform = Platform::current()?;
    let p = paths(platform)?;
    let executable = std::env::current_exe()?.canonicalize()?;
    let old = load(&p)?;
    let mut m = Manifest {
        schema: SCHEMA,
        platform,
        service_id: old
            .as_ref()
            .map(|m| m.service_id.clone())
            .unwrap_or_else(unique_id),
        state_dir: Some(p.root.clone()),
        executable,
        serve_args: serve_args.to_vec(),
        port,
        definition_sha256: String::new(),
        rendered_definition: String::new(),
        registered_sha256: None,
        working_directory: old.as_ref().and_then(|m| m.working_directory.clone()),
        environment: old
            .as_ref()
            .map(|m| m.environment.clone())
            .unwrap_or_default(),
        task_user: if platform == Platform::Windows {
            Some(current_user_sid()?)
        } else {
            None
        },
    };
    let bytes = definition(&m, &p.log).into_bytes();
    m.definition_sha256 = digest(&bytes);
    m.rendered_definition = String::from_utf8(bytes.clone())?;
    if print {
        println!("{}", String::from_utf8_lossy(&bytes));
        return Ok(());
    }
    let _lock = ServiceLock::acquire(&p)?;
    if read_file(&pending_path(&p))?.is_some() {
        bail!("a previous service operation was interrupted; run gobstopper proxy repair");
    }
    // Reread under the cross-process lock, then compare the exact predecessor.
    let locked = load(&p)?;
    if serde_json::to_vec(&locked)? != serde_json::to_vec(&old)? {
        bail!("service configuration changed during setup; retry");
    }
    if let Some(old) = old.as_ref() {
        verify_owned(old, &p, true)?;
        if old.definition_sha256 == m.definition_sha256 {
            return repair_locked(old.clone(), &p);
        }
        if !replace {
            bail!("different proxy settings are installed; use gobstopper proxy install --replace");
        }
        if let Ok(live) = status(old.port) {
            if !matches_identity(old, &live) {
                bail!("running proxy identity differs from the managed service; no process was stopped");
            }
            if live["drain_control"]["protocol"] != 1
                && live["keep_awake"]["active_inference"].as_u64() != Some(0)
            {
                bail!(
                    "this older proxy only supports idle upgrades; active inference was preserved"
                );
            }
        }
    } else {
        if read_file(&p.definition)?.is_some() {
            bail!("an unmanaged service definition exists; use proxy doctor to inspect it; no files were replaced");
        }
        if platform == Platform::Macos && !legacy_files()?.is_empty() {
            bail!("a legacy Gobstopper LaunchAgent exists; use proxy doctor to inspect its exact settings before migration");
        }
        ensure_free(port)?;
        if registered(&m, &p)?.is_some() {
            bail!("an unmanaged service job already exists; no job was replaced");
        }
    }
    fs::create_dir_all(p.log.parent().context("log directory")?)?;
    write_pending(&p, old.as_ref(), &m)?;
    if let Some(old) = old.as_ref() {
        // Keep an immutable predecessor before touching its manager or files.
        write_atomic(
            &p.root.join(format!("backup-{}.json", unique_id())),
            &serde_json::to_vec_pretty(old)?,
        )?;
        if old.port != port {
            ensure_free(port)?;
        }
        stop(old, &p)?;
    }
    write_atomic(&p.definition, &bytes)?;
    write_atomic(&p.manifest, &serde_json::to_vec_pretty(&m)?)?;
    if let Err(error) = activate(&mut m, &p) {
        // Reconcile before attempting rollback, never blindly repeat startup.
        stop(&m, &p).context("new service failed and its stop could not be verified; configuration retained for repair")?;
        if let Some(mut old) = old {
            write_atomic(&p.definition, old.rendered_definition.as_bytes())?;
            write_atomic(&p.manifest, &serde_json::to_vec_pretty(&old)?)?;
            let restored = activate(&mut old, &p).is_ok();
            if restored {
                clear_pending(&p)?;
            }
            bail!("new service failed: {error}; previous configuration restored, previous service healthy: {restored}");
        }
        return Err(error);
    }
    clear_pending(&p)?;
    println!("Gobstopper is running on http://127.0.0.1:{port} and starts at login.");
    Ok(())
}

fn repair_locked(mut m: Manifest, p: &Paths) -> Result<()> {
    verify_owned(&m, p, true)?;
    if !m.executable.is_file() {
        bail!("installed executable is missing; reinstall the binary before repairing startup");
    }
    if drain_path(p).exists() {
        stop(&m, p)?;
        activate(&mut m, p)?;
        println!("Recorded service stop recovered; the owned proxy is healthy.");
        return Ok(());
    }
    if read_file(&p.definition)?.is_none() {
        write_atomic(&p.definition, m.rendered_definition.as_bytes())?;
    }
    let is_registered = registered(&m, p)?.is_some();
    if let Ok(live) = status(m.port) {
        if !matches_identity(&m, &live) {
            bail!("another proxy owns the configured port; no process was stopped");
        }
        if !is_registered {
            bail!("the proxy is healthy but its service-manager job is missing; the running process was preserved");
        }
        if live["drain_control"]["phase"] == "committed"
            || needs_version_restart(&m, &live, &std::env::current_exe()?.canonicalize()?)
        {
            // Installing a new binary at the same permanent path leaves the
            // definition unchanged. A healthy predecessor process still
            // needs the same guarded restart as a configuration upgrade.
            stop(&m, p)?;
            activate(&mut m, p)?;
            println!("Gobstopper restarted with the installed version and is healthy.");
            return Ok(());
        }
        if live["drain_control"]["phase"] == "waiting" {
            bail!("a waiting drain lease owns admission; it will expire if its controller has stopped");
        }
        if live["draining"] == true {
            service_control(&m, false)
                .context("owned proxy is healthy but inference admission could not be resumed")?;
            println!("Gobstopper is healthy; inference admission resumed.");
            return Ok(());
        }
        println!("Gobstopper is healthy; no changes needed.");
        return Ok(());
    }
    ensure_free(m.port)?;
    if is_registered {
        stop(&m, p)?;
    }
    activate(&mut m, p)?;
    println!("Gobstopper startup repaired and the expected proxy is healthy.");
    Ok(())
}

/// Preserve an observed legacy default that changed between released versions.
/// Explicit arguments take precedence; older status schemas may omit this field.
fn legacy_effective_args(args: &[String], live: &Value) -> Result<Vec<String>> {
    let mut effective = args.to_vec();
    if !args
        .iter()
        .any(|arg| arg == "--keep-tail-percent" || arg.starts_with("--keep-tail-percent="))
    {
        if let Some(value) = live.get("keep_tail_percent") {
            let maximum = u64::from(gobstopper_adapters::request::MAX_KEEP_TAIL_PERCENT);
            let value = value
                .as_u64()
                .filter(|value| *value <= maximum)
                .with_context(|| format!("legacy keep_tail_percent must be an integer from 0 through {maximum}; no service was changed"))?;
            effective.extend(["--keep-tail-percent".into(), value.to_string()]);
        }
    }
    Ok(effective)
}

/// Explicitly adopt a pre-manifest macOS LaunchAgent. Its exact executable,
/// argv, supported environment, and loaded job identity must all agree.
/// Provider settings and source/session data are never changed.
pub fn migrate(print: bool, allow_dependent_caller: bool) -> Result<()> {
    if Platform::current()? != Platform::Macos {
        bail!("legacy LaunchAgent migration is available on macOS only");
    }
    let p = paths(Platform::Macos)?;
    if pending_path(&p).exists() || load(&p)?.is_some() {
        if !print {
            crate::proxy_caller::refuse(status_port(None)?, allow_dependent_caller)?;
        }
        return repair(print);
    }
    let files = legacy_files()?;
    if files.len() != 1 {
        bail!(
            "migration requires exactly one known legacy proxy LaunchAgent; found {}",
            files.len()
        );
    }
    let legacy_path = &files[0];
    let legacy_bytes = read_file(legacy_path)?.context("legacy service disappeared")?;
    let value = parse_legacy(&legacy_bytes)?;
    let label = value["Label"].as_str().context("legacy job has no label")?;
    if ![LABEL, LEGACY_LABEL].contains(&label)
        || legacy_path.file_stem().and_then(|p| p.to_str()) != Some(label)
    {
        bail!("legacy filename and label disagree");
    }
    let args: Vec<String> = value["ProgramArguments"]
        .as_array()
        .context("legacy job has no argument list")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .context("invalid legacy argument")
        })
        .collect::<Result<_>>()?;
    let executable = std::env::current_exe()?.canonicalize()?;
    if args.len() < 3
        || Path::new(&args[0]).canonicalize()? != executable
        || args[1] != "proxy"
        || args[2] != "serve"
        || args
            .iter()
            .any(|s| s.contains(['\n', '\r']) || s.starts_with("--service-id"))
    {
        bail!("legacy job must run this installed Gobstopper executable with proxy serve; no job was changed");
    }
    let environment: BTreeMap<String, String> = match value.get("EnvironmentVariables") {
        None => BTreeMap::new(),
        Some(v) => serde_json::from_value(v.clone())?,
    };
    const ALLOWED: &[&str] = &[
        "HOME",
        "PATH",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "GOBSTOPPER_STATS_FILE",
        "GOBSTOPPER_EVENTS_FILE",
    ];
    if environment
        .keys()
        .any(|key| !ALLOWED.contains(&key.as_str()))
    {
        bail!("legacy job has unsupported environment settings; preserve and review them before migration");
    }
    // Refuse extra launch behavior rather than silently throwing it away.
    const KEYS: &[&str] = &[
        "Label",
        "ProgramArguments",
        "EnvironmentVariables",
        "WorkingDirectory",
        "RunAtLoad",
        "KeepAlive",
        "ThrottleInterval",
        "StandardOutPath",
        "StandardErrorPath",
        "ExitTimeOut",
    ];
    if value
        .as_object()
        .context("legacy plist is not a dictionary")?
        .keys()
        .any(|k| !KEYS.contains(&k.as_str()))
    {
        bail!("legacy job has additional launch settings; no settings were discarded");
    }
    let mut port = crate::proxy::DEFAULT_PORT;
    let serve_args = args[3..].to_vec();
    for (i, arg) in serve_args.iter().enumerate() {
        if arg == "--port" {
            port = serve_args
                .get(i + 1)
                .context("legacy port is missing")?
                .parse()?;
        } else if let Some(v) = arg.strip_prefix("--port=") {
            port = v.parse()?;
        }
    }
    if port == 0 {
        bail!("legacy service has no stable port");
    }
    let _lock = if print {
        None
    } else {
        Some(ServiceLock::acquire(&p)?)
    };
    if !print
        && (load(&p)?.is_some()
            || read_file(legacy_path)?.as_deref() != Some(legacy_bytes.as_slice()))
    {
        bail!("service changed during migration; retry");
    }
    let target = format!("{}/{label}", domain());
    let job = run_manager(Platform::Macos, &["print".into(), target.clone()])?;
    let job = String::from_utf8_lossy(&job);
    if launchd_arguments(&job) != Some(args.clone())
        || !job
            .lines()
            .any(|l| l.trim() == format!("path = {}", legacy_path.display()))
    {
        bail!("loaded legacy job differs from its file; no process was stopped");
    }
    let live = status(port).context("legacy proxy must be healthy before migration")?;
    let serve_args = legacy_effective_args(&serve_args, &live)?;
    let mut m = Manifest {
        schema: SCHEMA,
        platform: Platform::Macos,
        service_id: unique_id(),
        state_dir: Some(p.root.clone()),
        executable,
        serve_args,
        port,
        definition_sha256: String::new(),
        rendered_definition: String::new(),
        registered_sha256: None,
        task_user: None,
        working_directory: value["WorkingDirectory"].as_str().map(PathBuf::from),
        environment,
    };
    let bytes = definition(&m, &p.log).into_bytes();
    m.definition_sha256 = digest(&bytes);
    m.rendered_definition = String::from_utf8(bytes.clone())?;
    let restoration = prepare_legacy_restoration(&value, &args, &m.serve_args)?;
    let snapshot = LegacySnapshot {
        path: legacy_path.clone(),
        label: label.to_owned(),
        bytes: legacy_bytes.clone(),
        arguments: args,
        restoration,
    };
    if print {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"action":"migrate_legacy_service","source":legacy_path,"destination":p.definition,"executable":m.executable,"port":port,"serve_args":m.serve_args,"proxy_settings_preserved":true,"backup_required":true})
            )?
        );
        return Ok(());
    }
    crate::proxy_caller::refuse(port, allow_dependent_caller)?;
    let observed = legacy_job_identity(&snapshot, job.as_bytes())?;
    legacy_restart_guard(&m, &observed, &live)?;
    let pending = Pending {
        schema: SCHEMA,
        previous: None,
        candidate: m.clone(),
        legacy: Some(snapshot),
    };
    let backup = p.root.join(format!("legacy-{}.plist", unique_id()));
    write_atomic(&backup, &legacy_bytes)?;
    write_atomic(&pending_path(&p), &serde_json::to_vec_pretty(&pending)?)?;
    let result = (|| -> Result<()> {
        let snapshot = pending
            .legacy
            .as_ref()
            .context("legacy snapshot is missing")?;
        let fresh = legacy_job(snapshot)?
            .context("legacy job disappeared before migration; journal retained")?;
        if fresh.identity != observed.identity || fresh.pid != observed.pid {
            bail!("legacy job changed before migration; process and journal preserved");
        }
        legacy_restart_guard(&m, &fresh, &status(port)?)?;
        verify_legacy_files(&p, &m, snapshot)?;
        run_manager(Platform::Macos, &["bootout".into(), target])?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while ensure_free(port).is_err() {
            if Instant::now() >= deadline {
                bail!(
                    "legacy job did not release its port; original file and backup were preserved"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if legacy_path != &p.definition {
            fs::remove_file(legacy_path)?;
        }
        write_atomic(&p.definition, &bytes)?;
        write_atomic(&p.manifest, &serde_json::to_vec_pretty(&m)?)?;
        activate(&mut m, &p)
    })();
    if let Err(error) = result {
        // The same identity and crash-boundary checks govern immediate failure
        // and a later repair. Never blindly overwrite or bootstrap the original.
        match recover_pending(&p) {
            Ok(_) => bail!("migration failed: {error}; recorded service recovered; original backup {}", backup.display()),
            Err(recovery) => bail!("migration failed: {error}; recovery requires attention: {recovery}; journal and original backup {} retained", backup.display()),
        }
    }
    clear_pending(&p)?;
    println!(
        "Gobstopper service migrated and healthy. Legacy configuration preserved at {}.",
        backup.display()
    );
    Ok(())
}

pub fn repair(print: bool) -> Result<()> {
    if print {
        println!("{}", serde_json::to_string_pretty(&inspect()?)?);
        return Ok(());
    }
    let p = paths(Platform::current()?)?;
    let _lock = ServiceLock::acquire(&p)?;
    if recover_pending(&p)? {
        return Ok(());
    }
    let m = load(&p)?.context("no managed service is installed; run gobstopper proxy install")?;
    repair_locked(m, &p)
}

pub fn inspect() -> Result<Value> {
    let platform = Platform::current()?;
    let p = paths(platform)?;
    let upgrade = read_file(&p.root.join("upgrade.json"))?
        .map(|bytes| serde_json::from_slice::<Value>(&bytes))
        .transpose()?;
    let m = load(&p)?;
    let legacy = if platform == Platform::Macos {
        legacy_files()?
    } else {
        vec![]
    };
    let Some(m) = m else {
        return Ok(
            json!({"schema": SCHEMA, "platform": platform, "installed": false, "pending_operation":pending_path(&p).exists(), "legacy_definitions": legacy, "upgrade": upgrade, "next": "gobstopper proxy install"}),
        );
    };
    let ownership = verify_owned(&m, &p, true).err().map(|e| e.to_string());
    let live = status(m.port).ok();
    let drain = load_drain(&m, &p);
    let drain_stage = drain
        .as_ref()
        .ok()
        .and_then(|op| op.as_ref())
        .map(|op| op.stage.as_str());
    let drain_error = drain.as_ref().err().map(|e| e.to_string());
    let manager = registered(&m, &p);
    let is_registered = manager.as_ref().is_ok_and(|value| value.is_some());
    let manager_error = manager.err().map(|error| error.to_string());
    Ok(json!({
        "schema":SCHEMA, "platform":platform, "installed":true,
        "upgrade":upgrade,
        "pending_operation":pending_path(&p).exists(),
        "drain_operation_stage":drain_stage,"drain_operation_error":drain_error,
        "drain_control":live.as_ref().and_then(|v|v.get("drain_control")),
        "healthy":!drain_path(&p).exists() && !pending_path(&p).exists() && ownership.is_none() && is_registered && p.definition.is_file() && live.as_ref().is_some_and(|v| matches_identity(&m,v) && v["draining"] != true),
        "definition_present":p.definition.is_file(), "definition":p.definition,
        "ownership_error":ownership, "port":m.port, "executable":m.executable,
        "service_id":m.service_id, "manager_registered":is_registered, "manager_error":manager_error,
        "live_version":live.as_ref().and_then(|v|v.get("version")),
        "keep_awake":live.as_ref().and_then(|v|v.get("keep_awake")), "next":"gobstopper proxy repair"
        ,"draining":live.as_ref().and_then(|v|v.get("draining"))
    }))
}

pub(crate) fn upgrade_journal_path() -> Result<PathBuf> {
    Ok(paths(Platform::current()?)?.root.join("upgrade.json"))
}

pub(crate) fn upgrade_write_journal(bytes: &[u8]) -> Result<()> {
    let p = paths(Platform::current()?)?;
    write_atomic(&p.root.join("upgrade.json"), bytes)
}

pub(crate) struct UpgradeJob {
    pub(crate) definition: PathBuf,
    pub(crate) log: PathBuf,
    pub(crate) text: String,
}

pub(crate) fn upgrade_job(executable: &Path, stage: &Path) -> Result<UpgradeJob> {
    let platform = Platform::current()?;
    let p = paths(platform)?;
    Ok(upgrade_job_for(
        platform,
        &home()?,
        &p.root,
        executable,
        stage,
    ))
}

fn upgrade_job_for(
    platform: Platform,
    home: &Path,
    root: &Path,
    executable: &Path,
    stage: &Path,
) -> UpgradeJob {
    let log = if platform == Platform::Macos {
        home.join("Library/Logs/gobstopper-upgrade.log")
    } else {
        root.join("upgrade.log")
    };
    let definition = if platform == Platform::Macos {
        home.join("Library/LaunchAgents/sh.gobstopper.upgrade.plist")
    } else {
        root.join("gobstopper-upgrade.transient")
    };
    let args = [
        executable.display().to_string(),
        "__upgrade-controller".into(),
        "--stage".into(),
        stage.display().to_string(),
        "--state-dir".into(),
        root.display().to_string(),
    ];
    let text = if platform == Platform::Macos {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>sh.gobstopper.upgrade</string><key>ProgramArguments</key><array>{}</array><key>RunAtLoad</key><true/><key>StandardOutPath</key><string>{}</string><key>StandardErrorPath</key><string>{}</string></dict></plist>\n", args.iter().map(|arg| format!("<string>{}</string>", xml(arg))).collect::<String>(), xml(&log.display().to_string()), xml(&log.display().to_string()))
    } else {
        format!(
            "systemd-run --user --unit gobstopper-upgrade --collect {}\n",
            args.iter()
                .map(|arg| unix_quote(arg))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    UpgradeJob {
        definition,
        log,
        text,
    }
}

pub(crate) fn upgrade_launch(job: &UpgradeJob, executable: &Path, stage: &Path) -> Result<()> {
    let platform = Platform::current()?;
    fs::create_dir_all(job.log.parent().context("upgrade log directory")?)?;
    if platform == Platform::Macos {
        if job.definition.exists() {
            bail!("an upgrade job definition already exists; inspect proxy doctor");
        }
        fs::create_dir_all(job.definition.parent().context("LaunchAgent directory")?)?;
        write_atomic(&job.definition, job.text.as_bytes())?;
        run_manager(
            platform,
            &[
                "bootstrap".into(),
                domain(),
                job.definition.display().to_string(),
            ],
        )?;
    } else if platform == Platform::Linux {
        let state = paths(platform)?.root;
        run_bounded(
            Path::new("/usr/bin/systemd-run"),
            &[
                "--user".into(),
                "--unit".into(),
                "gobstopper-upgrade".into(),
                "--collect".into(),
                format!("--property=StandardOutput=append:{}", job.log.display()),
                format!("--property=StandardError=append:{}", job.log.display()),
                executable.display().to_string(),
                "__upgrade-controller".into(),
                "--stage".into(),
                stage.display().to_string(),
                "--state-dir".into(),
                state.display().to_string(),
            ],
        )?;
    } else {
        bail!("detached upgrade is unsupported on this platform");
    }
    Ok(())
}

pub(crate) fn upgrade_cleanup(job: &UpgradeJob) -> Result<()> {
    if Platform::current()? == Platform::Macos {
        if job.definition.exists() {
            fs::remove_file(&job.definition)?;
        }
        run_manager(
            Platform::Macos,
            &[
                "bootout".into(),
                format!("{}/sh.gobstopper.upgrade", domain()),
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn upgrade_probe(read_only: bool) -> Result<(PathBuf, String)> {
    let p = paths(Platform::current()?)?;
    let _lock = if read_only {
        None
    } else {
        Some(ServiceLock::acquire(&p)?)
    };
    if pending_path(&p).exists() || drain_path(&p).exists() {
        bail!("a service operation is pending; inspect proxy doctor and run proxy repair");
    }
    let m = load(&p)?.context("no managed proxy service is installed")?;
    verify_owned(&m, &p, true)?;
    if registered(&m, &p)?.is_none() {
        bail!("managed proxy service is not registered");
    }
    let live = status(m.port)?;
    if !matches_identity(&m, &live) || live["draining"] != false {
        bail!("managed proxy service is not healthy");
    }
    Ok((
        m.executable,
        live["version"]
            .as_str()
            .context("proxy version is missing")?
            .to_owned(),
    ))
}

pub(crate) struct UpgradeControl {
    manifest: Manifest,
    paths: Paths,
    _lock: ServiceLock,
}

impl UpgradeControl {
    pub(crate) fn open(executable: &Path) -> Result<Self> {
        let paths = paths(Platform::current()?)?;
        let lock = ServiceLock::acquire(&paths)?;
        let manifest = load(&paths)?.context("managed service disappeared")?;
        if manifest.executable != executable || pending_path(&paths).exists() {
            bail!("managed service changed while upgrade was staged");
        }
        verify_owned(&manifest, &paths, true)?;
        Ok(Self {
            manifest,
            paths,
            _lock: lock,
        })
    }

    pub(crate) fn stop(&self) -> Result<()> {
        match stop(&self.manifest, &self.paths) {
            Ok(()) => Ok(()),
            Err(error) => {
                if load_drain(&self.manifest, &self.paths)?
                    .as_ref()
                    .is_some_and(|operation| operation.stage == "stop_acknowledged")
                {
                    stop(&self.manifest, &self.paths)
                } else {
                    Err(error)
                }
            }
        }
    }

    pub(crate) fn start(&mut self, version: &str) -> Result<()> {
        activate_expected(&mut self.manifest, &self.paths, Some(version))
    }

    pub(crate) fn restart_previous(&mut self) -> Result<()> {
        activate(&mut self.manifest, &self.paths)
    }
}

pub fn uninstall() -> Result<()> {
    let p = paths(Platform::current()?)?;
    let _lock = ServiceLock::acquire(&p)?;
    if read_file(&pending_path(&p))?.is_some() {
        bail!("a service operation is pending; run gobstopper proxy repair before uninstalling");
    }
    let Some(m) = load(&p)? else {
        println!("No managed proxy service is installed. Existing files were preserved.");
        return Ok(());
    };
    verify_owned(&m, &p, true)?;
    if let Ok(live) = status(m.port) {
        if !matches_identity(&m, &live) {
            bail!("running service identity differs; no process was stopped");
        }
        if live["drain_control"]["protocol"] != 1
            && live["keep_awake"]["active_inference"].as_u64() != Some(0)
        {
            bail!("this older proxy only supports idle uninstall; active inference was preserved");
        }
    }
    stop(&m, &p)?;
    if m.platform == Platform::Windows && registered(&m, &p)?.is_some() {
        run_manager(
            m.platform,
            &["/Delete".into(), "/TN".into(), task_name(&m), "/F".into()],
        )?;
    }
    if p.definition.exists() {
        fs::remove_file(&p.definition)?;
    }
    fs::remove_file(&p.manifest)?;
    if m.platform == Platform::Linux {
        run_manager(m.platform, &strings(&["--user", "daemon-reload"]))?;
    }
    println!("Gobstopper startup removed. Session data, logs, and backups were kept.");
    Ok(())
}

pub fn installed() -> bool {
    Platform::current()
        .and_then(paths)
        .and_then(|p| load(&p))
        .ok()
        .flatten()
        .is_some()
}

pub(crate) fn repair_may_restart() -> Result<bool> {
    let p = paths(Platform::current()?)?;
    if pending_path(&p).exists() || drain_path(&p).exists() {
        return Ok(true);
    }
    let Some(manifest) = load(&p)? else {
        return Ok(false);
    };
    let Ok(live) = status(manifest.port) else {
        return Ok(true);
    };
    Ok(live["draining"] != false
        || needs_version_restart(&manifest, &live, &std::env::current_exe()?.canonicalize()?))
}
pub fn restart_command() -> String {
    "gobstopper proxy repair".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_job_is_one_shot() {
        let job = upgrade_job_for(
            Platform::Macos,
            Path::new("/tmp/home"),
            Path::new("/tmp/service"),
            Path::new("/tmp/bin/gobstopper"),
            Path::new("/tmp/bin/stage"),
        );
        assert!(job.text.contains("<string>sh.gobstopper.upgrade</string>"));
        assert!(job.text.contains("<key>RunAtLoad</key><true/>"));
        assert!(!job.text.contains("KeepAlive"));
        assert!(job.text.contains("<string>__upgrade-controller</string>"));
        assert!(job.text.contains("<string>/tmp/bin/stage</string>"));
    }
    fn manifest(platform: Platform) -> Manifest {
        Manifest {
            schema: 1,
            platform,
            service_id: "aabbcc".into(),
            state_dir: None,
            executable: PathBuf::from("/opt/Gob Stopper/gobstopper"),
            serve_args: vec!["--port".into(), "8260".into()],
            port: 8260,
            definition_sha256: String::new(),
            rendered_definition: String::new(),
            registered_sha256: None,
            task_user: Some("S-1-5-21-123".into()),
            working_directory: None,
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn proxy_status_port_follows_manifest_and_preserves_explicit_diagnostics() {
        let root = std::env::temp_dir().join(format!("gobstopper-status-port-{}", unique_id()));
        fs::create_dir_all(&root).unwrap();
        let p = Paths {
            manifest: root.join("manifest.json"),
            definition: root.join("service.plist"),
            log: root.join("proxy.log"),
            root: root.clone(),
        };
        assert_eq!(
            installed_status_port(&p, Platform::Macos).unwrap(),
            crate::proxy::DEFAULT_PORT
        );
        let mut m = manifest(Platform::Macos);
        m.port = 18360;
        fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        assert_eq!(installed_status_port(&p, Platform::Macos).unwrap(), 18360);
        assert!(installed_status_port(&p, Platform::Linux).is_err());
        m.port = 0;
        fs::write(&p.manifest, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(installed_status_port(&p, Platform::Macos).is_err());
        fs::write(&p.manifest, b"broken").unwrap();
        assert!(installed_status_port(&p, Platform::Macos).is_err());
        assert_eq!(status_port(Some(18361)).unwrap(), 18361);
        assert!(status_port(Some(0)).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn definitions_bound_restart_and_do_not_request_display_wake_or_admin() {
        let mac = definition(&manifest(Platform::Macos), Path::new("/tmp/gobstopper.log"));
        assert!(mac.contains("<key>ThrottleInterval</key><integer>30</integer>"));
        assert!(mac.contains("<string>--service-id</string>"));
        let linux = definition(&manifest(Platform::Linux), Path::new("/tmp/log"));
        assert!(linux.contains("StartLimitBurst=5"));
        assert!(linux.contains("RestartSec=10"));
        assert!(!linux.contains("sudo"));
        let windows = definition(&manifest(Platform::Windows), Path::new("log"));
        assert!(windows.contains("LeastPrivilege"));
        assert!(windows.contains("InteractiveToken"));
        assert!(windows.contains("<Count>5</Count>"));
        assert!(!windows.contains("WakeToRun"));
    }
    #[test]
    fn escaping_preserves_literal_arguments() {
        assert_eq!(
            unix_quote("a $HOME %u \"quoted\""),
            "\"a $$HOME %%u \\\"quoted\\\"\""
        );
        assert_eq!(windows_quote("x\\"), "\"x\\\\\"");
        assert_eq!(windows_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(xml("<&\"'"), "&lt;&amp;&quot;&apos;");
    }
    #[test]
    fn health_requires_exact_instance_identity() {
        let m = manifest(Platform::Macos);
        let mut v = json!({"name":"gobstopper-proxy","port":8260,"service_id":"aabbcc","pid":42,"executable":"/opt/Gob Stopper/gobstopper"});
        assert!(matches_identity(&m, &v));
        v["service_id"] = json!("another");
        assert!(!matches_identity(&m, &v));
        assert!(!matches_identity(&m, &json!({})));
    }
    #[test]
    fn replacing_the_installed_binary_restarts_an_older_process_only_at_its_owned_path() {
        let m = manifest(Platform::Macos);
        let old = json!({"version":"older-installed-version"});
        let current = json!({"version":env!("CARGO_PKG_VERSION")});
        assert!(needs_version_restart(&m, &old, &m.executable));
        assert!(!needs_version_restart(&m, &current, &m.executable));
        assert!(!needs_version_restart(
            &m,
            &old,
            Path::new("/another/checkout/gobstopper")
        ));
        assert!(needs_version_restart(&m, &json!({}), &m.executable));
    }
    fn drain_fixture(stage: &str) -> (Manifest, Paths, DrainOperation) {
        let m = manifest(Platform::Macos);
        let root = std::env::temp_dir().join(format!("gobstopper-drain-{}", unique_id()));
        let p = Paths {
            manifest: root.join("manifest.json"),
            definition: root.join("definition"),
            log: root.join("log"),
            root,
        };
        let op = DrainOperation {
            schema: 1,
            service_id: m.service_id.clone(),
            executable: m.executable.clone(),
            definition_sha256: m.definition_sha256.clone(),
            port: m.port,
            pid: 42,
            instance_id: "incarnation".into(),
            owner: "0123456789abcdef".into(),
            epoch: 3,
            protocol: 1,
            stage: stage.into(),
        };
        (m, p, op)
    }
    #[test]
    fn unacknowledged_external_stop_blocks_stop_and_activation_even_without_listener_or_job() {
        let (mut m, p, op) = drain_fixture("stop_started");
        save_drain(&p, &op).unwrap();
        // Both checks precede all manager commands, process queries or starts.
        assert!(stop(&m, &p)
            .unwrap_err()
            .to_string()
            .contains("no durable acknowledgement"));
        assert!(activate(&mut m, &p)
            .unwrap_err()
            .to_string()
            .contains("no service was started"));
        assert_eq!(load_drain(&m, &p).unwrap().unwrap().stage, "stop_started");
        fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn respawn_rejects_committed_and_damaged_journals_but_allows_waiting_recovery() {
        let (m, p, mut op) = drain_fixture("waiting");
        save_drain(&p, &op).unwrap();
        assert!(check_startup_journal(&m, &p).is_ok());
        for stage in [
            "commit_intent",
            "committed",
            "stop_started",
            "stop_acknowledged",
            "unknown",
        ] {
            op.stage = stage.into();
            save_drain(&p, &op).unwrap();
            assert!(check_startup_journal(&m, &p).is_err(), "{stage}");
        }
        write_atomic(&drain_path(&p), b"broken").unwrap();
        assert!(check_startup_journal(&m, &p).is_err());
        clear_drain(&p).unwrap();
        assert!(check_startup_journal(&m, &p).is_ok());
        fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn startup_guard_uses_the_pinned_state_directory_and_saved_process_identity() {
        let (mut m, p, mut op) = drain_fixture("waiting");
        m.state_dir = Some(p.root.clone());
        m.executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        op.executable = m.executable.clone();
        write_atomic(&p.manifest, &serde_json::to_vec(&m).unwrap()).unwrap();
        save_drain(&p, &op).unwrap();
        assert!(check_startup_drain(Some(&m.service_id), Some(&p.root), m.port).is_ok());
        assert!(check_startup_drain(Some("another"), Some(&p.root), m.port).is_err());
        assert!(check_startup_drain(Some(&m.service_id), Some(&p.root), m.port + 1).is_err());
        assert!(check_startup_drain(None, Some(&p.root), m.port).is_err());
        op.stage = "commit_intent".into();
        save_drain(&p, &op).unwrap();
        assert!(check_startup_drain(Some(&m.service_id), Some(&p.root), m.port).is_err());
        fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn managed_definition_and_ownership_pin_the_same_startup_state_path() {
        for platform in [Platform::Macos, Platform::Linux, Platform::Windows] {
            let mut m = manifest(platform);
            m.state_dir = Some(PathBuf::from("/private/service state"));
            assert!(definition(&m, Path::new("/tmp/log")).contains("--service-state-dir"));
            assert!(service_arguments(&m).ends_with(&[
                "--service-state-dir".into(),
                "/private/service state".into()
            ]));
        }
    }
    #[test]
    fn all_recorded_stop_stages_require_reconciliation_before_start() {
        let (mut m, p, mut op) = drain_fixture("waiting");
        for stage in [
            "waiting",
            "commit_intent",
            "committed",
            "stop_started",
            "stop_acknowledged",
        ] {
            op.stage = stage.into();
            save_drain(&p, &op).unwrap();
            assert!(activate(&mut m, &p)
                .unwrap_err()
                .to_string()
                .contains("no service was started"));
        }
        clear_drain(&p).unwrap();
        assert!(require_clear_drain(&p).is_ok());
        assert!(load_drain(&m, &p).unwrap().is_none());
        fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn drain_journal_identity_schema_and_stage_are_checked_before_effects() {
        let (m, p, op) = drain_fixture("committed");
        for field in [
            "schema",
            "service_id",
            "executable",
            "port",
            "definition_sha256",
            "stage",
        ] {
            let mut value = serde_json::to_value(&op).unwrap();
            value[field] = if field == "schema" || field == "port" {
                json!(99)
            } else {
                json!("changed")
            };
            write_atomic(&drain_path(&p), &serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(load_drain(&m, &p).is_err(), "{field}");
        }
        fs::remove_dir_all(p.root).unwrap();
    }
    #[test]
    fn lease_acknowledgements_bind_every_owner_and_process_field() {
        let (_, _, op) = drain_fixture("waiting");
        let good = json!({"protocol":1,"instance_id":op.instance_id,"pid":op.pid,"owner":op.owner,"epoch":op.epoch,"active_inference":0});
        assert!(validate_lease_reply(&op, &good).is_ok());
        for field in [
            "protocol",
            "instance_id",
            "pid",
            "owner",
            "epoch",
            "active_inference",
        ] {
            let mut bad = good.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(validate_lease_reply(&op, &bad).is_err(), "{field}");
        }
    }
    #[test]
    fn readiness_rejects_paused_or_old_instances_after_upgrade() {
        let m = manifest(Platform::Macos);
        let mut live = json!({"name":"gobstopper-proxy","port":8260,"service_id":"aabbcc","pid":42,"executable":"/opt/Gob Stopper/gobstopper","version":env!("CARGO_PKG_VERSION"),"draining":false});
        assert!(ready_identity(&m, &live, &m.executable));
        live["draining"] = json!(true);
        assert!(!ready_identity(&m, &live, &m.executable));
        live["draining"] = json!(false);
        live["version"] = json!("old");
        assert!(!ready_identity(&m, &live, &m.executable));
    }
    #[test]
    fn restarted_process_does_not_inherit_previous_controller_authority() {
        let (m, _, op) = drain_fixture("committed");
        let mut live = json!({"name":"gobstopper-proxy","port":8260,"service_id":"aabbcc","pid":42,"executable":"/opt/Gob Stopper/gobstopper","instance_id":"incarnation"});
        assert!(same_drain_process(&m, &op, &live));
        live["instance_id"] = json!("new-incarnation");
        assert!(!same_drain_process(&m, &op, &live));
        live["instance_id"] = json!("incarnation");
        live["pid"] = json!(43);
        assert!(!same_drain_process(&m, &op, &live));
    }
    #[test]
    fn service_files_write_atomically_and_refuse_symlinks() {
        let dir = std::env::temp_dir().join(format!("gobstopper-service-{}", unique_id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("manifest.json");
        write_atomic(&file, b"before").unwrap();
        write_atomic(&file, b"after").unwrap();
        assert_eq!(read_file(&file).unwrap().unwrap(), b"after");
        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(read_file(&link).is_err());
        }
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn operation_lock_releases_without_deleting_custody_file() {
        let dir = std::env::temp_dir().join(format!("gobstopper-lock-{}", unique_id()));
        let p = Paths {
            root: dir.clone(),
            manifest: dir.join("m"),
            definition: dir.join("d"),
            log: dir.join("l"),
        };
        let first = ServiceLock::acquire(&p).unwrap();
        assert!(ServiceLock::acquire(&p).is_err());
        drop(first);
        assert!(ServiceLock::acquire(&p).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn loaded_job_identity_rejects_extra_or_changed_arguments() {
        let mut m = manifest(Platform::Macos);
        let p = Paths {
            root: "/tmp/service".into(),
            manifest: "/tmp/manifest.json".into(),
            definition: "/tmp/agent.plist".into(),
            log: "/tmp/log".into(),
        };
        let text = format!(
            "path = {}\nprogram = {}\narguments = {{\n{}\n}}\n",
            p.definition.display(),
            m.executable.display(),
            service_arguments(&m).join("\n")
        );
        assert!(verify_job_bytes(&m, &p, text.as_bytes()).is_ok());
        m.serve_args[1] = "8261".into();
        assert!(verify_job_bytes(&m, &p, text.as_bytes()).is_err());
        m.serve_args[1] = "8260".into();
        assert!(verify_job_bytes(
            &m,
            &p,
            text.replace("\n}\n", "\n--another-flag\n}\n").as_bytes()
        )
        .is_err());
        assert!(verify_job_bytes(
            &m,
            &p,
            text.replace("program = /opt", "program = /elsewhere")
                .as_bytes()
        )
        .is_err());
    }
    #[test]
    fn systemd_identity_compares_structured_argv_without_flattening() {
        let m = manifest(Platform::Linux);
        let p = Paths {
            root: "/tmp/service".into(),
            manifest: "/tmp/manifest.json".into(),
            definition: "/tmp/gobstopper-proxy.service".into(),
            log: "/tmp/log".into(),
        };
        // busctl get-property serializes its variant's array directly as
        // data, preserving nested argument arrays (systemd v257 busctl.c,
        // json_transform_variant / json_transform_array_or_struct).
        let valid = json!({
            "unit":format!("LoadState=loaded\nFragmentPath={}\nActiveState=active\n", p.definition.display()),
            "exec_start":{"type":"a(sasbttttuii)","data":[[
                m.executable, service_arguments(&m), false, 0, 0, 0, 0, 42, 0, 0
            ]]}
        });
        let verify = |value: &Value| verify_job_bytes(&m, &p, &serde_json::to_vec(value).unwrap());
        assert!(verify(&valid).is_ok());
        let mut changed = valid.clone();
        changed["exec_start"]["data"][0][1][4] = json!("8261");
        assert!(verify(&changed).is_err());
        changed = valid.clone();
        changed["exec_start"]["data"][0][1]
            .as_array_mut()
            .unwrap()
            .push(json!("--strict"));
        assert!(verify(&changed).is_err());
        changed = valid.clone();
        // The human-readable argv[] would look identical for this change.
        changed["exec_start"]["data"][0][1] = json!(service_arguments(&m)
            .join(" ")
            .split(' ')
            .collect::<Vec<_>>());
        assert!(verify(&changed).is_err());
        changed = valid.clone();
        changed["exec_start"]["data"][0][2] = json!(true);
        assert!(verify(&changed).is_err());
        changed = valid.clone();
        let extra = changed["exec_start"]["data"][0].clone();
        changed["exec_start"]["data"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        assert!(verify(&changed).is_err());
        changed = valid.clone();
        changed["exec_start"]["data"][0][0] = json!("/elsewhere/opt/Gob Stopper/gobstopper");
        assert!(verify(&changed).is_err());
        assert!(verify_job_bytes(&m, &p, b"ExecStart={ flattened output }").is_err());
    }
    #[test]
    fn service_control_requires_exact_success_and_confirmation() {
        let response = |code: u16, body: &str| {
            format!(
                "HTTP/1.1 {code} response\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        };
        assert!(
            verify_control_reply(response(200, r#"{"drained":true}"#).as_bytes(), true).is_ok()
        );
        assert!(
            verify_control_reply(response(200, r#"{"drained":false}"#).as_bytes(), false).is_ok()
        );
        for (code, body) in [
            (409, r#"{"drained":true}"#),
            (200, "{}"),
            (200, r#"{"drained":false}"#),
            (200, r#"{"drained":"true"}"#),
        ] {
            assert!(verify_control_reply(response(code, body).as_bytes(), true).is_err());
        }
        assert!(verify_control_reply(b"HTTP/1.1 200 okay", true).is_err());
        let mut invalid = manifest(Platform::Macos);
        invalid.service_id = "header\r\nother: value".into();
        assert!(service_control(&invalid, true)
            .unwrap_err()
            .to_string()
            .contains("control header"));
    }
    #[test]
    fn definition_ownership_preserves_changed_files_and_immutable_snapshot() {
        let dir = std::env::temp_dir().join(format!("gobstopper-owned-{}", unique_id()));
        fs::create_dir_all(&dir).unwrap();
        let p = Paths {
            root: dir.clone(),
            manifest: dir.join("m"),
            definition: dir.join("definition"),
            log: dir.join("log"),
        };
        let mut m = manifest(Platform::Macos);
        m.rendered_definition = definition(&m, &p.log);
        m.definition_sha256 = digest(m.rendered_definition.as_bytes());
        assert!(verify_definition(&m, &p, true).is_ok());
        assert!(verify_definition(&m, &p, false).is_err());
        write_atomic(&p.definition, m.rendered_definition.as_bytes()).unwrap();
        assert!(verify_definition(&m, &p, false).is_ok());
        write_atomic(&p.definition, b"external edit").unwrap();
        assert!(verify_definition(&m, &p, true).is_err());
        assert_eq!(read_file(&p.definition).unwrap().unwrap(), b"external edit");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn windows_pending_registration_reconciles_only_saved_initial_identity() {
        let candidate = manifest(Platform::Windows);
        let p = Paths {
            root: "/tmp/service".into(),
            manifest: "/tmp/manifest.json".into(),
            definition: "/tmp/task.xml".into(),
            log: "/tmp/log".into(),
        };
        let registered = b"<Task>scheduler-normalized owned definition</Task>";
        let mut current = candidate.clone();
        assert!(!completed_registration(&candidate, &current));
        current.registered_sha256 = Some(digest(registered));
        assert!(!same_manifest(Some(&candidate), Some(&current)));
        assert!(completed_registration(&candidate, &current));
        assert!(verify_job_bytes(&current, &p, registered).is_ok());
        assert!(verify_job_bytes(&current, &p, b"a different registered task").is_err());
        // Replacing an already-recorded identity is not an initialization.
        assert!(!completed_registration(&current, &current));
        for invalid in [String::new(), "g".repeat(64), "a".repeat(63)] {
            current.registered_sha256 = Some(invalid);
            assert!(!completed_registration(&candidate, &current));
        }
    }
    #[test]
    fn windows_pending_registration_preserves_every_other_manifest_field() {
        let candidate = manifest(Platform::Windows);
        let mut current = candidate.clone();
        current.registered_sha256 = Some(digest(b"registered task"));
        let completed = serde_json::to_value(&current).unwrap();
        let changes = [
            ("schema", json!(SCHEMA + 1)),
            ("platform", json!("linux")),
            ("service_id", json!("another-service")),
            ("state_dir", json!("/another/state-directory")),
            ("executable", json!("/another/gobstopper")),
            ("serve_args", json!(["--port", "8261"])),
            ("port", json!(8261)),
            ("definition_sha256", json!(digest(b"another definition"))),
            ("rendered_definition", json!("another definition")),
            ("task_user", json!("S-1-5-21-456")),
            ("working_directory", json!("/another/directory")),
            ("environment", json!({"ANOTHER_VARIABLE": "value"})),
        ];
        assert_eq!(changes.len() + 1, completed.as_object().unwrap().len());
        for (field, changed) in changes {
            let mut value = completed.clone();
            value[field] = changed;
            let modified: Manifest = serde_json::from_value(value).unwrap();
            assert!(!completed_registration(&candidate, &modified), "{field}");
        }
        let mut non_windows = candidate.clone();
        non_windows.platform = Platform::Macos;
        current.platform = Platform::Macos;
        assert!(!completed_registration(&non_windows, &current));
    }
    #[test]
    fn recovery_preserves_external_definition_changes_without_manager_access() {
        let dir = std::env::temp_dir().join(format!("gobstopper-recovery-{}", unique_id()));
        fs::create_dir_all(&dir).unwrap();
        let p = Paths {
            root: dir.clone(),
            manifest: dir.join("m"),
            definition: dir.join("d"),
            log: dir.join("l"),
        };
        let mut candidate = manifest(Platform::current().unwrap());
        candidate.rendered_definition = definition(&candidate, &p.log);
        candidate.definition_sha256 = digest(candidate.rendered_definition.as_bytes());
        write_pending(&p, None, &candidate).unwrap();
        write_atomic(&p.definition, b"external-change").unwrap();
        assert!(recover_pending(&p)
            .unwrap_err()
            .to_string()
            .contains("outside the pending operation"));
        assert_eq!(
            read_file(&p.definition).unwrap().unwrap(),
            b"external-change"
        );
        assert!(pending_path(&p).exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn migration_keeps_observed_tail_defaults_in_the_saved_arguments() {
        // v0.7.2 defaulted to 40 with no flag; v0.7.3+ defaults to 0.
        // Saving the argv alone would silently change this running service.
        for percent in [0, 40, 60] {
            let mut m = manifest(Platform::Macos);
            let original = m.serve_args.clone();
            m.serve_args = legacy_effective_args(
                &original,
                &json!({"version":"0.7.2","keep_tail_percent":percent}),
            )
            .unwrap();
            assert_eq!(&m.serve_args[..original.len()], original);
            let saved: Manifest = serde_json::from_slice(&serde_json::to_vec(&m).unwrap()).unwrap();
            let rendered = definition(&saved, Path::new("/tmp/gobstopper.log"));
            assert!(rendered
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .windows(2)
                .any(|lines| lines[0] == "<string>--keep-tail-percent</string>"
                    && lines[1] == format!("<string>{percent}</string>")));
            assert_eq!(
                legacy_effective_args(&saved.serve_args, &json!({"keep_tail_percent":12})).unwrap(),
                saved.serve_args
            );
        }
    }
    #[test]
    fn migration_preserves_both_explicit_tail_argument_forms() {
        for args in [
            strings(&["--port", "8260", "--keep-tail-percent", "12"]),
            strings(&["--keep-tail-percent=0", "--strict"]),
        ] {
            for live in [
                json!({"keep_tail_percent":40}),
                json!({"keep_tail_percent":"unused"}),
                json!({}),
            ] {
                assert_eq!(legacy_effective_args(&args, &live).unwrap(), args);
            }
        }
    }
    #[test]
    fn migration_rejects_invalid_observed_tail_and_does_not_invent_missing_settings() {
        let args = strings(&["--threshold=128000", "--keep-recent", "3"]);
        for invalid in [
            json!(61),
            json!(-1),
            json!(40.0),
            json!("40"),
            json!(true),
            json!(null),
            json!([]),
            json!({}),
            json!(u64::MAX),
        ] {
            assert!(
                legacy_effective_args(&args, &json!({"keep_tail_percent":invalid}))
                    .unwrap_err()
                    .to_string()
                    .contains("legacy keep_tail_percent")
            );
        }
        assert_eq!(
            legacy_effective_args(&args, &json!({"version":"old"})).unwrap(),
            args
        );
    }
    fn legacy_fixture(label: &str) -> (Manifest, LegacySnapshot, Value) {
        let mut candidate = manifest(Platform::Macos);
        let mut arguments = vec![
            candidate.executable.display().to_string(),
            "proxy".into(),
            "serve".into(),
        ];
        arguments.extend(candidate.serve_args.clone());
        let original = json!({"Label":label,"ProgramArguments":arguments,"RunAtLoad":true,"KeepAlive":{"SuccessfulExit":false},"ThrottleInterval":30,"WorkingDirectory":"/a & b","EnvironmentVariables":{"HOME":"/user","PATH":"/bin\r/custom"},"StandardOutPath":"/old/log"});
        candidate.serve_args =
            legacy_effective_args(&candidate.serve_args, &json!({"keep_tail_percent":40})).unwrap();
        let restoration =
            prepare_legacy_restoration(&original, &arguments, &candidate.serve_args).unwrap();
        let snapshot = LegacySnapshot {
            path: PathBuf::from(format!("/tmp/{label}.plist")),
            label: label.into(),
            bytes: format!(
                "<plist version=\"1.0\">{}</plist>",
                plist_value(&original).unwrap()
            )
            .into_bytes(),
            arguments,
            restoration,
        };
        (candidate, snapshot, original)
    }
    fn legacy_job_output(snapshot: &LegacySnapshot, identity: LegacyIdentity) -> Vec<u8> {
        let args = if identity == LegacyIdentity::Original {
            &snapshot.arguments
        } else {
            &snapshot.restoration.as_ref().unwrap().arguments
        };
        format!(
            "path = {}\nprogram = {}\npid = 123\narguments = {{\n{}\n}}\n",
            snapshot.path.display(),
            args[0],
            args.join("\n")
        )
        .into_bytes()
    }

    #[test]
    fn legacy_restoration_keeps_original_and_rejects_tampered_prepared_state() {
        let (mut candidate, mut snapshot, original) = legacy_fixture(LEGACY_LABEL);
        let original_bytes = snapshot.bytes.clone();
        let original_args = snapshot.arguments.clone();
        validate_legacy_restoration(&candidate, &snapshot, &original).unwrap();
        let saved = serde_json::to_vec(&snapshot).unwrap();
        let recovered: LegacySnapshot = serde_json::from_slice(&saved).unwrap();
        assert_eq!(recovered.bytes, original_bytes);
        assert_eq!(recovered.arguments, original_args);
        assert_eq!(recovered.restoration, snapshot.restoration);
        snapshot.restoration.as_mut().unwrap().bytes.push(b' ');
        assert!(validate_legacy_restoration(&candidate, &snapshot, &original).is_err());
        snapshot = serde_json::from_slice(&saved).unwrap();
        snapshot.restoration.as_mut().unwrap().arguments[0] = "/another/program".into();
        assert!(validate_legacy_restoration(&candidate, &snapshot, &original).is_err());
        snapshot = serde_json::from_slice(&saved).unwrap();
        candidate.serve_args.insert(0, "--strict".into());
        assert!(validate_legacy_restoration(&candidate, &snapshot, &original).is_err());
        assert_eq!(snapshot.bytes, original_bytes);
        assert_eq!(snapshot.arguments, original_args);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn legacy_prepared_plist_roundtrips_all_original_settings_except_effective_arguments() {
        let (candidate, snapshot, mut original) = legacy_fixture(LEGACY_LABEL);
        assert_eq!(parse_legacy(&snapshot.bytes).unwrap(), original);
        let prepared = snapshot.restoration.as_ref().unwrap();
        original["ProgramArguments"] = json!(prepared.arguments);
        assert_eq!(parse_legacy(&prepared.bytes).unwrap(), original);
        assert_eq!(&prepared.arguments[3..], candidate.serve_args);
        assert!(plist_value(&json!("\u{0000}")).is_err());
        assert!(plist_value(&json!(null)).is_err());
    }

    #[test]
    fn older_legacy_journals_decode_without_inventing_a_restoration() {
        let (mut candidate, snapshot, original) = legacy_fixture(LEGACY_LABEL);
        candidate.serve_args = snapshot.arguments[3..].to_vec();
        let mut old = serde_json::to_value(&snapshot).unwrap();
        old.as_object_mut().unwrap().remove("restoration");
        let recovered: LegacySnapshot = serde_json::from_value(old).unwrap();
        assert!(recovered.restoration.is_none());
        validate_legacy_restoration(&candidate, &recovered, &original).unwrap();
        assert_eq!(
            legacy_recovery_plan(false, None, false, false).unwrap(),
            LegacyRecoveryPlan::Restore(LegacyIdentity::Original)
        );
    }

    #[test]
    fn legacy_loaded_identities_and_health_are_checked_before_recovery_completes() {
        let (candidate, snapshot, _) = legacy_fixture(LEGACY_LABEL);
        for identity in [LegacyIdentity::Original, LegacyIdentity::Restoration] {
            let bytes = legacy_job_output(&snapshot, identity);
            let loaded = legacy_job_identity(&snapshot, &bytes).unwrap();
            assert_eq!(loaded.identity, identity);
            assert_eq!(loaded.pid, Some(123));
            let mut live = json!({"name":"gobstopper-proxy","port":candidate.port,"keep_awake":{"active_inference":99}});
            if identity == LegacyIdentity::Original {
                // Old status needs no pid/executable fields but its known
                // retained-history setting must still match the snapshot.
                live["keep_tail_percent"] = json!(40);
                assert!(legacy_ready(&candidate, &snapshot, &loaded, &live));
                live["keep_tail_percent"] = json!(0);
                assert!(!legacy_ready(&candidate, &snapshot, &loaded, &live));
            } else {
                assert!(!legacy_ready(&candidate, &snapshot, &loaded, &live));
                live["pid"] = json!(123);
                live["executable"] = json!(candidate.executable);
                live["keep_tail_percent"] = json!(40);
                assert!(legacy_ready(&candidate, &snapshot, &loaded, &live));
                for (key, bad) in [
                    ("pid", json!(124)),
                    ("executable", json!("/another/program")),
                    ("keep_tail_percent", json!(0)),
                    ("service_id", json!(candidate.service_id)),
                ] {
                    let mut changed = live.clone();
                    changed[key] = bad;
                    assert!(
                        !legacy_ready(&candidate, &snapshot, &loaded, &changed),
                        "{key}"
                    );
                }
            }
            let text = String::from_utf8(bytes).unwrap();
            assert!(legacy_job_identity(
                &snapshot,
                text.replace("path = /tmp/", "path = /other/").as_bytes()
            )
            .is_err());
            assert!(legacy_job_identity(
                &snapshot,
                text.replace("program = ", "program = /other/").as_bytes()
            )
            .is_err());
            assert!(legacy_job_identity(
                &snapshot,
                text.replace("\n8260\n", "\n8261\n").as_bytes()
            )
            .is_err());
        }
    }

    #[test]
    fn legacy_recovery_preserves_loaded_processes_across_crash_boundaries() {
        for identity in [LegacyIdentity::Original, LegacyIdentity::Restoration] {
            assert_eq!(
                legacy_recovery_plan(false, Some(identity), false, true).unwrap(),
                if identity == LegacyIdentity::Original {
                    LegacyRecoveryPlan::RestartOriginal
                } else {
                    LegacyRecoveryPlan::Preserve(identity)
                }
            );
            assert!(legacy_recovery_plan(true, Some(identity), true, true).is_err());
            assert!(legacy_recovery_plan(true, Some(identity), false, true).is_err());
        }
        assert_eq!(
            legacy_recovery_plan(false, Some(LegacyIdentity::Original), false, false).unwrap(),
            LegacyRecoveryPlan::Preserve(LegacyIdentity::Original)
        );
        // After old stop, candidate file/write/start failure, and restoration
        // write before bootstrap all require the prepared effective arguments.
        for new_job in [false, true] {
            assert_eq!(
                legacy_recovery_plan(new_job, None, false, true).unwrap(),
                LegacyRecoveryPlan::Restore(LegacyIdentity::Restoration)
            );
        }
        // A crash after healthy candidate startup must not interrupt inference.
        assert_eq!(
            legacy_recovery_plan(true, None, true, true).unwrap(),
            LegacyRecoveryPlan::CompleteCandidate
        );
    }

    #[test]
    fn legacy_original_restart_requires_matching_process_and_proven_idle_activity() {
        let (candidate, snapshot, _) = legacy_fixture(LEGACY_LABEL);
        let loaded = legacy_job_identity(
            &snapshot,
            &legacy_job_output(&snapshot, LegacyIdentity::Original),
        )
        .unwrap();
        let idle = json!({"name":"gobstopper-proxy","port":8260,"active_connections":1,"keep_tail_percent":40});
        legacy_restart_guard(&candidate, &loaded, &idle).unwrap();
        for (key, value) in [
            ("active_connections", json!(2)),
            ("active_connections", json!(null)),
            ("active_connections", json!("0")),
            ("pid", json!(124)),
            ("name", json!("foreign")),
            ("port", json!(8261)),
            ("keep_awake", json!({"active_inference":1})),
            ("keep_awake", json!({"active_inference":"0"})),
            ("keep_awake", json!({})),
        ] {
            let mut changed = idle.clone();
            changed[key] = value;
            assert!(
                legacy_restart_guard(&candidate, &loaded, &changed).is_err(),
                "{key}"
            );
        }
        // Already respawned under the new implicit default: an idle process can
        // be repaired to the recorded 40%, but it cannot pass readiness as-is.
        let mut reset = idle;
        reset["keep_tail_percent"] = json!(0);
        assert!(!legacy_ready(&candidate, &snapshot, &loaded, &reset));
        legacy_restart_guard(&candidate, &loaded, &reset).unwrap();
    }

    #[test]
    fn legacy_recovery_admits_only_recorded_files_at_each_crash_boundary() {
        for label in [LABEL, LEGACY_LABEL] {
            let dir =
                std::env::temp_dir().join(format!("gobstopper-legacy-recovery-{}", unique_id()));
            fs::create_dir_all(&dir).unwrap();
            let p = Paths {
                root: dir.clone(),
                manifest: dir.join("manifest.json"),
                definition: dir.join(format!("{LABEL}.plist")),
                log: dir.join("log"),
            };
            let (mut candidate, mut legacy, _) = legacy_fixture(label);
            legacy.path = dir.join(format!("{label}.plist"));
            candidate.rendered_definition = definition(&candidate, &p.log);
            candidate.definition_sha256 = digest(candidate.rendered_definition.as_bytes());
            let stages = [
                Some(legacy.bytes.clone()),
                None,
                Some(candidate.rendered_definition.as_bytes().to_vec()),
                Some(legacy.restoration.as_ref().unwrap().bytes.clone()),
            ];
            for bytes in stages {
                for path in [&p.definition, &legacy.path, &p.manifest] {
                    if path.exists() {
                        fs::remove_file(path).unwrap();
                    }
                }
                if let Some(bytes) = bytes {
                    let dest = if bytes == candidate.rendered_definition.as_bytes() {
                        &p.definition
                    } else {
                        &legacy.path
                    };
                    write_atomic(dest, &bytes).unwrap();
                }
                // Interrupted manifest removal is also a recognized boundary.
                for manifest_present in [false, true] {
                    if manifest_present {
                        write_atomic(&p.manifest, &serde_json::to_vec(&candidate).unwrap())
                            .unwrap();
                    }
                    verify_legacy_files(&p, &candidate, &legacy).unwrap();
                }
            }
            write_atomic(&legacy.path, b"external edit").unwrap();
            assert!(verify_legacy_files(&p, &candidate, &legacy).is_err());
            assert_eq!(read_file(&legacy.path).unwrap().unwrap(), b"external edit");
            write_atomic(&legacy.path, &legacy.bytes).unwrap();
            let mut foreign = candidate.clone();
            foreign.service_id = "different".into();
            write_atomic(&p.manifest, &serde_json::to_vec(&foreign).unwrap()).unwrap();
            assert!(verify_legacy_files(&p, &candidate, &legacy).is_err());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn migration_recovery_rejects_unrecognized_definition_target() {
        let candidate = manifest(Platform::Macos);
        let p = Paths {
            root: "/tmp/service".into(),
            manifest: "/tmp/m".into(),
            definition: "/tmp/d".into(),
            log: "/tmp/l".into(),
        };
        let snapshot = LegacySnapshot {
            path: "/tmp/unrelated.plist".into(),
            label: LEGACY_LABEL.into(),
            bytes: vec![],
            arguments: vec![],
            restoration: None,
        };
        assert!(recover_legacy(&p, &candidate, &snapshot)
            .unwrap_err()
            .to_string()
            .contains("target is invalid"));
    }
}

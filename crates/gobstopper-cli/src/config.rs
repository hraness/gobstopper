//! Configuration: layered policy resolution and userspace presets.
//!
//! Resolution order (later wins):
//!   built-in default -> ~/.config/gobstopper/config.toml [policy]
//!   -> [provider.<name>] -> named [presets.<name>] -> [sessions."<id>"]
//!
//! A preset may also carry `command`: a userspace program that receives
//! the normalized transcript as JSON on stdin and returns an array of
//! `Edit` objects on stdout. That is the custom-code escape hatch —
//! strategies stay deterministic, experiments live in user space.

use gobstopper_core::strategy::{PolicyConfig, QuotaPressure};
use gobstopper_core::Provider;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyPatch {
    pub strategy: Option<String>,
    pub trigger_tokens: Option<u64>,
    pub floor_tokens: Option<u64>,
    pub keep_recent_tool_outputs: Option<usize>,
    pub min_interval_secs: Option<u64>,
    /// Provider quota pressure: `low` compacts later, `high` earlier.
    pub quota_pressure: Option<QuotaPressure>,
    /// Userspace program: transcript JSON in, `Edit[]` JSON out.
    pub command: Option<String>,
    pub trusted_legacy_command: Option<bool>,
    pub plugin: Option<PluginSelection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSelection {
    pub manifest: PathBuf,
    pub trusted_sha256: String,
}

impl PolicyPatch {
    fn apply(&self, policy: &mut PolicyConfig) {
        if let Some(v) = self.trigger_tokens {
            policy.trigger_tokens = v;
        }
        if let Some(v) = self.floor_tokens {
            policy.floor_tokens = v;
        }
        if let Some(v) = self.keep_recent_tool_outputs {
            policy.keep_recent_tool_outputs = v;
        }
        if let Some(v) = self.min_interval_secs {
            policy.min_interval_secs = v;
        }
        if let Some(v) = self.quota_pressure {
            policy.quota_pressure = v;
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub policy: PolicyPatch,
    pub provider: BTreeMap<String, PolicyPatch>,
    pub presets: BTreeMap<String, PolicyPatch>,
    pub sessions: BTreeMap<String, PolicyPatch>,
}

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))
        })
        .unwrap_or_default()
        .join("gobstopper/config.toml")
}

pub fn load() -> anyhow::Result<Config> {
    use std::io::Read;
    let file = match std::fs::File::open(config_path()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 64 * 1024 { anyhow::bail!("configuration exceeds byte limit"); }
    parse(&text)
}

pub fn parse(text: &str) -> anyhow::Result<Config> {
    let config: Config = toml::from_str(text).map_err(|_| anyhow::anyhow!("invalid configuration syntax, field or type"))?;
    if config.provider.keys().any(|p| !matches!(p.as_str(), "codex" | "claude_code")) {
        anyhow::bail!("unknown provider configuration key; expected codex or claude_code");
    }
    Ok(config)
}

/// A fully resolved policy plus the strategy id and optional preset command.
pub struct Resolved {
    pub policy: PolicyConfig,
    pub strategy: String,
    pub command: Option<String>,
    pub trusted_legacy_command: bool,
    pub plugin: Option<PluginSelection>,
}

impl Config {
    pub fn resolve(
        &self,
        provider: Provider,
        session_id: &str,
        preset: Option<&str>,
        strategy_flag: Option<&str>,
    ) -> Result<Resolved, anyhow::Error> {
        let mut policy = PolicyConfig::default();
        let mut strategy = "auto".to_string();
        let mut command = None;
        let mut plugin = None;
        let mut trusted_legacy_command = false;
        let preset_patch = preset.map(|name| self.presets.get(name).ok_or_else(|| anyhow::anyhow!("unknown preset"))).transpose()?;
        for patch in [Some(&self.policy), self.provider.get(provider.as_str()), preset_patch, self.sessions.get(session_id)].into_iter().flatten() {
            patch.apply(&mut policy);
            if let Some(value) = &patch.strategy { strategy = value.clone(); }
            if patch.command.is_some() && patch.plugin.is_some() { anyhow::bail!("choose command or plugin, not both"); }
            if let Some(value) = &patch.command { command = Some(value.clone()); plugin = None; trusted_legacy_command = false; }
            if let Some(value) = &patch.plugin { plugin = Some(value.clone()); command = None; }
            if let Some(value) = patch.trusted_legacy_command { trusted_legacy_command = value; }
        }
        if let Some(flag) = strategy_flag {
            strategy = flag.to_string(); command = None; plugin = None;
        }
        if gobstopper_core::strategy::strategy_by_id(&strategy).is_none() {
            anyhow::bail!("unknown strategy");
        }
        if policy.trigger_tokens == 0 || policy.trigger_tokens > 10_000_000
            || policy.floor_tokens >= policy.trigger_tokens || policy.keep_recent_tool_outputs > 100_000
            || policy.min_interval_secs > 86400 {
            anyhow::bail!("invalid policy bounds: require 0 <= floor < trigger <= 10000000");
        }
        if command.is_some() && !trusted_legacy_command { anyhow::bail!("legacy command requires trusted_legacy_command=true; prefer an exact-identity plugin"); }
        if let Some(selection) = &plugin {
            if !selection.manifest.is_absolute() || selection.trusted_sha256.len() != 64
                || !selection.trusted_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                anyhow::bail!("plugin requires an absolute manifest path and exact trusted SHA-256");
            }
        }
        Ok(Resolved { policy, strategy, command, trusted_legacy_command, plugin })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_unknown_configuration_fails() {
        assert!(parse("[policy\ntrigger_tokens = 1").is_err());
        assert!(parse("[policy]\ntriger_tokens = 1").is_err());
        assert!(parse("[provider.claude]\ntrigger_tokens = 100000").is_err());
    }

    #[test]
    fn layers_and_explicit_strategy_override_are_deterministic() {
        let cfg = parse("[policy]\ntrigger_tokens = 200000\n[provider.codex]\ntrigger_tokens = 180000\n[presets.fast]\ntrigger_tokens = 120000\n[sessions.s]\ntrigger_tokens = 100000\ncommand = 'custom'\ntrusted_legacy_command = true").unwrap();
        let resolved = cfg.resolve(Provider::Codex, "s", Some("fast"), Some("elide")).unwrap();
        assert_eq!(resolved.policy.trigger_tokens, 100000);
        assert_eq!(resolved.strategy, "elide");
        assert!(resolved.command.is_none());
        assert!(cfg.resolve(Provider::Codex, "s", Some("unknown"), None).is_err());
    }

    #[test]
    fn legacy_code_needs_trust_and_invalid_policy_is_rejected() {
        let cfg = parse("[policy]\ncommand = 'custom'").unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
        let cfg = parse("[policy]\ntrigger_tokens = 10\nfloor_tokens = 10").unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
    }
}

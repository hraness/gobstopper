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
    /// Scored-only opt-in retention cutoff; independent of the size target.
    pub keep_score_threshold: Option<f64>,
    pub min_interval_secs: Option<u64>,
    /// Minimum seconds between in-place mutations of one session
    /// (watch store/in-place applies). Default 1800.
    pub apply_hold_secs: Option<u64>,
    /// Hard ceiling for the prompt-policy hook: at or above this many
    /// context tokens a supported provider hook returns a blocking
    /// decision instead of an advisory. `0` (default) disables blocking.
    pub block_tokens: Option<u64>,
    pub min_savings_tokens: Option<u64>,
    /// `cliff` only: newest assistant steps kept byte-for-byte. Default 3.
    pub keep_recent_turns: Option<usize>,
    /// `cliff` only: older tool results above this many bytes are dropped;
    /// smaller ones stay verbatim. Default 500.
    pub result_max_bytes: Option<u64>,
    /// Provider quota pressure: `low` compacts later, `high` earlier.
    pub quota_pressure: Option<QuotaPressure>,
    /// Derive trigger/floor per session from the provider window and
    /// past compaction yields (see `gobstopper tune`).
    pub adaptive: Option<bool>,
    /// Userspace program: transcript JSON in, `Edit[]` JSON out.
    pub command: Option<String>,
    pub trusted_legacy_command: Option<bool>,
    pub plugin: Option<PluginSelection>,
    /// Legacy compatibility flag. Direct provider-store writes remain
    /// unavailable until compatible lifetime provider custody is qualified.
    pub auto_apply_store: Option<bool>,
    /// Legacy compatibility flag. Direct transcript replacement remains
    /// unavailable until compatible lifetime provider custody is qualified.
    pub auto_apply_inplace: Option<bool>,
    /// Let watch ask a supported Codex, Claude or Devin provider to compact
    /// a closed session natively. Default off. No native outcome falls back
    /// to direct surgery; ownership still requires provider qualification.
    pub auto_compact_closed: Option<bool>,
    /// Devin only: deadline in seconds for one `devin acp` compact
    /// (initialize + session/load replay + /compact + async status).
    /// `session/load` streams the whole node history — ~25k nodes can
    /// exceed 10 minutes — so the default is generous. Default 1800.
    pub acp_timeout_secs: Option<u64>,
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
        if let Some(v) = self.keep_score_threshold {
            policy.keep_score_threshold = Some(v);
        }
        if let Some(v) = self.min_interval_secs {
            policy.min_interval_secs = v;
        }
        if let Some(v) = self.apply_hold_secs {
            policy.apply_hold_secs = v;
        }
        if let Some(v) = self.block_tokens {
            policy.block_tokens = v;
        }
        if let Some(v) = self.min_savings_tokens {
            policy.min_savings_tokens = v;
        }
        if let Some(v) = self.keep_recent_turns {
            policy.keep_recent_turns = v;
        }
        if let Some(v) = self.result_max_bytes {
            policy.result_max_bytes = v;
        }
        if let Some(v) = self.quota_pressure {
            policy.quota_pressure = v;
        }
        if let Some(v) = self.adaptive {
            policy.adaptive = v;
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
    /// Rollout gates for the prompt-policy advisory, keyed by provider id
    /// (`codex`, `claude_code`, `devin`) with a 0-100 percentage. Sessions
    /// are bucketed deterministically by id: `devin = 50` shows the
    /// compaction advisory to a stable half of Devin sessions; the other
    /// half is the control cohort. Absent or 100 = always advise.
    pub rollout: BTreeMap<String, u8>,
}

pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default()
        .join("gobstopper/config.toml")
}

pub fn load() -> anyhow::Result<Config> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(config_path()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(error.into()),
    };
    if !file.metadata()?.is_file() {
        anyhow::bail!("configuration must be a regular file");
    }
    let mut text = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 64 * 1024 {
        anyhow::bail!("configuration exceeds byte limit");
    }
    parse(&text)
}

pub fn validate_policy(policy: &PolicyConfig) -> anyhow::Result<()> {
    gobstopper_core::policy::validate_policy(policy).map_err(anyhow::Error::msg)
}

pub fn parse(text: &str) -> anyhow::Result<Config> {
    let config: Config = toml::from_str(text)
        .map_err(|_| anyhow::anyhow!("invalid configuration syntax, field or type"))?;
    for key in config.provider.keys().chain(config.rollout.keys()) {
        if !matches!(key.as_str(), "codex" | "claude_code" | "devin") {
            anyhow::bail!(
                "unknown provider configuration key; expected codex, claude_code, or devin"
            );
        }
    }
    if config.rollout.values().any(|pct| *pct > 100) {
        anyhow::bail!("rollout percentages must be between 0 and 100");
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
    /// See `PolicyPatch::auto_apply_store`.
    pub auto_apply_store: bool,
    /// See `PolicyPatch::auto_apply_inplace`.
    pub auto_apply_inplace: bool,
    /// See `PolicyPatch::auto_compact_closed`.
    pub auto_compact_closed: bool,
    /// See `PolicyPatch::acp_timeout_secs`.
    pub acp_timeout_secs: u64,
}

impl Resolved {
    /// Inspection must reject configured executable strategies before any
    /// threshold shortcut. The caller separately chooses the pure evaluation
    /// path, which never resolves environment-selected scorers or digests.
    pub fn ensure_inspection(&self) -> anyhow::Result<()> {
        if self.command.is_some() || self.plugin.is_some() {
            anyhow::bail!("external strategies are unavailable in deterministic inspection");
        }
        Ok(())
    }
}

impl Config {
    pub fn resolve(
        &self,
        provider: Provider,
        session_id: &str,
        preset: Option<&str>,
        strategy_flag: Option<&str>,
    ) -> Result<Resolved, anyhow::Error> {
        self.resolve_provider(provider.as_str(), session_id, preset, strategy_flag)
    }

    pub fn resolve_provider(
        &self,
        provider_id: &str,
        session_id: &str,
        preset: Option<&str>,
        strategy_flag: Option<&str>,
    ) -> Result<Resolved, anyhow::Error> {
        if !matches!(provider_id, "codex" | "claude_code" | "devin") {
            anyhow::bail!("unknown provider");
        }
        let mut policy = PolicyConfig::default();
        let mut strategy = "auto".to_string();
        let mut command = None;
        let mut plugin = None;
        let mut trusted_legacy_command = false;
        let mut auto_apply_store = false;
        let mut auto_apply_inplace = false;
        let mut auto_compact_closed = false;
        let mut acp_timeout_secs = 1800_u64;
        let preset_patch = preset
            .map(|name| {
                self.presets
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("unknown preset"))
            })
            .transpose()?;
        for patch in [
            Some(&self.policy),
            self.provider.get(provider_id),
            preset_patch,
            self.sessions.get(session_id),
        ]
        .into_iter()
        .flatten()
        {
            patch.apply(&mut policy);
            if let Some(value) = &patch.strategy {
                strategy = value.clone();
            }
            if patch.command.is_some() && patch.plugin.is_some() {
                anyhow::bail!("choose command or plugin, not both");
            }
            if let Some(value) = &patch.command {
                command = Some(value.clone());
                plugin = None;
                trusted_legacy_command = false;
            }
            if let Some(value) = &patch.plugin {
                plugin = Some(value.clone());
                command = None;
            }
            if let Some(value) = patch.trusted_legacy_command {
                trusted_legacy_command = value;
            }
            if let Some(value) = patch.auto_apply_store {
                auto_apply_store = value;
            }
            if let Some(value) = patch.auto_apply_inplace {
                auto_apply_inplace = value;
            }
            if let Some(value) = patch.auto_compact_closed {
                auto_compact_closed = value;
            }
            if let Some(value) = patch.acp_timeout_secs {
                acp_timeout_secs = value.clamp(60, 7200);
            }
        }
        if let Some(flag) = strategy_flag {
            strategy = flag.to_string();
            command = None;
            plugin = None;
        }
        if gobstopper_core::strategy::strategy_by_id(&strategy).is_none() {
            anyhow::bail!("unknown strategy");
        }
        validate_policy(&policy)?;
        if command.is_some() && !trusted_legacy_command {
            anyhow::bail!("legacy command requires trusted_legacy_command=true; prefer an exact-identity plugin");
        }
        if let Some(selection) = &plugin {
            if !selection.manifest.is_absolute()
                || selection.trusted_sha256.len() != 64
                || !selection
                    .trusted_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
            {
                anyhow::bail!(
                    "plugin requires an absolute manifest path and exact trusted SHA-256"
                );
            }
        }
        Ok(Resolved {
            policy,
            strategy,
            command,
            trusted_legacy_command,
            plugin,
            auto_apply_store,
            auto_apply_inplace,
            auto_compact_closed,
            acp_timeout_secs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_rejects_external_strategies_without_loading_them() {
        for config in [
            "[policy]\ncommand = 'must-not-run'\ntrusted_legacy_command = true".to_string(),
            format!(
                "[policy.plugin]\nmanifest = '/does/not/exist/plugin.json'\ntrusted_sha256 = '{}'",
                "a".repeat(64)
            ),
        ] {
            let cfg = parse(&config).unwrap();
            assert!(cfg
                .resolve(Provider::Codex, "s", None, None)
                .unwrap()
                .ensure_inspection()
                .is_err());
            // Explicit built-in selection clears external strategy authority.
            assert!(cfg
                .resolve(Provider::Codex, "s", None, Some("scored"))
                .unwrap()
                .ensure_inspection()
                .is_ok());
        }
        assert!(Config::default()
            .resolve(Provider::Codex, "s", None, None)
            .unwrap()
            .ensure_inspection()
            .is_ok());
    }

    #[test]
    fn malformed_and_unknown_configuration_fails() {
        assert!(parse("[policy\ntrigger_tokens = 1").is_err());
        assert!(parse("[policy]\ntriger_tokens = 1").is_err());
        assert!(parse("[provider.claude]\ntrigger_tokens = 100000").is_err());
    }

    #[test]
    fn rollout_percentages_are_validated() {
        let cfg = parse("[rollout]\ndevin = 50\nclaude_code = 0").unwrap();
        assert_eq!(cfg.rollout["devin"], 50);
        assert_eq!(cfg.rollout["claude_code"], 0);
        assert!(parse("[rollout]\ndevin = 101").is_err());
        assert!(parse("[rollout]\nunknown = 50").is_err());
    }

    #[test]
    fn layers_and_explicit_strategy_override_are_deterministic() {
        let cfg = parse("[policy]\ntrigger_tokens = 200000\nmin_savings_tokens = 10000\n[provider.codex]\ntrigger_tokens = 180000\n[presets.fast]\ntrigger_tokens = 120000\nmin_savings_tokens = 20000\n[sessions.s]\ntrigger_tokens = 100000\ncommand = 'custom'\ntrusted_legacy_command = true").unwrap();
        let resolved = cfg
            .resolve(Provider::Codex, "s", Some("fast"), Some("elide"))
            .unwrap();
        assert_eq!(resolved.policy.trigger_tokens, 100000);
        assert_eq!(resolved.policy.min_savings_tokens, 20000);
        assert_eq!(resolved.strategy, "elide");
        assert!(resolved.command.is_none());
        assert!(cfg
            .resolve(Provider::Codex, "s", Some("unknown"), None)
            .is_err());
        let cfg = parse("[provider.devin]\ntrigger_tokens = 90000").unwrap();
        assert_eq!(
            cfg.resolve_provider("devin", "", None, None)
                .unwrap()
                .policy
                .trigger_tokens,
            90000
        );
    }

    #[test]
    fn legacy_code_needs_trust_and_invalid_policy_is_rejected() {
        let cfg = parse("[policy]\ncommand = 'custom'").unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
        let cfg = parse("[policy]\ntrigger_tokens = 10\nfloor_tokens = 10").unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
        let cfg = parse(
            "[policy]\ntrigger_tokens = 100\nfloor_tokens = 10\nmin_savings_tokens = 10000001",
        )
        .unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
    }

    #[test]
    fn scored_retention_cutoff_is_opt_in_layered_and_bounded() {
        assert_eq!(
            Config::default()
                .resolve(Provider::Codex, "s", None, None)
                .unwrap()
                .policy
                .keep_score_threshold,
            None
        );
        let cfg = parse("[policy]\nkeep_score_threshold = 0.7\n[presets.retained]\nstrategy = 'scored'\nkeep_score_threshold = 0.5\n[sessions.s]\nkeep_score_threshold = 0.4").unwrap();
        assert_eq!(
            cfg.resolve(Provider::Codex, "other", None, None)
                .unwrap()
                .policy
                .keep_score_threshold,
            Some(0.7)
        );
        assert_eq!(
            cfg.resolve(Provider::Codex, "other", Some("retained"), None)
                .unwrap()
                .policy
                .keep_score_threshold,
            Some(0.5)
        );
        assert_eq!(
            cfg.resolve(Provider::Codex, "s", Some("retained"), None)
                .unwrap()
                .policy
                .keep_score_threshold,
            Some(0.4)
        );
        for value in ["nan", "inf", "-0.1", "1.1"] {
            let cfg = parse(&format!("[policy]\nkeep_score_threshold = {value}")).unwrap();
            assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
        }
    }

    #[test]
    fn cliff_knobs_default_layer_and_reject_out_of_range_values() {
        let resolved = Config::default()
            .resolve(Provider::Codex, "s", None, Some("cliff"))
            .unwrap();
        assert_eq!(resolved.strategy, "cliff");
        assert_eq!(resolved.policy.keep_recent_turns, 3);
        assert_eq!(resolved.policy.result_max_bytes, 500);
        let cfg = parse(
            "[policy]\nkeep_recent_turns = 5\n[presets.cliff]\nstrategy = 'cliff'\nresult_max_bytes = 1000\n[sessions.s]\nkeep_recent_turns = 1",
        )
        .unwrap();
        let resolved = cfg
            .resolve(Provider::Codex, "s", Some("cliff"), None)
            .unwrap();
        assert_eq!(resolved.strategy, "cliff");
        assert_eq!(resolved.policy.keep_recent_turns, 1);
        assert_eq!(resolved.policy.result_max_bytes, 1000);
        let other = cfg.resolve(Provider::Codex, "other", None, None).unwrap();
        assert_eq!(other.policy.keep_recent_turns, 5);
        assert_eq!(other.policy.result_max_bytes, 500);
        let cfg = parse("[policy]\nkeep_recent_turns = 100001").unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
        let cfg = parse("[policy]\nresult_max_bytes = 10000001").unwrap();
        assert!(cfg.resolve(Provider::Codex, "s", None, None).is_err());
    }

    #[test]
    fn block_ceiling_and_closed_compact_default_off_and_layer() {
        let resolved = Config::default()
            .resolve(Provider::ClaudeCode, "s", None, None)
            .unwrap();
        assert_eq!(resolved.policy.block_tokens, 0);
        assert!(!resolved.auto_compact_closed);
        let cfg = parse(
            "[provider.claude_code]\nblock_tokens = 500000\nauto_compact_closed = true\n[sessions.s]\nblock_tokens = 700000",
        )
        .unwrap();
        let resolved = cfg.resolve(Provider::ClaudeCode, "s", None, None).unwrap();
        assert_eq!(resolved.policy.block_tokens, 700_000);
        assert!(resolved.auto_compact_closed);
        // Other providers don't inherit the claude_code section.
        assert!(
            !cfg.resolve(Provider::Devin, "s", None, None)
                .unwrap()
                .auto_compact_closed
        );
        // Out-of-range ceilings are rejected at resolution.
        let cfg = parse("[policy]\nblock_tokens = 10000001").unwrap();
        assert!(cfg.resolve(Provider::ClaudeCode, "s", None, None).is_err());
    }
}

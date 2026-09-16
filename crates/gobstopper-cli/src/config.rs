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

use gobstopper_core::strategy::PolicyConfig;
use gobstopper_core::Provider;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PolicyPatch {
    pub strategy: Option<String>,
    pub trigger_tokens: Option<u64>,
    pub floor_tokens: Option<u64>,
    pub keep_recent_tool_outputs: Option<usize>,
    pub min_interval_secs: Option<u64>,
    /// Userspace program: transcript JSON in, `Edit[]` JSON out.
    pub command: Option<String>,
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
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
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

pub fn load() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// A fully resolved policy plus the strategy id and optional preset command.
pub struct Resolved {
    pub policy: PolicyConfig,
    pub strategy: String,
    pub command: Option<String>,
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
        self.policy.apply(&mut policy);
        let mut strategy = self.policy.strategy.clone();
        let mut command = self.policy.command.clone();

        if let Some(patch) = self.provider.get(provider.as_str()) {
            patch.apply(&mut policy);
            strategy = patch.strategy.clone().or(strategy);
            command = patch.command.clone().or(command);
        }
        if let Some(name) = preset {
            let patch = self
                .presets
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("unknown preset '{name}'"))?;
            patch.apply(&mut policy);
            strategy = patch.strategy.clone().or(strategy);
            command = patch.command.clone().or(command);
        }
        if let Some(patch) = self.sessions.get(session_id) {
            patch.apply(&mut policy);
            strategy = patch.strategy.clone().or(strategy);
            command = patch.command.clone().or(command);
        }
        if let Some(flag) = strategy_flag {
            strategy = Some(flag.to_string());
        }
        Ok(Resolved {
            policy,
            strategy: strategy.unwrap_or_else(|| "auto".to_string()),
            command,
        })
    }
}

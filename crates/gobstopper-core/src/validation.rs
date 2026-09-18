use crate::strategy::PolicyConfig;
use crate::{Edit, Transcript};
use std::collections::{HashMap, HashSet};

pub const MAX_EDITS: usize = 64;
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_DIGEST_BYTES: usize = 32 * 1024;

pub fn validate_edits(
    transcript: &Transcript,
    policy: &PolicyConfig,
    edits: &[Edit],
) -> Result<(), &'static str> {
    if edits.len() > MAX_EDITS || transcript.items.len() > MAX_ITEMS {
        return Err("plan exceeds item limits");
    }
    let items: HashMap<_, _> = transcript.items.iter().map(|i| (i.line_index, i)).collect();
    if items.len() != transcript.items.len() {
        return Err("ambiguous transcript indexes");
    }
    let protected: HashSet<_> = transcript
        .items
        .iter()
        .rev()
        .filter(|i| i.elidable_bytes.is_some())
        .take(policy.keep_recent_tool_outputs)
        .map(|i| i.line_index)
        .collect();
    let known_tool_use_ids: HashSet<&str> = transcript
        .items
        .iter()
        .flat_map(|item| item.tool_use_ids.iter().map(String::as_str))
        .collect();
    let mut seen = HashSet::new();
    let mut digests = 0;
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
                per_item_stubs,
            } => {
                let line_set: HashSet<usize> = line_indexes.iter().copied().collect();
                if line_indexes.len() > MAX_ITEMS
                    || stub_template.len() > 96
                    || stub_template.contains("{kind}")
                    || stub_template.contains(['\n', '\r'])
                    || per_item_stubs.len() > line_indexes.len()
                    || per_item_stubs.keys().any(|k| !line_set.contains(k))
                    || per_item_stubs.values().any(|s| {
                        s.is_empty()
                            || s.len() > 200
                            || s.contains(['\n', '\r'])
                            || s.chars().any(char::is_control)
                    })
                {
                    return Err("invalid elision bounds or stub template");
                }
                for line in line_indexes {
                    let item = items.get(line).ok_or("unknown edit index")?;
                    if item.elidable_bytes.is_none()
                        || item.est_tokens == 0
                        || protected.contains(line)
                    {
                        return Err("edit targets protected or non-elidable content");
                    }
                    if !seen.insert(*line) {
                        return Err("overlapping elision edits");
                    }
                }
            }
            Edit::InjectDigest { digest } => {
                digests += 1;
                let size = digest
                    .goal
                    .iter()
                    .chain(&digest.summary)
                    .chain(&digest.concepts)
                    .chain(&digest.files_touched)
                    .chain(&digest.decisions)
                    .chain(&digest.errors)
                    .chain(&digest.open_tasks)
                    .chain(&digest.current_work)
                    .chain(&digest.context)
                    .try_fold(0usize, |total, s| total.checked_add(s.len()))
                    .ok_or("digest size overflow")?;
                if digests > 1
                    || digest.covers_items == 0
                    || digest.covers_items > transcript.items.len()
                    || size > MAX_DIGEST_BYTES
                {
                    return Err("invalid digest bounds");
                }
            }
            Edit::ProviderCompact { control } => {
                if edits.len() != 1
                    || control.is_empty()
                    || control.len() > 256
                    || control.chars().any(char::is_control)
                {
                    return Err("invalid provider control");
                }
            }
            Edit::CacheEdit { tool_use_ids } => {
                if edits.len() != 1
                    || transcript.session.provider != crate::Provider::ClaudeCode
                    || tool_use_ids.is_empty()
                    || tool_use_ids.len() > MAX_ITEMS
                {
                    return Err("invalid cache edit");
                }
                let mut cache_ids = HashSet::new();
                for id in tool_use_ids {
                    if id.is_empty()
                        || id.len() > 256
                        || !id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                        })
                        || !known_tool_use_ids.contains(id.as_str())
                        || !cache_ids.insert(id)
                    {
                        return Err("invalid cache edit tool_use_id");
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, Provider, SessionHandle, TranscriptItem, UsageSample};
    use crate::DigestBlock;
    use std::path::PathBuf;

    fn transcript(provider: Provider) -> Transcript {
        Transcript {
            session: SessionHandle {
                provider,
                session_id: "s".to_string(),
                path: PathBuf::from("/tmp/s"),
                cwd: None,
                age_secs: u64::MAX,
            },
            items: vec![TranscriptItem {
                line_index: 1,
                kind: ItemKind::ToolResult,
                est_tokens: 500,
                elidable_bytes: Some(2_000),
                elidable_parts: 1,
                label: "Read".to_string(),
                summary: None,
                uuid: None,
                parent_uuid: None,
                tool_use_ids: vec!["toolu_valid".to_string()],
                payload_sha256: Some("a".repeat(64)),
            }],
            usage: UsageSample {
                context_tokens: 500,
                ..Default::default()
            },
        }
    }

    #[test]
    fn every_digest_field_counts_toward_the_limit() {
        let transcript = transcript(Provider::ClaudeCode);
        let edits = vec![Edit::InjectDigest {
            digest: DigestBlock {
                errors: vec!["x".repeat(MAX_DIGEST_BYTES + 1)],
                covers_items: 1,
                ..Default::default()
            },
        }];
        assert!(validate_edits(&transcript, &PolicyConfig::default(), &edits).is_err());
    }

    #[test]
    fn cache_edits_are_claude_only_bound_and_unmixed() {
        let valid = vec![Edit::CacheEdit {
            tool_use_ids: vec!["toolu_valid".to_string()],
        }];
        assert!(validate_edits(
            &transcript(Provider::ClaudeCode),
            &PolicyConfig::default(),
            &valid
        )
        .is_ok());
        assert!(validate_edits(
            &transcript(Provider::Codex),
            &PolicyConfig::default(),
            &valid
        )
        .is_err());
        let unknown = vec![Edit::CacheEdit {
            tool_use_ids: vec!["toolu_unknown".to_string()],
        }];
        assert!(validate_edits(
            &transcript(Provider::ClaudeCode),
            &PolicyConfig::default(),
            &unknown
        )
        .is_err());
        let mixed = vec![
            valid[0].clone(),
            Edit::Elide {
                line_indexes: vec![1],
                stub_template: "[elided]".to_string(),
                per_item_stubs: Default::default(),
            },
        ];
        assert!(validate_edits(
            &transcript(Provider::ClaudeCode),
            &PolicyConfig {
                keep_recent_tool_outputs: 0,
                ..Default::default()
            },
            &mixed
        )
        .is_err());
    }

    #[test]
    fn per_item_stubs_are_bounded() {
        let transcript = transcript(Provider::ClaudeCode);
        let policy = PolicyConfig {
            keep_recent_tool_outputs: 0,
            ..Default::default()
        };
        let elide = |stubs: Vec<(usize, &str)>| {
            vec![Edit::Elide {
                line_indexes: vec![1],
                stub_template: "[elided {bytes}]".to_string(),
                per_item_stubs: stubs.into_iter().map(|(k, v)| (k, v.to_string())).collect(),
            }]
        };
        assert!(validate_edits(&transcript, &policy, &elide(vec![])).is_ok());
        assert!(
            validate_edits(&transcript, &policy, &elide(vec![(1, "read src/main.rs")])).is_ok()
        );
        // Unknown key, empty, oversized, and control-containing stubs fail.
        assert!(validate_edits(&transcript, &policy, &elide(vec![(2, "x")])).is_err());
        assert!(validate_edits(&transcript, &policy, &elide(vec![(1, "")])).is_err());
        assert!(validate_edits(&transcript, &policy, &elide(vec![(1, &"x".repeat(201))])).is_err());
        assert!(validate_edits(&transcript, &policy, &elide(vec![(1, "a\nb")])).is_err());
        assert!(validate_edits(&transcript, &policy, &elide(vec![(1, "a\u{7}b")])).is_err());
    }

    #[test]
    fn old_elide_json_deserializes_without_stub_map() {
        let edit: Edit =
            serde_json::from_str(r#"{"op":"elide","line_indexes":[1],"stub_template":"[elided]"}"#)
                .unwrap();
        match edit {
            Edit::Elide { per_item_stubs, .. } => assert!(per_item_stubs.is_empty()),
            _ => panic!("expected elide"),
        }
    }
}

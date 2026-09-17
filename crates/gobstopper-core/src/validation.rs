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
    let mut seen = HashSet::new();
    let mut digests = 0;
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
            } => {
                if line_indexes.len() > MAX_ITEMS
                    || stub_template.len() > 96
                    || stub_template.contains("{kind}")
                    || stub_template.contains(['\n', '\r'])
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
                    .chain(&digest.decisions)
                    .chain(&digest.files_touched)
                    .chain(&digest.open_tasks)
                    .try_fold(0usize, |total, s| total.checked_add(s.len()))
                    .ok_or("digest size overflow")?;
                if digests > 1
                    || digest.covers_items > transcript.items.len()
                    || size > MAX_DIGEST_BYTES
                {
                    return Err("invalid digest bounds");
                }
            }
            Edit::ProviderCompact { .. } if edits.len() != 1 => {
                return Err("provider controls cannot be mixed with file edits")
            }
            Edit::ProviderCompact { .. } => {}
        }
    }
    Ok(())
}

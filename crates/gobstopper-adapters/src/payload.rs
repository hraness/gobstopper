use serde_json::Value;
use sha2::{Digest, Sha256};

fn text_block(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("text" | "input_text" | "output_text")
    )
}

/// Concatenate the text content of a string or an array of text blocks.
pub fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| text_block(b))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

pub fn text_bytes(value: &Value) -> u64 {
    match value {
        Value::String(s) => s.len() as u64,
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| text_block(b))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .fold(0u64, |total, text| total.saturating_add(text.len() as u64)),
        _ => 0,
    }
}

pub fn eligible_bytes(value: &Value) -> u64 {
    let bytes = if supported_content(value) {
        text_bytes(value)
    } else {
        0
    };
    if bytes > 256 {
        bytes
    } else {
        0
    }
}

pub fn fingerprint<'a>(values: impl IntoIterator<Item = &'a Value>) -> Option<String> {
    let mut hasher = Sha256::new();
    let mut count = 0u64;
    for value in values {
        let bytes = serde_json::to_vec(value).ok()?;
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        count = count.saturating_add(1);
    }
    (count > 0).then(|| format!("{:x}", hasher.finalize()))
}

pub fn elide(value: &mut Value, stub: String) -> u64 {
    let old = eligible_bytes(value);
    if old == 0 || stub.len() as u64 >= old {
        return 0;
    }
    let mut candidate = value.clone();
    match &mut candidate {
        Value::String(s) => *s = stub,
        Value::Array(blocks) => {
            let mut replacement = Some(stub);
            for block in blocks.iter_mut().filter(|b| text_block(b)) {
                if block.get("text").is_some_and(Value::is_string) {
                    block["text"] = Value::String(replacement.take().unwrap_or_default());
                }
            }
        }
        _ => return 0,
    }
    if candidate.to_string().len() >= value.to_string().len() {
        return 0;
    }
    *value = candidate;
    old
}

/// Duplicate JSON keys make identity, role and rewrite selection ambiguous.
/// Keep serde_json's default nesting bound while rejecting them recursively.
pub(crate) fn decode_record(raw: &str) -> Result<Value, serde_json::Error> {
    use serde::de::{MapAccess, SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer};
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> Visitor<'de> for V {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("unambiguous JSON")
                }
                fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Unique, E> {
                    serde_json::Number::from_f64(v)
                        .map(|v| Unique(v.into()))
                        .ok_or_else(|| E::custom("invalid number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
                fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                    let mut out = Vec::new();
                    while let Some(Unique(v)) = a.next_element()? {
                        out.push(v);
                    }
                    Ok(Unique(Value::Array(out)))
                }
                fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                    let mut out = serde_json::Map::new();
                    while let Some(k) = a.next_key::<String>()? {
                        if out.contains_key(&k) {
                            return Err(serde::de::Error::custom("duplicate JSON key"));
                        }
                        out.insert(k, a.next_value::<Unique>()?.0);
                    }
                    Ok(Unique(Value::Object(out)))
                }
            }
            d.deserialize_any(V)
        }
    }
    serde_json::from_str::<Unique>(raw).map(|v| v.0)
}

/// Only a string or a wholly understood text-block array has elision authority.
/// Mixed image/reasoning/system/future blocks are unavailable as a whole.
pub(crate) fn supported_content(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Array(blocks) => {
            !blocks.is_empty()
                && blocks.iter().all(|block| {
                    text_block(block)
                        && block.get("text").is_some_and(Value::is_string)
                        && block.as_object().is_some_and(|object| {
                            object.keys().all(|k| matches!(k.as_str(), "type" | "text"))
                        })
                })
        }
        _ => false,
    }
}

/// Lowering independently restricts caller-selected physical indexes to the
/// current supported projection. Passing an arbitrary Edit grants no authority
/// to rewrite protected, dead, ambiguous or unknown records.
pub(crate) fn elision_targets(
    provider: gobstopper_core::Provider,
    raw: &str,
    requested: &[usize],
) -> Result<std::collections::HashSet<usize>, crate::AdapterError> {
    let handle = gobstopper_core::SessionHandle {
        provider,
        session_id: "detached".into(),
        path: Default::default(),
        cwd: None,
        age_secs: u64::MAX,
    };
    let transcript = match provider {
        gobstopper_core::Provider::Codex => crate::codex::load_bytes(handle, raw.as_bytes()),
        gobstopper_core::Provider::ClaudeCode => crate::claude::load_bytes(handle, raw.as_bytes()),
        gobstopper_core::Provider::Devin => crate::devin::load_bytes(handle, raw.as_bytes()),
    }?;
    let eligible: std::collections::HashSet<_> = transcript
        .items
        .iter()
        .filter(|item| item.is_elidable())
        .map(|item| item.line_index)
        .collect();
    Ok(requested
        .iter()
        .copied()
        .filter(|i| eligible.contains(i))
        .collect())
}

pub(crate) fn check_digest_bounds(
    digest: &gobstopper_core::DigestBlock,
) -> Result<(), crate::AdapterError> {
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
        .try_fold(0usize, |total, s| total.checked_add(s.len()));
    if size.is_none_or(|n| n > gobstopper_core::validation::MAX_DIGEST_BYTES) {
        return Err(crate::AdapterError::InvalidEdit(
            "digest exceeds transform bounds",
        ));
    }
    Ok(())
}

pub(crate) fn check_edit_bounds(
    edits: &[gobstopper_core::Edit],
) -> Result<(), crate::AdapterError> {
    use gobstopper_core::{
        validation::{MAX_DIGEST_BYTES, MAX_EDITS, MAX_ITEMS},
        Edit,
    };
    if edits.len() > MAX_EDITS {
        return Err(crate::AdapterError::InvalidEdit("too many transform edits"));
    }
    for edit in edits {
        match edit {
            Edit::Elide {
                line_indexes,
                stub_template,
                per_item_stubs,
            } if line_indexes.len() > MAX_ITEMS
                || stub_template.len() > MAX_DIGEST_BYTES
                || per_item_stubs.len() > MAX_ITEMS
                || per_item_stubs.values().any(|s| s.len() > MAX_DIGEST_BYTES) =>
            {
                return Err(crate::AdapterError::InvalidEdit(
                    "elision exceeds transform bounds",
                ))
            }
            Edit::InjectDigest { digest } => check_digest_bounds(digest)?,
            _ => {}
        }
    }
    Ok(())
}

/// Small, regular-file-only head inspection. Complete records only, no unbounded
/// read_line allocation. A truncated oversized record has no identity authority.
pub(crate) fn head_records(path: &std::path::Path, max_records: usize) -> Vec<Value> {
    use std::io::Read;
    const LIMIT: u64 = 512 * 1024;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        return Vec::new();
    }
    let Ok(file) = options.open(path) else {
        return Vec::new();
    };
    let Ok(meta) = file.metadata() else {
        return Vec::new();
    };
    if !meta.is_file() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(LIMIT + 1).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    if bytes.len() as u64 > LIMIT {
        bytes.truncate(LIMIT as usize);
        let Some(end) = bytes.iter().rposition(|byte| *byte == b'\n') else {
            return Vec::new();
        };
        bytes.truncate(end + 1);
    }
    let mut records = Vec::new();
    for raw in bytes.split(|byte| *byte == b'\n').take(max_records.min(64)) {
        let Ok(raw) = std::str::from_utf8(raw) else {
            return Vec::new();
        };
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(record) = decode_record(raw) else {
            return Vec::new();
        };
        if !record.is_object() {
            return Vec::new();
        }
        records.push(record);
    }
    records
}

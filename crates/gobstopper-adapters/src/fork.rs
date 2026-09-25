//! Fork-on-write: copy a session transcript to a NEW session id so a
//! compaction (or resume) can be tried without touching the original.
//!
//! Claude Code (`Provider::ClaudeCode`): the fork is `<new-id>.jsonl` in
//! the same projects directory; every line's `sessionId` field is
//! rewritten to the new id while `uuid`/`parentUuid` linkage is left
//! untouched, so the conversation tree stays internally valid.
//!
//! Codex (`Provider::Codex`): the fork is `rollout-<isots>-<new-id>.jsonl`
//! next to the source rollout — the source filename's timestamp portion
//! is reused and the new id keeps the name unique. The session id inside
//! `session_meta` records (`payload.id` / `payload.session_id`, the same
//! fields [`crate::codex::scan_meta`] reads) is rewritten; other
//! id-shaped fields the adapter does not own (e.g. `rollout_id`) pass
//! through untouched.
//!
//! Writes go through a temp file + rename in the same directory, and an
//! existing target is never overwritten — forking is additive-only.

use anyhow::{bail, Context};
use gobstopper_core::Provider;
use serde_json::Value;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Outcome of a successful [`fork`].
#[derive(Debug, Clone)]
pub struct ForkResult {
    /// Path to the new sibling transcript.
    pub path: PathBuf,
    /// The new session id (uuid v4 string).
    pub session_id: String,
    /// Provider-specific resume hint, e.g. `claude --resume <id>` or
    /// `codex fork <id>` — display text only.
    pub resume_hint: String,
}

/// Process-unique counter mixed into generated ids and temp names so
/// same-instant forks still diverge.
static FORK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Pseudo-uuid v4: SHA-256 over (time, pid, counter, source path, stack
/// address) with the version/variant bits forced — no uuid crate needed.
pub(crate) fn generate_session_id(seed: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_be_bytes());
    hasher.update(std::process::id().to_be_bytes());
    hasher.update(FORK_COUNTER.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    hasher.update(seed.to_string_lossy().as_bytes());
    // A stack address contributes ASLR entropy independent of the pid.
    let marker = 0u8;
    hasher.update((&marker as *const u8 as usize).to_be_bytes());
    let digest = hasher.finalize();

    let mut b = [0u8; 16];
    b.copy_from_slice(&digest[..16]);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant 10

    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        let _ = write!(s, "{byte:02x}");
    }
    s
}

/// A caller-supplied id lands in a filename; restrict it to the
/// uuid-ish alphabet so it can never traverse directories.
pub(crate) fn validate_session_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        bail!("invalid session id {id:?}: expected [0-9a-zA-Z-]+");
    }
    Ok(())
}

/// Map every JSON object line through `rewrite`; lines that fail to
/// parse, are not objects, or which `rewrite` declines pass through
/// byte-for-byte, newline included.
fn map_jsonl<F>(raw: &str, mut rewrite: F) -> String
where
    F: FnMut(&mut serde_json::Map<String, Value>) -> bool,
{
    let mut out = String::with_capacity(raw.len() + 64);
    for line in raw.split_inclusive('\n') {
        let (body, nl) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        let rewritten = crate::payload::decode_record(body)
            .ok()
            .and_then(|mut record| {
                let changed = record.as_object_mut().is_some_and(&mut rewrite);
                changed.then(|| serde_json::to_string(&record).unwrap_or_else(|_| body.to_string()))
            });
        match rewritten {
            Some(r) => {
                out.push_str(&r);
                out.push_str(nl);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// Claude dialect: rewrite `sessionId` on every line that carries it.
/// All other lines — and all `uuid`/`parentUuid` linkage — pass through
/// byte-for-byte so the conversation tree stays valid.
fn fork_claude(raw: &str, new_id: &str) -> String {
    map_jsonl(raw, |obj| {
        if obj.contains_key("sessionId") {
            obj.insert("sessionId".to_string(), Value::String(new_id.to_string()));
            true
        } else {
            false
        }
    })
}

/// Codex dialect: rewrite the session id inside `session_meta` records —
/// `payload.id` and `payload.session_id`, the same fields
/// [`crate::codex::scan_meta`] treats as the session id. Every other
/// record (and any other id-shaped field) passes through byte-for-byte.
fn fork_codex(raw: &str, new_id: &str) -> String {
    map_jsonl(raw, |obj| {
        if obj.get("type").and_then(Value::as_str) != Some("session_meta") {
            return false;
        }
        let Some(payload) = obj.get_mut("payload").and_then(Value::as_object_mut) else {
            return false;
        };
        let mut touched = false;
        for key in ["id", "session_id"] {
            if payload.contains_key(key) {
                payload.insert(key.to_string(), Value::String(new_id.to_string()));
                touched = true;
            }
        }
        touched
    })
}

/// Strip a trailing `-<uuid>` (36 chars dashed 8-4-4-4-12) from a rollout
/// stem, returning the `rollout-<isots>` prefix.
fn strip_uuid_suffix(stem: &str) -> Option<&str> {
    let split = stem.len().checked_sub(37)?;
    if !stem.is_char_boundary(split) {
        return None;
    }
    let (head, tail) = stem.split_at(split);
    if head.is_empty() || !tail.starts_with('-') {
        return None;
    }
    let id = &tail[1..];
    let dashed = [8usize, 13, 18, 23];
    let uuid_shaped = id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if dashed.contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        });
    uuid_shaped.then_some(head)
}

/// `rollout-<isots>-<uuid>.jsonl` naming: reuse the source's timestamp
/// portion when it can be isolated — preferring the recorded session id,
/// then uuid shape — else keep the whole stem. The new id appended last
/// keeps the name unique either way.
fn codex_fork_name(src: &Path, old_id: Option<&str>, new_id: &str) -> String {
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rollout");
    let prefix = old_id
        .and_then(|old| stem.strip_suffix(old))
        .and_then(|s| s.strip_suffix('-'))
        .filter(|s| !s.is_empty())
        .or_else(|| strip_uuid_suffix(stem))
        .unwrap_or(stem);
    format!("{prefix}-{new_id}.jsonl")
}

/// Fork `src` into a sibling transcript with a fresh session id.
/// Returns the new file's path. The original is never modified.
/// Legacy entry point refuses before I/O: recovery publication requires an
/// explicit vault root. Use `fork_with_vault`.
pub fn fork(
    _provider: Provider,
    _src: &Path,
    _new_session_id: Option<String>,
) -> anyhow::Result<ForkResult> {
    bail!("fork requires an explicit recovery vault; use fork_with_vault")
}

pub fn fork_with_vault(
    provider: Provider,
    src: &Path,
    new_session_id: Option<String>,
    root: &Path,
) -> anyhow::Result<ForkResult> {
    let src = src.canonicalize()?;
    let original = crate::transaction::read(&src)?;
    if crate::detect::sniff_provider(&src).is_some_and(|actual| actual != provider) {
        bail!("source provider does not match fork provider");
    }
    let new_id = new_session_id.unwrap_or_else(|| generate_session_id(&src));
    validate_session_id(&new_id)?;
    publish_fork(provider, &src, &original, new_id, "fork", root)
}

/// Recover one exact identity from frozen bytes. Missing, conflicting, malformed
/// or duplicate-key metadata grants no authority to target a provider session.
/// This checks local identity, not provider acceptance or live ownership.
pub fn source_session_id(provider: Provider, bytes: &[u8]) -> anyhow::Result<String> {
    if bytes.len() as u64 > crate::transaction::max_transcript_bytes() {
        bail!("source identity exceeds transcript byte limit");
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("source identity is not UTF-8"))?;
    let mut identity: Option<String> = None;
    for (line_index, line) in text.lines().enumerate() {
        if line_index >= gobstopper_core::validation::MAX_ITEMS {
            bail!("source identity exceeds record limit");
        }
        if line.trim().is_empty() {
            continue;
        }
        let value = crate::payload::decode_record(line)
            .map_err(|_| anyhow::anyhow!("source identity contains malformed or ambiguous JSON"))?;
        let ids: Vec<&Value> = match provider {
            Provider::Codex if value["type"] == "session_meta" => {
                let payload = value
                    .get("payload")
                    .filter(|v| v.is_object())
                    .ok_or_else(|| anyhow::anyhow!("source metadata is missing"))?;
                let ids: Vec<_> = [payload.get("id"), payload.get("session_id")]
                    .into_iter()
                    .flatten()
                    .collect();
                if ids.is_empty() {
                    bail!("source metadata has no session identity");
                }
                ids
            }
            Provider::ClaudeCode => value.get("sessionId").into_iter().collect(),
            _ => Vec::new(),
        };
        for value in ids {
            let id = value
                .as_str()
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 256
                        && id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                        })
                })
                .ok_or_else(|| anyhow::anyhow!("source session identity is invalid"))?;
            if identity.as_deref().is_some_and(|prior| prior != id) {
                bail!("source session identities disagree");
            }
            identity = Some(id.to_owned());
        }
    }
    identity.ok_or_else(|| anyhow::anyhow!("source session identity is unavailable"))
}

fn publish_fork(
    provider: Provider,
    source: &Path,
    bytes: &[u8],
    session_id: String,
    kind: &str,
    root: &Path,
) -> anyhow::Result<ForkResult> {
    let source_session_id = source_session_id(provider, bytes)?;
    if crate::verify::verify(provider, bytes)
        .iter()
        .any(|finding| finding.severity == crate::verify::Severity::Error)
    {
        bail!("fork source has unsupported structural defects");
    }
    let output = crate::transaction::prepare(provider, bytes, |raw| {
        Ok(rewrite_identity(provider, raw, &session_id))
    })?;
    let op = crate::copy::OperationIdentity {
        revision: 2,
        kind: kind.into(),
        provider,
        source_path: source.into(),
        source_session_id,
        source_sha256: crate::copy::sha256(bytes),
        inputs_sha256: crate::copy::sha256(session_id.as_bytes()),
        output_session_id: session_id.clone(),
    };
    let receipt = crate::copy::publish_prepared(op, bytes, &output, Some(kind), root)?;
    Ok(ForkResult {
        path: receipt.path,
        session_id: session_id.clone(),
        resume_hint: match provider {
            Provider::Codex if kind == "fork" => format!("codex fork {session_id}"),
            Provider::Codex => format!("codex resume {session_id}"),
            Provider::ClaudeCode => format!("claude --resume {session_id}"),
        },
    })
}

/// Restore one exact archived object into a new no-clobber sibling session.
/// `source` supplies only the destination directory and naming anchor; it may
/// be missing or corrupt, so its current contents are not recovery authority.
/// Callers selecting an existing session must independently check that the
/// archived bytes and index binding match that expected provider/session/store
/// before invoking this function. The snapshot's own identity is validated here.
pub fn restore_copy(
    provider: Provider,
    source: &Path,
    sha256: &str,
    root: &Path,
) -> anyhow::Result<ForkResult> {
    let reader = crate::vault::Reader::open(root)?;
    let bytes = reader.read_object(sha256)?;
    if crate::verify::verify(provider, &bytes)
        .iter()
        .any(|f| f.severity == crate::verify::Severity::Error)
    {
        bail!("snapshot has structural errors; source was not modified");
    }
    let source = if source.is_absolute() {
        source.to_path_buf()
    } else {
        std::env::current_dir()?.join(source)
    };
    let source = source.canonicalize().or_else(|_| {
        let parent = source
            .parent()
            .context("source has no parent")?
            .canonicalize()?;
        Ok::<_, anyhow::Error>(parent.join(source.file_name().context("source has no filename")?))
    })?;
    publish_fork(
        provider,
        &source,
        &bytes,
        generate_session_id(&source),
        "restore",
        root,
    )
}

pub(crate) fn rewrite_identity(provider: Provider, raw: &str, id: &str) -> String {
    match provider {
        Provider::Codex => fork_codex(raw, id),
        Provider::ClaudeCode => fork_claude(raw, id),
    }
}

pub(crate) fn target_path_bound(
    provider: Provider,
    source: &Path,
    old_id: &str,
    id: &str,
) -> PathBuf {
    let name = match provider {
        Provider::ClaudeCode => format!("{id}.jsonl"),
        Provider::Codex => codex_fork_name(source, Some(old_id), id),
    };
    source.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fork(provider: Provider, src: &Path, id: Option<String>) -> anyhow::Result<ForkResult> {
        fork_with_vault(provider, src, id, &src.parent().unwrap().join("vault"))
    }
    use crate::verify::{verify, Severity};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "gob-fork-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn uuid_shaped(id: &str) -> bool {
        let b = id.as_bytes();
        b.len() == 36
            && [8usize, 13, 18, 23].iter().all(|&i| b[i] == b'-')
            && b.iter()
                .enumerate()
                .all(|(i, c)| [8, 13, 18, 23].contains(&i) || c.is_ascii_hexdigit())
    }

    #[test]
    fn generated_id_is_uuid_v4_shaped() {
        let id = generate_session_id(Path::new("seed"));
        assert!(uuid_shaped(&id), "not uuid-shaped: {id}");
        let b = id.as_bytes();
        assert_eq!(b[14], b'4', "version nibble");
        assert!(matches!(b[19], b'8' | b'9' | b'a' | b'b'), "variant bits");
        // Two generations never collide.
        assert_ne!(id, generate_session_id(Path::new("seed")));
    }

    #[test]
    fn claude_fork_rewrites_session_id_and_keeps_chain() {
        let dir = TestDir::new();
        let src = dir.0.join("11111111-2222-4333-8444-555555555555.jsonl");
        let raw = concat!(
            "{\"type\":\"summary\",\"summary\":\"s\",\"leafUuid\":\"u3\"}\n",
            "{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"sessionId\":\"OLD\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
            "{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"sessionId\":\"OLD\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Bash\",\"input\":{}}]}}\n",
            "{\"type\":\"user\",\"uuid\":\"u3\",\"parentUuid\":\"u2\",\"sessionId\":\"OLD\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}]}}\n",
        );
        fs::write(&src, raw).unwrap();

        let res = fork(Provider::ClaudeCode, &src, None).unwrap();
        assert!(uuid_shaped(&res.session_id));
        // `<new-id>.jsonl` in the same directory.
        assert_eq!(res.path.parent().unwrap(), dir.0.canonicalize().unwrap());
        assert_eq!(
            res.path.file_name().unwrap().to_str().unwrap(),
            format!("{}.jsonl", res.session_id)
        );
        assert_eq!(
            res.resume_hint,
            format!("claude --resume {}", res.session_id)
        );

        let forked = fs::read_to_string(&res.path).unwrap();
        // Every sessionId rewritten.
        for line in forked.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            if let Some(sid) = v.get("sessionId") {
                assert_eq!(sid.as_str().unwrap(), res.session_id);
            }
        }
        // Line without sessionId passed through byte-for-byte.
        assert!(
            forked.starts_with("{\"type\":\"summary\",\"summary\":\"s\",\"leafUuid\":\"u3\"}\n")
        );
        // uuid/parentUuid chain intact — verify reports zero findings.
        let findings = verify(Provider::ClaudeCode, forked.as_bytes());
        assert_eq!(findings, vec![], "expected zero findings, got {findings:?}");
        // Original untouched.
        assert_eq!(fs::read_to_string(&src).unwrap(), raw);
    }

    #[test]
    fn claude_fork_refuses_existing_target() {
        let dir = TestDir::new();
        let src = dir.0.join("a.jsonl");
        fs::write(
            &src,
            "{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"s\"}\n",
        )
        .unwrap();
        let taken = dir.0.join("taken.jsonl");
        fs::write(&taken, "{\"keep\":true}\n").unwrap();

        let err = fork(Provider::ClaudeCode, &src, Some("taken".into())).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        // The existing file is not clobbered.
        assert_eq!(fs::read_to_string(&taken).unwrap(), "{\"keep\":true}\n");
    }

    #[test]
    fn fork_rejects_path_like_session_id() {
        let dir = TestDir::new();
        let src = dir.0.join("a.jsonl");
        fs::write(&src, "{\"type\":\"user\",\"sessionId\":\"s\"}\n").unwrap();
        assert!(fork(Provider::ClaudeCode, &src, Some("../evil".into())).is_err());
        assert!(fork(Provider::ClaudeCode, &src, Some(String::new())).is_err());
        assert!(fork(Provider::ClaudeCode, &src, Some("a/b".into())).is_err());
        // Nothing was written.
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn fork_rejects_oversized_source_before_reading_it() {
        let dir = TestDir::new();
        let src = dir.0.join("large.jsonl");
        let file = fs::File::create(&src).unwrap();
        file.set_len(crate::transaction::max_transcript_bytes() + 1)
            .unwrap();
        assert!(fork(Provider::ClaudeCode, &src, Some("bounded".into())).is_err());
        assert!(!dir.0.join("bounded.jsonl").exists());
    }

    #[test]
    fn fork_rejects_confident_provider_mismatch() {
        let dir = TestDir::new();
        let src = dir.0.join("s.jsonl");
        fs::write(
            &src,
            "{\"type\":\"user\",\"uuid\":\"u1\",\"sessionId\":\"s\"}\n",
        )
        .unwrap();
        // File sniffs as Claude; asking for a Codex fork must fail.
        assert!(fork(Provider::Codex, &src, None).is_err());
    }

    #[test]
    fn codex_fork_rewrites_meta_id_and_names_like_siblings() {
        let dir = TestDir::new();
        let old = "11111111-2222-4333-8444-555555555555";
        let src = dir
            .0
            .join(format!("rollout-2025-06-01T12-00-00-{old}.jsonl"));
        let raw = concat!(
            "{\"timestamp\":\"t\",\"ordinal\":1,\"type\":\"session_meta\",\"payload\":{\"id\":\"11111111-2222-4333-8444-555555555555\",\"cwd\":\"/w\"}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":2,\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[]}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":3,\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"input_tokens\":10}}}\n",
        );
        fs::write(&src, raw).unwrap();

        let res = fork(Provider::Codex, &src, None).unwrap();
        // Timestamp portion reused; new id keeps the name unique.
        assert_eq!(
            res.path.file_name().unwrap().to_str().unwrap(),
            format!("rollout-2025-06-01T12-00-00-{}.jsonl", res.session_id)
        );
        assert_eq!(res.resume_hint, format!("codex fork {}", res.session_id));

        // The adapter's own meta scan sees the new id.
        let (id, cwd) = crate::codex::scan_meta(&res.path);
        assert_eq!(id.as_deref(), Some(res.session_id.as_str()));
        assert_eq!(cwd.as_deref(), Some(Path::new("/w")));

        let forked = fs::read_to_string(&res.path).unwrap();
        let out_lines: Vec<&str> = forked.lines().collect();
        let src_lines: Vec<&str> = raw.lines().collect();
        assert_eq!(out_lines.len(), src_lines.len());
        // Non-meta records are byte-identical.
        assert_eq!(out_lines[1], src_lines[1]);
        assert_eq!(out_lines[2], src_lines[2]);
        // The meta record carries the new id and drops the old.
        assert!(out_lines[0].contains(&res.session_id));
        assert!(!out_lines[0].contains(old));

        // Resume-valid.
        let findings = verify(Provider::Codex, forked.as_bytes());
        assert_eq!(findings, vec![], "expected zero findings, got {findings:?}");
        // Original untouched.
        assert_eq!(fs::read_to_string(&src).unwrap(), raw);
    }

    #[test]
    fn codex_fork_rewrites_session_id_variant() {
        // Older schema: `payload.session_id` instead of `payload.id`.
        let dir = TestDir::new();
        let old = "22222222-3333-4444-8555-666666666666";
        let src = dir
            .0
            .join(format!("rollout-2025-06-02T08-15-30-{old}.jsonl"));
        let raw = concat!(
            "{\"timestamp\":\"t\",\"ordinal\":1,\"type\":\"session_meta\",\"payload\":{\"session_id\":\"22222222-3333-4444-8555-666666666666\"}}\n",
            "{\"timestamp\":\"t\",\"ordinal\":2,\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\"}}\n",
        );
        fs::write(&src, raw).unwrap();

        let res = fork(Provider::Codex, &src, Some("newid-123".into())).unwrap();
        assert_eq!(res.session_id, "newid-123");
        let (id, _) = crate::codex::scan_meta(&res.path);
        assert_eq!(id.as_deref(), Some("newid-123"));
        let findings = verify(Provider::Codex, &fs::read(&res.path).unwrap());
        assert!(!findings.iter().any(|f| f.severity == Severity::Error));
    }

    #[test]
    fn codex_fork_refuses_missing_identity_even_with_uuid_filename() {
        // No session_meta record: fall back to the uuid-shaped filename
        // suffix to recover the timestamp portion.
        let dir = TestDir::new();
        let old = "33333333-4444-4555-8666-777777777777";
        let src = dir
            .0
            .join(format!("rollout-2025-06-03T09-30-00-{old}.jsonl"));
        fs::write(
            &src,
            "{\"timestamp\":\"t\",\"ordinal\":1,\"type\":\"response_item\",\"payload\":{\"type\":\"message\"}}\n",
        )
        .unwrap();

        assert!(fork(Provider::Codex, &src, None).is_err());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn identity_authority_rejects_ambiguous_metadata_before_creating_recovery_or_output() {
        let dir = TestDir::new();
        let source = dir.0.join("source.jsonl");
        for raw in [
            r#"{"type":"session_meta","payload":{"id":"a","id":"b"}}"#,
            r#"{"type":"session_meta","payload":{"id":"a","session_id":"b"}}"#,
            r#"{"type":"session_meta","payload":{"id":null,"session_id":"b"}}"#,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"a\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"b\"}}",
        ] {
            fs::write(&source, raw).unwrap();
            assert!(source_session_id(Provider::Codex, raw.as_bytes()).is_err());
            assert!(fork(Provider::Codex, &source, Some("new-session".into())).is_err());
            assert_eq!(fs::read_to_string(&source).unwrap(), raw);
            assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        }
        for raw in [
            r#"{"type":"session_meta","payload":{"id":"a"}}"#,
            r#"{"type":"session_meta","payload":{"session_id":"a"}}"#,
            r#"{"type":"session_meta","payload":{"id":"a","session_id":"a"}}"#,
        ] {
            assert_eq!(
                source_session_id(Provider::Codex, raw.as_bytes()).unwrap(),
                "a"
            );
        }
        assert!(source_session_id(
            Provider::ClaudeCode,
            b"{\"sessionId\":\"a\"}\n{\"sessionId\":\"b\"}"
        )
        .is_err());
        // Even the non-authoritative pure mapper preserves duplicate-key input;
        // it cannot silently delete a field before a later verifier sees it.
        let duplicate = r#"{"type":"user","sessionId":"a","sessionId":"b"}"#;
        assert_eq!(
            rewrite_identity(Provider::ClaudeCode, duplicate, "new"),
            duplicate
        );
    }
}

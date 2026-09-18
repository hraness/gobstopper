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
fn validate_session_id(id: &str) -> anyhow::Result<()> {
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
        let rewritten = serde_json::from_str::<Value>(body)
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
pub fn fork(
    provider: Provider,
    src: &Path,
    new_session_id: Option<String>,
) -> anyhow::Result<ForkResult> {
    let src = src.canonicalize()?;
    let original = crate::transaction::read(&src)?;
    let new_id = match new_session_id {
        Some(id) => {
            validate_session_id(&id)?;
            id
        }
        None => generate_session_id(&src),
    };

    // A wrong-provider fork would produce a corrupt sibling; bail only
    // on a confident mismatch so head-truncated files still fork.
    if let Some(actual) = crate::detect::sniff_provider(&src) {
        if actual != provider {
            bail!(
                "{} looks like a {} transcript, not {}",
                src.display(),
                actual.as_str(),
                provider.as_str()
            );
        }
    }

    let dir = src.parent().unwrap_or_else(|| Path::new("."));
    let name = match provider {
        Provider::ClaudeCode => format!("{new_id}.jsonl"),
        Provider::Codex => {
            let old_id = crate::codex::scan_meta(&src).0;
            codex_fork_name(&src, old_id.as_deref(), &new_id)
        }
    };
    let target = dir.join(&name);
    // symlink_metadata (not exists) so a dangling symlink can't be
    // silently replaced either.
    if fs::symlink_metadata(&target).is_ok() {
        bail!("fork target {} already exists", target.display());
    }

    let raw = std::str::from_utf8(&original).context("transcript is not UTF-8")?;
    let out = match provider {
        Provider::ClaudeCode => fork_claude(raw, &new_id),
        Provider::Codex => fork_codex(raw, &new_id),
    };

    // Temp file + rename in the same directory: a killed fork never
    // leaves a half-written transcript behind the new id's name.
    if crate::transaction::read(&src)? != original {
        bail!("source changed while preparing fork");
    }
    crate::transaction::publish_new(&target, out.as_bytes())?;

    let resume_hint = match provider {
        Provider::ClaudeCode => format!("claude --resume {new_id}"),
        Provider::Codex => format!("codex fork {new_id}"),
    };
    Ok(ForkResult {
        path: target,
        session_id: new_id,
        resume_hint,
    })
}

pub fn restore_copy(
    provider: Provider,
    source: &Path,
    sha256: &str,
    root: &Path,
) -> anyhow::Result<ForkResult> {
    let bytes = crate::vault::read_object(sha256, root)?;
    if crate::verify::verify(provider, &bytes)
        .iter()
        .any(|f| f.severity == crate::verify::Severity::Error)
    {
        bail!("snapshot has structural errors; source was not modified");
    }
    let session_id = generate_session_id(source);
    let output = rewrite_identity(provider, std::str::from_utf8(&bytes)?, &session_id);
    let path = target_path(provider, source, &session_id);
    crate::transaction::publish_new(&path, output.as_bytes())?;
    let resume_hint = match provider {
        Provider::Codex => format!("codex resume {session_id}"),
        Provider::ClaudeCode => format!("claude --resume {session_id}"),
    };
    Ok(ForkResult {
        path,
        session_id,
        resume_hint,
    })
}

pub(crate) fn rewrite_identity(provider: Provider, raw: &str, id: &str) -> String {
    match provider {
        Provider::Codex => fork_codex(raw, id),
        Provider::ClaudeCode => fork_claude(raw, id),
    }
}

pub(crate) fn target_path(provider: Provider, source: &Path, id: &str) -> PathBuf {
    let name = match provider {
        Provider::ClaudeCode => format!("{id}.jsonl"),
        Provider::Codex => {
            codex_fork_name(source, crate::codex::scan_meta(source).0.as_deref(), id)
        }
    };
    source.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        file.set_len(crate::transaction::MAX_TRANSCRIPT_BYTES + 1)
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
    fn codex_fork_name_survives_missing_meta() {
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

        let res = fork(Provider::Codex, &src, None).unwrap();
        assert_eq!(
            res.path.file_name().unwrap().to_str().unwrap(),
            format!("rollout-2025-06-03T09-30-00-{}.jsonl", res.session_id)
        );
    }
}

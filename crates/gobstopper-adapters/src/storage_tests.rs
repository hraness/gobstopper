//! Declared storage boundary tests. Fault switches exist only in this test binary.
use crate::transaction::faults::{self, Action};
use crate::{copy, fork, transaction, vault, AdapterError};
use gobstopper_core::{CompactionPlan, Edit, Provider, SessionHandle};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "gobstopper-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf {
        self.0.join("vault")
    }
    fn source(&self) -> PathBuf {
        self.0.join("rollout-synthetic.jsonl")
    }
    fn setup(&self) -> (SessionHandle, String, CompactionPlan) {
        setup(&self.0)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn setup(dir: &Path) -> (SessionHandle, String, CompactionPlan) {
    let source = dir.join("rollout-synthetic.jsonl");
    let bytes = format!(
        "{}\n{}\n",
        json!({"type":"session_meta","payload":{"id":"synthetic"}}),
        json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"t","output":"x".repeat(4000)}})
    );
    if !source.exists() {
        fs::write(&source, &bytes).unwrap();
    }
    (
        SessionHandle {
            provider: Provider::Codex,
            session_id: "synthetic".into(),
            path: source,
            cwd: None,
            age_secs: u64::MAX,
        },
        copy::sha256(bytes.as_bytes()),
        CompactionPlan {
            strategy: "elide".into(),
            rationale: "synthetic".into(),
            context_tokens_before: 1000,
            context_tokens_after: 10,
            edits: vec![Edit::Elide {
                line_indexes: vec![1],
                stub_template: "[elided]".into(),
                per_item_stubs: Default::default(),
            }],
        },
    )
}
fn operation(root: &Path) -> (String, PathBuf, copy::CopyReceipt) {
    let path = fs::read_dir(root.join("operations"))
        .unwrap()
        .map(Result::unwrap)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap();
    (
        path.file_stem().unwrap().to_str().unwrap().into(),
        path.clone(),
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap(),
    )
}
fn inventory(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        if !root.exists() {
            return;
        }
        for entry in fs::read_dir(root).unwrap().map(Result::unwrap) {
            if entry.file_type().unwrap().is_dir() {
                visit(&entry.path(), out);
            } else {
                out.push((entry.path(), fs::read(entry.path()).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    visit(root, &mut out);
    out.sort();
    out
}

#[test]
fn publication_faults_preserve_actual_visibility_and_io_cause() {
    for stage in [
        "write",
        "file_sync",
        "link",
        "directory_sync",
        "publication_sync",
    ] {
        for after in [false, true] {
            let f = Fixture::new();
            let target = f.0.join("output");
            let guard = faults::install(stage, None, after, 1, Action::Error);
            let err = transaction::publish_new(&target, b"candidate").unwrap_err();
            drop(guard);
            let visible =
                matches!(stage, "directory_sync" | "publication_sync") || stage == "link" && after;
            assert_eq!(target.exists(), visible, "{stage}/{after}: {err}");
            if visible {
                assert_eq!(fs::read(&target).unwrap(), b"candidate");
                assert!(
                    matches!(&err, AdapterError::Publication { visibility: transaction::Visibility::Published, source, .. }
                    if source.to_string() == "injected storage failure")
                );
            }
            assert!(err.to_string().contains("injected storage failure"));
        }
    }
    for after in [false, true] {
        let f = Fixture::new();
        let target = f.0.join("owned-index");
        fs::write(&target, b"old").unwrap();
        let guard = faults::install("rename", Some(&target), after, 1, Action::Error);
        let err = transaction::replace(&target, b"old", b"new").unwrap_err();
        drop(guard);
        assert_eq!(
            fs::read(&target).unwrap(),
            if after { b"new" } else { b"old" }
        );
        assert!(
            matches!(err,AdapterError::Publication { visibility, .. } if visibility == if after {transaction::Visibility::Published} else {transaction::Visibility::NotPublished})
        );
    }
}

#[test]
fn reused_objects_require_publication_confirmation_before_index_reference() {
    // A shared-custody writer can observe a peer's new link before the peer
    // confirms its directory sync. Simulate the abandoned publication, then
    // require a retry to refuse a failed confirmation of that existing inode.
    for (directory, publication) in [("chunks", 1), ("manifests", 2)] {
        for stage in ["file_sync", "publication_sync"] {
            let f = Fixture::new();
            let source = b"shared immutable snapshot bytes";
            let guard = faults::install("link", None, true, publication, Action::Error);
            assert!(vault::snapshot_data(
                source,
                &f.source(),
                Provider::Codex,
                "s",
                None,
                &f.root(),
            )
            .is_err());
            drop(guard);
            let object = fs::read_dir(f.root().join(directory))
                .unwrap()
                .map(Result::unwrap)
                .map(|entry| entry.path())
                .find(|path| path.file_name().unwrap().len() == 64)
                .unwrap();
            let bytes = fs::read(&object).unwrap();
            assert!(!f.root().join("index.jsonl").exists());

            let guard = faults::install(stage, Some(&object), false, 1, Action::Error);
            let retry =
                vault::snapshot_data(source, &f.source(), Provider::Codex, "s", None, &f.root());
            drop(guard);
            assert!(retry.is_err(), "reused {directory} skipped {stage}");
            assert!(!f.root().join("index.jsonl").exists());
            assert_eq!(fs::read(object).unwrap(), bytes);

            let recovered =
                vault::snapshot_data(source, &f.source(), Provider::Codex, "s", None, &f.root())
                    .unwrap();
            assert_eq!(vault::list(&f.root()).unwrap().len(), 1);
            assert_eq!(
                vault::read_object(&recovered.sha256, &f.root()).unwrap(),
                source,
            );
        }
    }
}

#[test]
fn prepared_output_recovers_exactly_after_source_changes_and_index_retirement() {
    let f = Fixture::new();
    let (handle, hash, plan) = f.setup();
    let guard = faults::install("intent_prepared", None, true, 1, Action::Error);
    assert!(copy::compact(&handle, &hash, &plan, &f.root()).is_err());
    drop(guard);
    let (id, _, pending) = operation(&f.root());
    assert!(!pending.completed && !pending.path.exists());
    let exact = vault::read_object(
        pending.output_manifest_sha256.as_deref().unwrap(),
        &f.root(),
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(f.source())
        .unwrap()
        .write_all(b"\nprovider appended\n")
        .unwrap();
    let changed = fs::read(f.source()).unwrap();
    vault::prune(&f.root(), 0, false).unwrap();
    assert!(vault::list(&f.root()).unwrap().is_empty());
    let completed = copy::compact(&handle, &hash, &plan, &f.root()).unwrap();
    assert_eq!(fs::read(&completed.path).unwrap(), exact);
    assert_eq!(fs::read(f.source()).unwrap(), changed);
    let before = inventory(&f.root());
    assert!(copy::recover_operation(&id, &f.root()).unwrap().completed);
    assert_eq!(inventory(&f.root()), before);
}

#[test]
fn completed_publication_does_not_overwrite_conflicting_output() {
    let f = Fixture::new();
    let (handle, hash, plan) = f.setup();
    let receipt = copy::compact(&handle, &hash, &plan, &f.root()).unwrap();
    let (id, _, _) = operation(&f.root());
    fs::write(&receipt.path, b"foreign data").unwrap();
    let before = inventory(&f.root());
    assert!(copy::recover_operation(&id, &f.root()).is_err());
    assert_eq!(fs::read(&receipt.path).unwrap(), b"foreign data");
    assert_eq!(inventory(&f.root()), before);
}

#[test]
fn damaged_roots_never_authorize_deletion_or_append() {
    for damage in 0..5 {
        let f = Fixture::new();
        let (h, _, _) = f.setup();
        let first = vault::snapshot(&h.path, h.provider, &h.session_id, None, &f.root()).unwrap();
        let index = f.root().join("index.jsonl");
        match damage {
            0 => {
                let mut bytes = fs::read(&index).unwrap();
                bytes.pop();
                fs::write(&index, bytes).unwrap();
            }
            1 => {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&index)
                    .unwrap()
                    .write_all(b"{broken}\n")
                    .unwrap();
            }
            2 => {
                let mut invalid = serde_json::to_value(&first).unwrap();
                invalid["strategy"] = json!("x".repeat(129));
                fs::OpenOptions::new()
                    .append(true)
                    .open(&index)
                    .unwrap()
                    .write_all(format!("{invalid}\n").as_bytes())
                    .unwrap();
            }
            3 => {
                fs::create_dir(f.root().join("operations")).unwrap();
                fs::write(
                    f.root()
                        .join("operations")
                        .join(format!("{}.json", "a".repeat(64))),
                    b"{broken}",
                )
                .unwrap();
            }
            _ => {
                fs::write(
                    f.root().join("manifests").join(".gobstopper-abandoned.tmp"),
                    b"unclassified evidence",
                )
                .unwrap();
            }
        }
        let before = inventory(&f.root());
        assert!(vault::prune(&f.root(), 0, false).is_err());
        assert_eq!(inventory(&f.root()), before);
        if damage < 3 {
            assert!(vault::snapshot(&h.path, h.provider, &h.session_id, None, &f.root()).is_err());
            assert_eq!(inventory(&f.root()), before);
        }
        assert!(vault::read_object(&first.sha256, &f.root()).is_ok());
    }
}

#[test]
fn duplicate_recovery_fields_never_authorize_deletion() {
    for field in [
        "snapshot_manifest_sha256",
        "output_manifest_sha256",
        "revision",
    ] {
        let f = Fixture::new();
        let (h, hash, plan) = f.setup();
        copy::compact(&h, &hash, &plan, &f.root()).unwrap();
        let (_, path, _) = operation(&f.root());
        let raw = fs::read_to_string(&path).unwrap();
        let value = if field == "revision" {
            "2".to_string()
        } else {
            format!("\"{}\"", "a".repeat(64))
        };
        let needle = format!("\"{field}\":");
        let duplicate = raw.replacen(&needle, &format!("{needle}{value},{needle}"), 1);
        assert_ne!(duplicate, raw);
        fs::write(&path, duplicate).unwrap();
        let before = inventory(&f.root());
        assert!(vault::prune(&f.root(), 0, false).is_err(), "{field}");
        assert_eq!(inventory(&f.root()), before, "{field}");
    }
    for version in [2, 3] {
        let f = Fixture::new();
        let entry =
            vault::snapshot_data(b"old", &f.source(), Provider::Codex, "s", None, &f.root())
                .unwrap();
        let raw = if version == 3 {
            fs::read_to_string(f.root().join("manifests").join(&entry.sha256)).unwrap()
        } else {
            fs::create_dir(f.root().join("records")).unwrap();
            fs::write(f.root().join("records").join(&entry.source_sha256), b"old").unwrap();
            json!({"schema_version":2,"records":[entry.source_sha256],"source_sha256":entry.source_sha256,"bytes":3}).to_string()
        };
        let duplicate = raw.replacen(
            "\"source_sha256\":",
            &format!(
                "\"source_sha256\":\"{}\",\"source_sha256\":",
                "a".repeat(64)
            ),
            1,
        );
        let bad_sha = copy::sha256(duplicate.as_bytes());
        fs::write(f.root().join("manifests").join(&bad_sha), duplicate).unwrap();
        let before = inventory(&f.root());
        assert!(vault::read_object(&bad_sha, &f.root()).is_err());
        assert!(vault::prune(&f.root(), 0, false).is_err());
        assert_eq!(inventory(&f.root()), before);
    }
}

#[test]
#[cfg(unix)]
fn private_directory_creation_preserves_existing_modes() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o750)).unwrap();
    transaction::private_dir(&f.0).unwrap();
    assert_eq!(
        fs::metadata(&f.0).unwrap().permissions().mode() & 0o777,
        0o750
    );
    transaction::private_dir(&f.root()).unwrap();
    assert_eq!(
        fs::metadata(f.root()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&f.0).unwrap().permissions().mode() & 0o777,
        0o750
    );
}

#[test]
fn failed_manifest_unlink_never_collects_its_chunks() {
    for after in [false, true] {
        let f = Fixture::new();
        let entry =
            vault::snapshot_data(b"old", &f.source(), Provider::Codex, "s", None, &f.root())
                .unwrap();
        let manifest = f.root().join("manifests").join(&entry.sha256);
        let guard = faults::install("unlink", Some(&manifest), after, 1, Action::Error);
        let err = vault::prune(&f.root(), 0, false).unwrap_err();
        drop(guard);
        let incomplete = err.downcast_ref::<vault::PruneIncomplete>().unwrap();
        assert_eq!(incomplete.report.chunks_removed, 0);
        assert_eq!(incomplete.report.manifests_removed, usize::from(after));
        assert_eq!(fs::read_dir(f.root().join("chunks")).unwrap().count(), 1);
        if !after {
            assert_eq!(
                vault::read_object(&entry.sha256, &f.root()).unwrap(),
                b"old"
            );
        }
    }
}

#[test]
fn retention_uses_append_order_and_exact_store_identity() {
    let f = Fixture::new();
    let root = f.root();
    let mut first = vault::snapshot_data(
        b"first",
        &f.0.join("a"),
        Provider::Codex,
        "same",
        None,
        &root,
    )
    .unwrap();
    let mut second = vault::snapshot_data(
        b"second",
        &f.0.join("a"),
        Provider::Codex,
        "same",
        None,
        &root,
    )
    .unwrap();
    let other = vault::snapshot_data(
        b"other",
        &f.0.join("b"),
        Provider::Codex,
        "same",
        None,
        &root,
    )
    .unwrap();
    first.ts = 999;
    second.ts = 1;
    fs::write(
        root.join("index.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap(),
            serde_json::to_string(&other).unwrap()
        ),
    )
    .unwrap();
    vault::prune(&root, 1, false).unwrap();
    let entries = vault::list(&root).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|e| e.sha256 == second.sha256));
    assert!(entries.iter().any(|e| e.sha256 == other.sha256));
}

#[test]
fn storage_child() {
    let Some(dir) = std::env::var_os("GOBSTOPPER_STORAGE_TEST_ROOT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let (handle, hash, plan) = setup(&dir);
    let root = dir.join("vault");
    if std::env::var("GOBSTOPPER_STORAGE_TEST_STAGE").as_deref() == Ok("reader_hold") {
        let _reader = vault::Reader::open(&root).unwrap();
        fs::write(dir.join("reader-ready"), b"ready").unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        panic!("reader fixture input closed before termination");
    }
    let stage = match std::env::var("GOBSTOPPER_STORAGE_TEST_STAGE")
        .unwrap()
        .as_str()
    {
        "partial_write" => "partial_write",
        "link" => "link",
        "intent_prepared" => "intent_prepared",
        "output_published" => "output_published",
        "receipt_completed" => "receipt_completed",
        "prune_index_replaced" => "prune_index_replaced",
        _ => panic!("unknown fixture checkpoint"),
    };
    let _guard = faults::install(stage, None, true, 1, Action::Kill);
    if stage == "prune_index_replaced" {
        vault::prune(&root, 0, false).unwrap();
    } else {
        copy::compact(&handle, &hash, &plan, &root).unwrap();
    }
    panic!("fixture did not reach checkpoint");
}

#[test]
#[cfg(unix)]
fn process_death_releases_custody_and_preserves_pins_or_requires_repair() {
    use std::os::unix::process::ExitStatusExt;
    for stage in [
        "partial_write",
        "link",
        "intent_prepared",
        "output_published",
        "receipt_completed",
        "prune_index_replaced",
    ] {
        let f = Fixture::new();
        let (h, hash, plan) = f.setup();
        let source = fs::read(f.source()).unwrap();
        if stage == "prune_index_replaced" {
            copy::compact(&h, &hash, &plan, &f.root()).unwrap();
        }
        let mut command = Command::new(std::env::current_exe().unwrap());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GOBSTOPPER_") {
                command.env_remove(key);
            }
        }
        let mut child = command
            .args(["--exact", "storage_tests::storage_child", "--nocapture"])
            .env("GOBSTOPPER_STORAGE_TEST_ROOT", &f.0)
            .env("GOBSTOPPER_STORAGE_TEST_STAGE", stage)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("fixture deadline {stage}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{stage}");
        assert_eq!(fs::read(f.source()).unwrap(), source);
        if f.root().exists() {
            let lock = fs::File::open(f.root()).unwrap();
            fs2::FileExt::try_lock_exclusive(&lock).unwrap();
            drop(lock);
        }
        if matches!(
            stage,
            "intent_prepared" | "output_published" | "receipt_completed" | "prune_index_replaced"
        ) {
            let (id, _, pending) = operation(&f.root());
            let exact = vault::read_object(
                pending.output_manifest_sha256.as_deref().unwrap(),
                &f.root(),
            )
            .unwrap();
            let recovered = copy::recover_operation(&id, &f.root()).unwrap();
            assert!(recovered.completed);
            assert_eq!(fs::read(recovered.path).unwrap(), exact);
            vault::prune(&f.root(), 0, false).unwrap();
            assert!(copy::recover_operation(&id, &f.root()).unwrap().completed);
        } else {
            let before = inventory(&f.root());
            assert!(vault::prune(&f.root(), 0, false).is_err());
            assert_eq!(inventory(&f.root()), before);
        }
    }
}

#[test]
fn bounded_read_refuses_special_files_and_overflowing_configuration() {
    assert_eq!(
        transaction::parse_limit(Some("18446744073709551615")),
        transaction::DEFAULT_MAX_TRANSCRIPT_BYTES
    );
    assert_eq!(
        transaction::parse_limit(Some("1023")),
        transaction::DEFAULT_MAX_TRANSCRIPT_BYTES
    );
    assert_eq!(transaction::parse_limit(Some("1024")), 1024);
    let f = Fixture::new();
    let file = f.0.join("regular");
    fs::write(&file, b"abc").unwrap();
    assert!(transaction::read_with_limit(&file, 2).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};
        let link = f.0.join("link");
        symlink(&file, &link).unwrap();
        assert!(transaction::read(&link).is_err());
        let fifo = f.0.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // Exact synthetic fixture path, mode0600, valid NUL-terminated string.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(transaction::read(&fifo).is_err()); // O_NONBLOCK prevents a hang.
    }
    assert!(fork::fork(Provider::Codex, &f.0.join("missing"), None).is_err());
    assert!(!f.root().exists());
}

#[test]
fn torn_index_append_reports_unknown_effect_and_blocks_collection() {
    let f = Fixture::new();
    let (h, _, _) = f.setup();
    let snapshot = vault::snapshot(&h.path, h.provider, &h.session_id, None, &f.root()).unwrap();
    let index = f.root().join("index.jsonl");
    let guard = faults::install("partial_write", Some(&index), true, 1, Action::Error);
    let err = vault::snapshot(&h.path, h.provider, &h.session_id, None, &f.root()).unwrap_err();
    drop(guard);
    assert!(matches!(
        err.downcast_ref::<AdapterError>(),
        Some(AdapterError::Publication {
            visibility: transaction::Visibility::Unknown,
            ..
        })
    ));
    let before = inventory(&f.root());
    assert!(vault::prune(&f.root(), 0, false).is_err());
    assert_eq!(inventory(&f.root()), before);
    assert!(vault::read_object(&snapshot.sha256, &f.root()).is_ok());
}

#[test]
fn native_recovery_pin_is_idempotent_and_never_automatically_retired() {
    let f = Fixture::new();
    let source = vault::snapshot_data(
        b"source",
        &f.source(),
        Provider::Codex,
        "s",
        None,
        &f.root(),
    )
    .unwrap();
    let other =
        vault::snapshot_data(b"other", &f.source(), Provider::Codex, "s", None, &f.root()).unwrap();
    let id = "a".repeat(64);
    vault::retain_operation_snapshot(&id, &source.sha256, &f.root()).unwrap();
    vault::retain_operation_snapshot(&id, &source.sha256, &f.root()).unwrap();
    assert!(vault::retain_operation_snapshot(&id, &other.sha256, &f.root()).is_err());
    vault::prune(&f.root(), 0, false).unwrap();
    assert_eq!(
        vault::read_object(&source.sha256, &f.root()).unwrap(),
        b"source"
    );
    fs::write(
        f.root().join("pins").join(format!("native-{id}.json")),
        b"{}",
    )
    .unwrap();
    let before = inventory(&f.root());
    assert!(vault::prune(&f.root(), 0, false).is_err());
    assert_eq!(inventory(&f.root()), before);
}

#[test]
fn legacy_completed_receipt_reconciles_after_source_change_and_pruning() {
    let f = Fixture::new();
    let (h, hash, plan) = f.setup();
    let original = fs::read(&h.path).unwrap();
    let modern = copy::compact(&h, &hash, &plan, &f.root()).unwrap();
    let (_, modern_path, _) = operation(&f.root());
    let canonical = h.path.canonicalize().unwrap();
    let identity =
        copy::sha256(&serde_json::to_vec(&(h.provider, &canonical, &hash, &plan.edits)).unwrap());
    let session = format!(
        "{}-{}-4{}-a{}-{}",
        &identity[..8],
        &identity[8..12],
        &identity[13..16],
        &identity[17..20],
        &identity[20..32]
    );
    let mut legacy = modern;
    legacy.schema_version = 1;
    legacy.operation = None;
    legacy.output_manifest_sha256 = None;
    legacy.snapshot_manifest_sha256 = None;
    legacy.session_id = session.clone();
    legacy.path = fork::target_path_bound(h.provider, &canonical, &h.session_id, &session);
    let bytes = fork::rewrite_identity(
        h.provider,
        std::str::from_utf8(&original).unwrap(),
        &session,
    )
    .into_bytes();
    legacy.output_sha256 = copy::sha256(&bytes);
    legacy.bytes_after = bytes.len() as u64;
    legacy.reclaimed_bytes = legacy.bytes_before.saturating_sub(legacy.bytes_after);
    fs::write(&legacy.path, &bytes).unwrap();
    fs::remove_file(modern_path).unwrap();
    fs::write(
        f.root().join("operations").join(format!("{identity}.json")),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    fs::write(&h.path, b"provider changed the source").unwrap();
    vault::prune(&f.root(), 0, false).unwrap();
    vault::prune(&f.root(), 0, false).unwrap();
    let recovered = copy::compact(&h, &hash, &plan, &f.root()).unwrap();
    assert_eq!(recovered.path, legacy.path);
    assert_eq!(fs::read(&recovered.path).unwrap(), bytes);
    assert_eq!(fs::read(&h.path).unwrap(), b"provider changed the source");
}

#[test]
#[cfg(unix)]
fn independent_reader_process_excludes_prune_until_collected() {
    let f = Fixture::new();
    let (h, _, _) = f.setup();
    vault::snapshot(&h.path, h.provider, &h.session_id, None, &f.root()).unwrap();
    let before = inventory(&f.root());
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GOBSTOPPER_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .args(["--exact", "storage_tests::storage_child", "--nocapture"])
        .env("GOBSTOPPER_STORAGE_TEST_ROOT", &f.0)
        .env("GOBSTOPPER_STORAGE_TEST_STAGE", "reader_hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !f.0.join("reader-ready").exists() {
        if child.try_wait().unwrap().is_some() {
            panic!("reader fixture exited");
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("reader fixture deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let exclusive = fs::File::open(f.root()).unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&exclusive).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    fs2::FileExt::try_lock_exclusive(&exclusive).unwrap();
    drop(exclusive);
    assert_eq!(inventory(&f.root()), before);
    vault::prune(&f.root(), 1, false).unwrap();
}

//! Bounded deterministic command-sequence evidence; not a durability proof.
use gobstopper_adapters::{copy, vault};
use gobstopper_core::{CompactionPlan, Edit, Provider, SessionHandle};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SEED: u64 = 0x6a09_e667_f3bc_c909;
const STEPS: usize = 64;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FILES: u64 = 1200;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gobstopper-sequence-{}-{SEED:x}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn footprint(path: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    for entry in fs::read_dir(path).unwrap().map(Result::unwrap) {
        let metadata = entry.metadata().unwrap();
        if metadata.is_dir() {
            let (f, b) = footprint(&entry.path());
            files += f;
            bytes += b;
        } else {
            assert!(metadata.is_file());
            files += 1;
            bytes += metadata.len();
        }
    }
    (files, bytes)
}

#[test]
fn bounded_snapshot_copy_prune_corruption_and_recovery_sequence() {
    let fixture = Fixture::new();
    let source = fixture.0.join("rollout-sequence.jsonl");
    let root = fixture.0.join("vault");
    let started = Instant::now();
    let mut seed = SEED;
    let mut retained = Vec::new();
    let mut receipts = Vec::new();
    let mut peak_files = 0;
    let mut peak_bytes = 0;
    let mut corruptions = 0;
    for step in 0..STEPS {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let bytes = format!("{}\n{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":"sequence"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"tool","name":"read","arguments":"{}"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"tool","output":format!("generation {} {}",step / 8,"synthetic-content ".repeat(1200))}})
        ).into_bytes();
        // Only this isolated fixture simulates the provider changing its own
        // bytes; every Gobstopper command below must preserve this exact value.
        fs::write(&source, &bytes).unwrap();
        fs::File::open(&source)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH
                    + Duration::from_secs(if step % 2 == 0 { 1 } else { 1_000_000 }),
            ))
            .unwrap();
        let handle = SessionHandle {
            provider: Provider::Codex,
            session_id: "sequence".into(),
            path: source.canonicalize().unwrap(),
            cwd: None,
            age_secs: u64::MAX,
        };
        let snapshot = vault::snapshot(
            &source,
            Provider::Codex,
            "sequence",
            Some("sequence"),
            &root,
        )
        .unwrap();
        let pin = copy::sha256(format!("sequence:{SEED}:{step}").as_bytes());
        vault::retain_operation_snapshot(&pin, &snapshot.sha256, &root).unwrap();
        retained.push((snapshot.sha256.clone(), bytes.clone()));
        let plan = CompactionPlan {
            strategy: "elide".into(),
            rationale: "bounded fixture".into(),
            context_tokens_before: 6000,
            context_tokens_after: 100,
            edits: vec![Edit::Elide {
                line_indexes: vec![2],
                stub_template: format!("[sequence item {step} elided]"),
                per_item_stubs: Default::default(),
            }],
        };
        let receipt = copy::compact(&handle, &copy::sha256(&bytes), &plan, &root).unwrap();
        let output = fs::read(&receipt.path).unwrap();
        let operation =
            copy::sha256(&serde_json::to_vec(receipt.operation.as_ref().unwrap()).unwrap());
        assert!(receipt.completed);
        assert_eq!(copy::sha256(&output), receipt.output_sha256);
        receipts.push((operation, receipt.path, output));

        // Interleave live collection with retained operation/recovery roots.
        vault::prune(&root, (seed as usize) % 4, false).unwrap();
        let selected = (seed as usize) % retained.len();
        let (sha, expected) = &retained[selected];
        assert_eq!(vault::read_object(sha, &root).unwrap(), *expected);
        let (operation, path, expected) = &receipts[(seed as usize) % receipts.len()];
        assert_eq!(
            copy::recover_operation(operation, &root).unwrap().path,
            *path
        );
        assert_eq!(fs::read(path).unwrap(), *expected);

        if step % 8 == 3 {
            let manifest: serde_json::Value = serde_json::from_slice(
                &fs::read(root.join("manifests").join(&snapshot.sha256)).unwrap(),
            )
            .unwrap();
            let chunk = root
                .join("chunks")
                .join(manifest["chunks"][0].as_str().unwrap());
            let intact = fs::read(&chunk).unwrap();
            fs::write(&chunk, b"injected-corruption").unwrap();
            assert!(vault::read_object(&snapshot.sha256, &root).is_err());
            assert!(vault::prune(&root, 0, false).is_err());
            fs::write(&chunk, intact).unwrap();
            corruptions += 1;
        }
        if step % 8 == 7 {
            let index = root.join("index.jsonl");
            let intact = fs::read(&index).unwrap();
            fs::write(&index, b"{\"torn\":").unwrap();
            assert!(vault::list(&root).is_err());
            assert!(vault::prune(&root, 0, false).is_err());
            assert_eq!(fs::read(&index).unwrap(), b"{\"torn\":");
            fs::write(&index, intact).unwrap();
            corruptions += 1;
        }
        assert_eq!(
            fs::read(&source).unwrap(),
            bytes,
            "seed {SEED}, step {step}"
        );
        let (files, bytes) = footprint(&fixture.0);
        peak_files = peak_files.max(files);
        peak_bytes = peak_bytes.max(bytes);
        assert!(
            files <= MAX_FILES && bytes <= MAX_BYTES,
            "seed {SEED}, step {step}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "bounded sequence deadline"
        );
    }
    vault::prune(&root, 0, false).unwrap();
    assert!(vault::list(&root).unwrap().is_empty());
    for (sha, expected) in retained {
        assert_eq!(vault::read_object(&sha, &root).unwrap(), expected);
    }
    for (operation, path, expected) in receipts {
        assert!(
            copy::recover_operation(&operation, &root)
                .unwrap()
                .completed
        );
        assert_eq!(fs::read(path).unwrap(), expected);
    }
    assert_eq!(corruptions, 16);
    eprintln!("sequence receipt: seed={SEED}, steps={STEPS}, corruption_recoveries={corruptions}, peak_files={peak_files}, peak_bytes={peak_bytes}, elapsed_ms={}", started.elapsed().as_millis());
}

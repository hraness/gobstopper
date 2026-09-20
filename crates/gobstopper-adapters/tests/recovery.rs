use gobstopper_adapters::{codex, copy, recovery, vault};
use gobstopper_core::{plan::DigestBlock, Edit, Provider};
use serde_json::json;
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "gobstopper-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn snapshot(&self, bytes: &[u8]) -> vault::VaultEntry {
        let path = self.0.join("session.jsonl");
        fs::write(&path, bytes).unwrap();
        vault::snapshot(
            &path,
            Provider::Codex,
            "synthetic",
            None,
            &self.0.join("vault"),
        )
        .unwrap()
    }
    fn root(&self) -> PathBuf {
        self.0.join("vault")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn search_finds_decoded_nested_values_without_disclosing_content() {
    let f = Fixture::new();
    let source = b"{\"payload\":{\"history\":[{\"text\":\"caf\\u00e9 secret secret\"}]}}\n{\"secret\":4}\nnot-json\n\n42\n";
    let s = f.snapshot(source);
    let found = recovery::search_snapshot(&s.sha256, "café", 20, &f.root()).unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].record_index, 0);
    assert_eq!(found.unsearchable_records, 2);
    let serialized = serde_json::to_string(&found).unwrap();
    assert!(!serialized.contains("café") && !serialized.contains("secret"));
    assert_eq!(found.source_sha256, copy::sha256(source));
    let found = recovery::search_snapshot(&s.sha256, "secret", 20, &f.root()).unwrap();
    assert_eq!(found.matched_records, 1); // object keys are not string values
    assert_eq!(found.matches[0].match_count, 2);
    assert!(
        recovery::search_snapshot(&s.sha256, "absent", 20, &f.root())
            .unwrap()
            .matches
            .is_empty()
    );
    assert_eq!(fs::read(f.0.join("session.jsonl")).unwrap(), source);
}

#[test]
fn search_reports_overflow_and_rejects_unbounded_inputs() {
    let f = Fixture::new();
    let s = f.snapshot(b"\"needle\"\n\"needle\"\n");
    let found = recovery::search_snapshot(&s.sha256, "needle", 1, &f.root()).unwrap();
    assert_eq!(found.matched_records, 2);
    assert_eq!(found.matches.len(), 1);
    assert!(found.truncated);
    for q in ["".to_string(), "x".repeat(1025)] {
        assert!(recovery::search_snapshot(&s.sha256, &q, 1, &f.root()).is_err());
    }
    for limit in [0, 51, usize::MAX] {
        assert!(recovery::search_snapshot(&s.sha256, "needle", limit, &f.root()).is_err());
    }
    for sha in ["../objects/secret", &s.sha256[..16], &s.source_sha256] {
        assert!(recovery::search_snapshot(sha, "needle", 1, &f.root()).is_err());
    }
}

#[test]
fn paginated_unicode_read_reconstructs_exact_record_and_advances() {
    let f = Fixture::new();
    let line = "{\"text\":\"🙂café𐀀東京\"}";
    let s = f.snapshot(format!("{line}\n").as_bytes());
    let mut offset = 0;
    let mut reconstructed = String::new();
    loop {
        let page = recovery::read_snapshot_record(&s.sha256, 0, offset, 4, &f.root()).unwrap();
        assert!(page.content.len() <= 4);
        assert_eq!(page.record_sha256, copy::sha256(line.as_bytes()));
        assert_eq!(page.record_bytes, line.len());
        assert_eq!(page.content_trust, "untrusted_archived_data");
        reconstructed.push_str(&page.content);
        match page.next_offset {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    assert_eq!(reconstructed, line);
    let start = line.find('🙂').unwrap();
    assert!(recovery::read_snapshot_record(&s.sha256, 0, start + 1, 4, &f.root()).is_err());
    for (record, offset, max) in [(1, 0, 4), (0, line.len() + 1, 4), (0, 0, 3), (0, 0, 16385)] {
        assert!(recovery::read_snapshot_record(&s.sha256, record, offset, max, &f.root()).is_err());
    }
}

#[test]
fn record_crosses_chunk_boundary_and_corruption_fails_closed() {
    let f = Fixture::new();
    let line = format!("{{\"text\":\"{}needle\"}}", "x".repeat(1024 * 1024));
    let s = f.snapshot(line.as_bytes());
    let page = recovery::read_snapshot_record(&s.sha256, 0, 1024 * 1024, 64, &f.root()).unwrap();
    assert_eq!(page.content.as_bytes(), &line.as_bytes()[1024 * 1024..]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(f.root().join("manifests").join(&s.sha256)).unwrap())
            .unwrap();
    let chunk = manifest["chunks"][0].as_str().unwrap();
    fs::write(f.root().join("chunks").join(chunk), b"corrupted").unwrap();
    assert!(recovery::read_snapshot_record(&s.sha256, 0, 0, 4096, &f.root()).is_err());
    assert!(recovery::search_snapshot(&s.sha256, "needle", 1, &f.root()).is_err());
}

#[test]
fn legacy_full_object_and_empty_snapshot_are_supported() {
    let f = Fixture::new();
    let root = f.root();
    fs::create_dir_all(root.join("objects")).unwrap();
    let bytes = b"{\"text\":\"needle\"}";
    let sha = copy::sha256(bytes);
    fs::write(root.join("objects").join(&sha), bytes).unwrap();
    assert_eq!(
        recovery::search_snapshot(&sha, "needle", 1, &root)
            .unwrap()
            .matched_records,
        1
    );
    assert_eq!(
        recovery::read_snapshot_record(&sha, 0, 0, 4096, &root)
            .unwrap()
            .content
            .as_bytes(),
        bytes
    );
    let empty = f.snapshot(b"");
    assert_eq!(
        recovery::search_snapshot(&empty.sha256, "x", 1, &root)
            .unwrap()
            .matched_records,
        0
    );
    assert!(recovery::read_snapshot_record(&empty.sha256, 0, 0, 4096, &root).is_err());
}

#[test]
fn default_codex_writer_cards_are_recalled_in_every_field() {
    let f = Fixture::new();
    let path = f.0.join("session.jsonl");
    fs::write(
        &path,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"synthetic\"}}\n",
    )
    .unwrap();
    let digest = DigestBlock {
        goal: Some("goal-key".into()),
        summary: Some("summary-key".into()),
        concepts: vec!["concept-key".into()],
        files_touched: vec!["file-key".into()],
        decisions: vec!["decision-key".into()],
        errors: vec!["error-key".into()],
        open_tasks: vec!["todo-key".into()],
        current_work: Some("current-key".into()),
        context: Some("context-key".into()),
        covers_items: 9,
    };
    codex::apply(
        &path,
        &[Edit::InjectDigest {
            digest: digest.clone(),
        }],
    )
    .unwrap();
    let snap = vault::snapshot(&path, Provider::Codex, "synthetic", None, &f.root()).unwrap();
    for query in [
        "goal-key",
        "summary-key",
        "concept-key",
        "file-key",
        "decision-key",
        "error-key",
        "todo-key",
        "current-key",
        "context-key",
    ] {
        let rows = vault::recall("synthetic", Some(query), Some(&snap.sha256), &f.root()).unwrap();
        assert_eq!(rows.len(), 1, "missing {query}");
        assert_eq!(rows[0].digest, digest);
    }
}

#[test]
fn lookalike_tool_and_assistant_cards_are_not_recalled() {
    let f = Fixture::new();
    let marker = "[gobstopper state card]\ngoal: false-positive\n";
    let rows = [
        json!({"type":"response_item","payload":{"type":"function_call_output","output":marker}}),
        json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"input_text","text":marker}]}}),
        json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("quoted {marker}")}]}}),
    ];
    let source = rows.iter().map(|v| format!("{v}\n")).collect::<String>();
    f.snapshot(source.as_bytes());
    assert!(vault::recall("*", None, None, &f.root())
        .unwrap()
        .is_empty());
}

#[test]
fn legacy_snapshot_with_excess_records_is_rejected_before_expansion() {
    let f = Fixture::new();
    let root = f.root();
    fs::create_dir_all(root.join("objects")).unwrap();
    let data = vec![b'\n'; gobstopper_core::validation::MAX_ITEMS + 3];
    let sha = copy::sha256(&data);
    fs::write(root.join("objects").join(&sha), data).unwrap();
    assert!(recovery::search_snapshot(&sha, "x", 1, &root)
        .unwrap_err()
        .to_string()
        .contains("record limit"));
    assert!(recovery::read_snapshot_record(&sha, 0, 0, 4096, &root)
        .unwrap_err()
        .to_string()
        .contains("record limit"));
}

#[path = "../src/session_data/importers.rs"]
pub mod importers;
#[path = "../src/session_data/metrics.rs"]
pub mod metrics;
#[path = "../src/session_data/schema.rs"]
#[allow(dead_code)]
pub mod schema;
#[path = "../src/session_data/store.rs"]
#[allow(dead_code)]
pub mod store;
mod session_data {
    pub use crate::importers;
    pub use crate::schema::*;
    pub use crate::store::{prepare_private_dir, Query, Store};
}

use session_data::{
    Basis, Envelope, Event, Generation, Identity, OpaqueId, Outcome, Quantity, Query, Source,
    SourceKind, Store, Usage,
};
use std::io::Cursor;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "gobstopper-data-test-{}",
            OpaqueId::random().unwrap().0
        ));
        session_data::prepare_private_dir(&path).unwrap();
        Self(path)
    }
    fn database(&self) -> PathBuf {
        self.0.join("sessions.sqlite3")
    }
    fn store(&self) -> Store {
        Store::open(&self.database()).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start() -> Envelope {
    Envelope::new(
        Source {
            kind: SourceKind::LiveProxy,
            id: OpaqueId::random().unwrap(),
            profile: "gobstopper-proxy-v1".into(),
        },
        Identity {
            runtime_id: Some(OpaqueId::random().unwrap()),
            session_id: Some(OpaqueId::random().unwrap()),
            request_id: Some(OpaqueId::random().unwrap()),
            attempt_id: Some(OpaqueId::random().unwrap()),
            tool_id: None,
        },
        Event::RequestStarted {
            provider: "anthropic".into(),
            model: Some("claude-sonnet-4-6".into()),
        },
    )
    .unwrap()
}
fn finish(start: &Envelope) -> Envelope {
    Envelope::new(
        start.source.clone(),
        start.identity.clone(),
        Event::RequestFinished {
            outcome: Outcome::Success,
            http_status: Some(200),
            duration_ms: Some(10_000),
            first_output_ms: Some(1000),
            generation: None,
            usage: Some(Usage {
                input_tokens: Some(Quantity::reported(100)),
                output_tokens: Some(Quantity::reported(50)),
                ..Usage::default()
            }),
        },
    )
    .unwrap()
}

#[test]
fn committed_start_survives_restart_and_replay_is_idempotent() {
    let temp = Temp::new();
    let begun = start();
    let mut store = temp.store();
    assert_eq!(
        store
            .append_batch(std::slice::from_ref(&begun))
            .unwrap()
            .inserted,
        1
    );
    drop(store);
    let mut store = temp.store();
    assert_eq!(store.status().unwrap().incomplete_attempts, 1);
    let replay = store.append_batch(std::slice::from_ref(&begun)).unwrap();
    assert_eq!(replay.duplicates, 1);
    assert_eq!(replay.revision, 1);
    store.append_batch(&[finish(&begun)]).unwrap();
    let status = store.status().unwrap();
    assert_eq!(status.events, 2);
    assert_eq!(status.incomplete_attempts, 0);
    assert_eq!(status.revision, 2);
    assert_eq!(store.check().unwrap().checked_events, 2);
}

#[test]
fn conflicting_identity_rolls_back_entire_batch() {
    let temp = Temp::new();
    let mut store = temp.store();
    let begun = start();
    store.append_batch(std::slice::from_ref(&begun)).unwrap();
    let mut changed = begun.clone();
    changed.observed_at_ms += 1;
    assert!(store
        .append_batch(&[start(), changed])
        .unwrap_err()
        .to_string()
        .contains("identity_conflict"));
    assert_eq!(store.status().unwrap().events, 1);
    assert_eq!(store.status().unwrap().revision, 1);
    let mut duplicate_stage = begun;
    duplicate_stage.event_id = OpaqueId::random().unwrap();
    assert!(store
        .append_batch(&[duplicate_stage])
        .unwrap_err()
        .to_string()
        .contains("lifecycle_conflict"));
}

#[test]
fn portable_import_is_atomic_checked_and_idempotent() {
    let a = Temp::new();
    let b = Temp::new();
    let mut source = a.store();
    let begun = start();
    source
        .append_batch(&[begun.clone(), finish(&begun)])
        .unwrap();
    let mut bytes = Vec::new();
    source.export(&Query::default(), &mut bytes).unwrap();
    let mut destination = b.store();
    assert!(destination
        .import(Cursor::new(&bytes[..bytes.len() - 5]))
        .is_err());
    assert_eq!(destination.status().unwrap().events, 0);
    let mut corrupt = String::from_utf8(bytes.clone()).unwrap();
    corrupt = corrupt.replacen("claude-sonnet-4-6", "claude-sonnet-4-5", 1);
    assert!(destination
        .import(Cursor::new(corrupt))
        .unwrap_err()
        .to_string()
        .contains("checksum"));
    assert_eq!(destination.status().unwrap().events, 0);
    assert_eq!(destination.import(Cursor::new(&bytes)).unwrap().inserted, 2);
    assert_eq!(
        destination.import(Cursor::new(&bytes)).unwrap().duplicates,
        2
    );
    assert_eq!(
        source.events(&Query::default()).unwrap(),
        destination.events(&Query::default()).unwrap()
    );
    // Portable exports preserve opaque observations but omit the keyed namespace.
    assert_ne!(
        source.opaque("session", "raw-private-session"),
        destination.opaque("session", "raw-private-session")
    );
}

#[test]
fn future_schema_and_unknown_databases_are_refused() {
    let temp = Temp::new();
    let store = temp.store();
    drop(store);
    let conn = rusqlite::Connection::open(temp.database()).unwrap();
    conn.pragma_update(None, "user_version", 999).unwrap();
    drop(conn);
    assert!(Store::open(&temp.database())
        .err()
        .unwrap()
        .to_string()
        .contains("newer_than_binary"));
    let other = Temp::new();
    let conn = rusqlite::Connection::open(other.database()).unwrap();
    conn.execute_batch("CREATE TABLE user_data(secret TEXT);")
        .unwrap();
    drop(conn);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(other.database(), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(Store::open(&other.database())
        .err()
        .unwrap()
        .to_string()
        .contains("unknown_database"));
}

#[test]
fn additive_migration_preserves_observations_revision_and_namespace() {
    let temp = Temp::new();
    let mut store = temp.store();
    let event = start();
    store.append_batch(std::slice::from_ref(&event)).unwrap();
    let identity = store.opaque("session", "a");
    drop(store);
    let conn = rusqlite::Connection::open(temp.database()).unwrap();
    conn.execute_batch("DROP INDEX events_time; DROP INDEX events_session; DROP INDEX events_attempt; PRAGMA user_version=1;").unwrap();
    drop(conn);
    let store = temp.store();
    assert_eq!(store.status().unwrap().schema_version, 2);
    assert_eq!(store.status().unwrap().revision, 1);
    assert_eq!(store.opaque("session", "a"), identity);
    assert_eq!(store.events(&Query::default()).unwrap(), vec![event]);
    assert!(store.check().unwrap().ok);
}

#[test]
fn backup_includes_committed_wal_and_preserves_identity() {
    let temp = Temp::new();
    let target = Temp::new();
    let mut store = temp.store();
    store.append_batch(&[start()]).unwrap();
    let keyed = store.opaque("session", "private");
    let receipt = store.backup(&target.database()).unwrap();
    assert_eq!(receipt.checked_events, 1);
    let copy = target.store();
    assert_eq!(copy.opaque("session", "private"), keyed);
    assert!(store
        .backup(&target.database())
        .unwrap_err()
        .to_string()
        .contains("destination_exists"));
}

#[test]
fn metrics_keep_generation_and_request_rates_distinct_and_missing() {
    let first = start();
    let terminal = finish(&first);
    let pending = start();
    let report = metrics::metrics(&[first, terminal, pending]);
    let cohort = &report.cohorts[0];
    assert_eq!(cohort.attempts, 2);
    assert_eq!(cohort.incomplete_attempts, 1);
    assert_eq!(cohort.reported_output_tokens, "50");
    let request = cohort
        .measures
        .iter()
        .find(|m| m.id == "request_output_tokens_per_second")
        .unwrap();
    assert_eq!(request.value, Some(5.0));
    assert_eq!(request.missing, 1);
    assert_eq!(request.status, "partial");
    let generation = cohort
        .measures
        .iter()
        .find(|m| m.id == "generation_output_tokens_per_second")
        .unwrap();
    assert_eq!(generation.value, None);
    assert_eq!(
        generation.unavailable_reason,
        Some("no_matching_generation_spans")
    );
}

#[test]
fn matched_generation_and_reported_tokens_are_required_for_generation_rate() {
    let begun = start();
    let mut terminal = finish(&begun);
    if let Event::RequestFinished { generation, .. } = &mut terminal.event {
        *generation = Some(Generation {
            output_tokens: 40,
            duration_ms: 2000,
        });
    }
    terminal.validate().unwrap();
    let report = metrics::metrics(std::slice::from_ref(&terminal));
    assert_eq!(
        report.cohorts[0]
            .measures
            .iter()
            .find(|m| m.id == "generation_output_tokens_per_second")
            .unwrap()
            .value,
        Some(20.0)
    );
    if let Event::RequestFinished { usage, .. } = &mut terminal.event {
        usage.as_mut().unwrap().output_tokens = Some(Quantity {
            value: 50,
            basis: Basis::Estimated,
        });
    }
    assert!(terminal.validate().is_err());
}

#[test]
fn cohorts_and_unknown_session_do_not_double_count_or_invent_relationships() {
    let mut a = start();
    a.identity.session_id = None;
    let mut b = finish(&a);
    b.source.kind = SourceKind::NativeTranscript;
    b.source.profile = "claude-metadata-v1".into();
    b.source.id = OpaqueId::random().unwrap();
    let events = vec![a, b];
    let sessions = metrics::sessions(&events);
    assert!(sessions.sessions.is_empty());
    assert_eq!(sessions.attempts_without_session, 2);
    let report = metrics::metrics(&events);
    assert_eq!(report.cohorts.len(), 2);
    assert_eq!(report.cohorts[0].attempts, 1);
    assert_eq!(report.cohorts[1].attempts, 1);
}

#[test]
fn time_windows_are_half_open_and_missing_start_remains_missing() {
    let temp = Temp::new();
    let mut store = temp.store();
    let mut begun = start();
    begun.observed_at_ms = 100;
    let mut terminal = finish(&begun);
    terminal.observed_at_ms = 200;
    store.append_batch(&[begun, terminal]).unwrap();
    let selected = store
        .events(&Query {
            since_ms: Some(200),
            until_ms: Some(201),
            session_id: None,
        })
        .unwrap();
    assert_eq!(selected.len(), 1);
    let rows = metrics::requests(&selected);
    assert_eq!(rows[0].started_at_ms, None);
    assert!(!rows[0].incomplete);
    assert!(store
        .events(&Query {
            since_ms: Some(2),
            until_ms: Some(1),
            session_id: None
        })
        .is_err());
}

#[test]
fn legacy_records_preserve_estimates_without_inventing_usage_or_identity() {
    let temp = Temp::new();
    let mut store = temp.store();
    let source = store.opaque("legacy-source", "test-a");
    let line="{\"ts\":\"2026-09-30T12:00:00Z\",\"est_tokens_in\":123456,\"est_tokens_out\":98765,\"compacted\":true,\"path\":\"/private-do-not-copy\"}\n";
    let data = format!("{line}{line}");
    let receipt =
        session_data::importers::import_legacy(&mut store, source.clone(), Cursor::new(&data))
            .unwrap();
    assert_eq!(receipt.append.inserted, 2);
    assert_eq!(
        session_data::importers::import_legacy(&mut store, source, Cursor::new(&data))
            .unwrap()
            .append
            .duplicates,
        2
    );
    let events = store.events(&Query::default()).unwrap();
    assert_eq!(events[0].identity, Identity::default());
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("private-do-not-copy"));
    let report = metrics::metrics(&events);
    assert_eq!(report.cohorts[0].attempts, 0);
    assert_eq!(report.cohorts[0].legacy_observations, 2);
    assert_eq!(report.cohorts[0].reported_input_tokens, "0");
}

#[test]
fn codex_native_tools_keep_unknown_outcomes_and_strip_content() {
    let temp = Temp::new();
    let mut store = temp.store();
    let source=concat!(
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"PRIVATE_SESSION\",\"cwd\":\"/PRIVATE_PATH\"}}\n",
        "{\"timestamp\":\"2026-09-30T12:00:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"call_id\":\"PRIVATE_CALL\",\"name\":\"exec\",\"input\":\"PRIVATE_PROGRAM\"}}\n",
        "{\"timestamp\":\"2026-09-30T12:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call_output\",\"call_id\":\"PRIVATE_CALL\",\"output\":\"PRIVATE_OUTPUT\"}}\n");
    let receipt = session_data::importers::import_native(
        &mut store,
        session_data::importers::NativeProvider::Codex,
        Cursor::new(source),
    )
    .unwrap();
    assert_eq!(receipt.append.inserted, 2);
    let events = store.events(&Query::default()).unwrap();
    assert!(!serde_json::to_string(&events).unwrap().contains("PRIVATE"));
    let tools = metrics::tools(&events);
    assert_eq!(tools.len(), 1);
    assert!(tools[0].requested && tools[0].terminal);
    assert_eq!(tools[0].outcome, Some(Outcome::Unknown));
    assert_eq!(metrics::sessions(&events).sessions.len(), 1);
}

#[test]
fn claude_native_reads_text_users_and_only_explicit_terminal_usage() {
    let temp = Temp::new();
    let mut store = temp.store();
    let source=concat!(
        "{\"type\":\"user\",\"sessionId\":\"s\",\"timestamp\":\"2026-09-30T12:00:00Z\",\"message\":{\"content\":\"PRIVATE_USER_TEXT\"}}\n",
        "{\"type\":\"assistant\",\"sessionId\":\"s\",\"requestId\":\"req\",\"timestamp\":\"2026-09-30T12:00:01Z\",\"message\":{\"id\":\"m\",\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n",
        "{\"type\":\"assistant\",\"sessionId\":\"s\",\"requestId\":\"req\",\"timestamp\":\"2026-09-30T12:00:02Z\",\"message\":{\"id\":\"m\",\"stop_reason\":\"end_turn\",\"content\":[{\"type\":\"text\",\"text\":\"PRIVATE_OUTPUT\"}],\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"cache_read_input_tokens\":20,\"cache_creation_input_tokens\":0}}}\n");
    let receipt = session_data::importers::import_native(
        &mut store,
        session_data::importers::NativeProvider::Claude,
        Cursor::new(source),
    )
    .unwrap();
    assert_eq!(receipt.append.inserted, 1);
    let events = store.events(&Query::default()).unwrap();
    let requests = metrics::requests(&events);
    assert_eq!(requests[0].started_at_ms, None);
    assert_eq!(requests[0].duration_ms, None);
    assert_eq!(
        requests[0]
            .usage
            .as_ref()
            .unwrap()
            .input_tokens
            .unwrap()
            .value,
        30
    );
    assert!(!serde_json::to_string(&events).unwrap().contains("PRIVATE"));
}

#[test]
fn partial_native_source_and_corrupt_event_leave_previous_state_intact() {
    let temp = Temp::new();
    let mut store = temp.store();
    store.append_batch(&[start()]).unwrap();
    assert!(session_data::importers::import_native(
        &mut store,
        session_data::importers::NativeProvider::Codex,
        Cursor::new("{\"type\":")
    )
    .is_err());
    assert_eq!(store.status().unwrap().events, 1);
    let conn = rusqlite::Connection::open(temp.database()).unwrap();
    conn.execute("UPDATE events SET envelope='{}'", []).unwrap();
    drop(conn);
    assert!(store
        .check()
        .unwrap_err()
        .to_string()
        .contains("integrity_failed"));
    assert!(store.events(&Query::default()).is_err());
}

#[cfg(unix)]
#[test]
fn permissive_directory_and_symlink_database_are_refused() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = Temp::new();
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(&temp.database()).is_err());
    std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = Temp::new();
    let store = target.store();
    drop(store);
    symlink(target.database(), temp.database()).unwrap();
    assert!(Store::open(&temp.database()).is_err());
}

#[test]
fn crash_child() {
    let Some(path) = std::env::var_os("GOBSTOPPER_DATA_CRASH_TEST") else {
        return;
    };
    let path = PathBuf::from(path);
    let mut store = Store::open(&path).unwrap();
    store.append_batch(&[start()]).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE; DELETE FROM events; UPDATE metadata SET revision=999;")
        .unwrap();
    std::process::exit(42);
}

#[test]
fn crashed_transaction_rolls_back_but_committed_attempt_stays_incomplete() {
    let temp = Temp::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("crash_child")
        .arg("--nocapture")
        .env("GOBSTOPPER_DATA_CRASH_TEST", temp.database())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(42));
    let store = temp.store();
    let state = store.status().unwrap();
    assert_eq!(state.events, 1);
    assert_eq!(state.revision, 1);
    assert_eq!(state.incomplete_attempts, 1);
    assert!(store.check().unwrap().ok);
}

#[test]
fn concurrent_initialization_preserves_namespace_and_serializes_writers() {
    let temp = Temp::new();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let path = temp.database();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let mut store = Store::open(&path).unwrap();
                let identity = store.opaque("session", "same-native-id");
                for _ in 0..5 {
                    store.append_batch(&[start()]).unwrap();
                }
                identity
            })
        })
        .collect();
    let identities: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert!(identities.iter().all(|id| id == &identities[0]));
    let store = temp.store();
    assert_eq!(store.status().unwrap().events, 15);
    assert_eq!(store.check().unwrap().revision, 15);
}

#[test]
fn mismatched_known_attempt_identity_is_rejected_atomically() {
    let temp = Temp::new();
    let mut store = temp.store();
    let begun = start();
    store.append_batch(std::slice::from_ref(&begun)).unwrap();
    let mut end = finish(&begun);
    end.identity.request_id = Some(OpaqueId::random().unwrap());
    assert!(store
        .append_batch(&[end])
        .unwrap_err()
        .to_string()
        .contains("attempt_identity_conflict"));
    assert_eq!(store.status().unwrap().events, 1);
}

#[test]
fn metric_profiles_remain_separate_within_one_source_kind() {
    let first = start();
    let mut second = start();
    second.source.profile = "gobstopper-proxy-future".into();
    let report = metrics::metrics(&[first, second]);
    assert_eq!(report.cohorts.len(), 2);
    assert!(report.cohorts.iter().all(|cohort| cohort.attempts == 1));
    assert_ne!(
        report.cohorts[0].source_profile,
        report.cohorts[1].source_profile
    );
}

#[test]
fn cli_exports_imports_and_checks_a_private_store() {
    fn run(temp: &Temp, args: &[&str]) -> serde_json::Value {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .arg("data")
            .arg("--state-dir")
            .arg(&temp.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    let source = Temp::new();
    let destination = Temp::new();
    source.store().append_batch(&[start()]).unwrap();
    let snapshot = source.0.join("observations.jsonl");
    assert_eq!(
        run(&source, &["export", "--output", snapshot.to_str().unwrap()])["exported_events"],
        1
    );
    assert_eq!(
        run(&destination, &["import", snapshot.to_str().unwrap()])["inserted"],
        1
    );
    assert_eq!(
        run(&destination, &["import", snapshot.to_str().unwrap()])["duplicates"],
        1
    );
    assert_eq!(run(&destination, &["status"])["incomplete_attempts"], 1);
    assert_eq!(run(&destination, &["metrics"])["cohorts"][0]["attempts"], 1);
    assert_eq!(run(&destination, &["check"])["ok"], true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

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
use std::io::{BufReader, Cursor};
use std::path::PathBuf;
use store::{check_archive, PageOptions, DEFAULT_READ_TIMEOUT_MS};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
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
            reason: None,
            timings: None,
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
fn read_commands_refuse_absent_state_without_initializing_a_database() {
    let temp = Temp::new();
    assert!(Store::open_for_read(&temp.database()).is_err());
    assert!(!temp.database().exists());
    for command in ["status", "metrics", "events", "check"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_gobstopper"))
            .args([
                "--no-update",
                "data",
                "--state-dir",
                temp.0.to_str().unwrap(),
                command,
            ])
            .env("XDG_CONFIG_HOME", &temp.0)
            .env("HRANESS_NO_UPDATE", "1")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{command}");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined
                .to_ascii_lowercase()
                .contains("data_state_unavailable"),
            "{command}: {combined}"
        );
        assert!(!temp.database().exists());
    }
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
fn current_schema_open_and_queries_do_not_take_the_writer_lock() {
    let temp = Temp::new();
    let mut store = temp.store();
    let event = start();
    store.append_batch(std::slice::from_ref(&event)).unwrap();
    let identity = store.opaque("session", "same-native-id");
    let writer = rusqlite::Connection::open(temp.database()).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE metadata SET revision=revision+100;")
        .unwrap();
    // A zero busy timeout makes any unnecessary writer lock fail immediately.
    // The query must see only committed data while the other writer is active.
    let reader =
        Store::open_with_busy_timeout(&temp.database(), std::time::Duration::ZERO).unwrap();
    assert_eq!(reader.status().unwrap().revision, 1);
    assert_eq!(reader.events(&Query::default()).unwrap(), vec![event]);
    assert_eq!(reader.opaque("session", "same-native-id"), identity);
    assert!(reader.check().unwrap().ok);
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn current_schema_fast_path_rejects_wrong_application_and_negative_versions() {
    for (version, application) in [(2, 123), (-1, 0x47534442)] {
        let temp = Temp::new();
        let mut store = temp.store();
        store.append_batch(&[start()]).unwrap();
        drop(store);
        let connection = rusqlite::Connection::open(temp.database()).unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        connection
            .pragma_update(None, "application_id", application)
            .unwrap();
        assert!(Store::open(&temp.database()).is_err());
        assert_eq!(
            connection
                .pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
                .unwrap(),
            version
        );
        assert_eq!(
            connection
                .pragma_query_value::<i32, _>(None, "application_id", |r| r.get(0))
                .unwrap(),
            application
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn concurrent_migration_rechecks_schema_and_preserves_observations_and_namespace() {
    let temp = Temp::new();
    let mut store = temp.store();
    let event = start();
    store.append_batch(std::slice::from_ref(&event)).unwrap();
    let identity = store.opaque("session", "same-native-id");
    drop(store);
    let connection = rusqlite::Connection::open(temp.database()).unwrap();
    connection.execute_batch("DROP INDEX events_time; DROP INDEX events_session; DROP INDEX events_attempt; PRAGMA user_version=1;").unwrap();
    drop(connection);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = (0..3)
        .map(|_| {
            let path = temp.database();
            let barrier = barrier.clone();
            let expected = event.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let store = Store::open(&path).unwrap();
                assert_eq!(store.status().unwrap().schema_version, 2);
                assert_eq!(store.status().unwrap().revision, 1);
                assert_eq!(store.events(&Query::default()).unwrap(), vec![expected]);
                assert!(store.check().unwrap().ok);
                store.opaque("session", "same-native-id")
            })
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().unwrap(), identity);
    }
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
fn cli_data_directory_explicit_overrides_keep_precedence() {
    assert_cli_data_directory(
        &[
            ("GOBSTOPPER_DATA_DIR", "explicit"),
            ("XDG_DATA_HOME", "xdg"),
            ("LOCALAPPDATA", "local"),
            ("USERPROFILE", "profile"),
            ("HOME", "home"),
        ],
        "explicit",
    );
    assert_cli_data_directory(
        &[
            ("XDG_DATA_HOME", "xdg"),
            ("LOCALAPPDATA", "local"),
            ("USERPROFILE", "profile"),
            ("HOME", "home"),
        ],
        "xdg/gobstopper/private",
    );
}

#[test]
#[cfg(not(windows))]
fn cli_data_directory_keeps_unix_home_default() {
    assert_cli_data_directory(
        &[
            ("LOCALAPPDATA", "local"),
            ("USERPROFILE", "profile"),
            ("HOME", "home"),
        ],
        "home/.local/share/gobstopper/private",
    );
}

#[test]
#[cfg(windows)]
fn cli_data_directory_uses_windows_local_app_data_without_home() {
    assert_cli_data_directory(
        &[("LOCALAPPDATA", "local"), ("USERPROFILE", "profile")],
        "local/gobstopper/private",
    );
    assert_cli_data_directory(&[("LOCALAPPDATA", "local")], "local/gobstopper/private");
}

#[test]
#[cfg(windows)]
fn cli_data_directory_falls_back_to_windows_user_profile_without_home() {
    assert_cli_data_directory(
        &[("USERPROFILE", "profile")],
        "profile/AppData/Local/gobstopper/private",
    );
}

fn assert_cli_data_directory(variables: &[(&str, &str)], expected: &str) {
    let temp = Temp::new();
    let expected_path = temp.0.join(expected).join("sessions.sqlite3");
    drop(Store::open(&expected_path).unwrap());
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_gobstopper"));
    command.args(["data", "status"]);
    for variable in [
        "GOBSTOPPER_DATA_DIR",
        "XDG_DATA_HOME",
        "LOCALAPPDATA",
        "USERPROFILE",
        "HOME",
    ] {
        command.env_remove(variable);
    }
    for (variable, relative) in variables {
        command.env(variable, temp.0.join(relative));
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["events"], 0);
    assert!(temp.0.join(expected).join("sessions.sqlite3").is_file());
}

fn measure<'a>(report: &'a metrics::Metrics, id: &str) -> &'a metrics::Measure {
    report.cohorts[0]
        .measures
        .iter()
        .find(|m| m.id == id)
        .unwrap()
}

fn expected_measure(
    id: &str,
    unit: &str,
    value: Option<f64>,
    counts: (&str, &str, u64, u64),
    reason: Option<&str>,
) -> serde_json::Value {
    let (numerator, denominator, measured, missing) = counts;
    serde_json::json!({
        "id": id, "unit": unit, "value": value,
        "numerator": numerator, "denominator": denominator,
        "measured": measured, "missing": missing,
        "status": if measured == 0 { "unavailable" } else if missing > 0 { "partial" } else { "available" },
        "unavailable_reason": reason,
    })
}

#[test]
fn streaming_metrics_match_old_measures_and_slice_lenses() {
    let temp = Temp::new();
    let mut store = temp.store();
    let begun = start();
    let mut terminal = finish(&begun);
    if let Event::RequestFinished { generation, .. } = &mut terminal.event {
        *generation = Some(Generation {
            output_tokens: 40,
            duration_ms: 2000,
        });
    }
    let pending = start();
    let mut zero = finish(&start());
    if let Event::RequestFinished {
        duration_ms,
        first_output_ms,
        usage,
        ..
    } = &mut zero.event
    {
        *duration_ms = Some(0);
        *first_output_ms = Some(0);
        *usage = Some(Usage {
            input_tokens: Some(Quantity::reported(0)),
            output_tokens: Some(Quantity::reported(0)),
            ..Usage::default()
        });
    }
    let mut estimated = finish(&start());
    if let Event::RequestFinished {
        duration_ms,
        first_output_ms,
        usage,
        ..
    } = &mut estimated.event
    {
        *duration_ms = Some(5000);
        *first_output_ms = None;
        *usage = Some(Usage {
            input_tokens: Some(Quantity {
                value: 150,
                basis: Basis::Estimated,
            }),
            output_tokens: Some(Quantity {
                value: 70,
                basis: Basis::Estimated,
            }),
            ..Usage::default()
        });
    }
    let mut unknown = finish(&start());
    if let Event::RequestFinished {
        outcome,
        duration_ms,
        first_output_ms,
        usage,
        ..
    } = &mut unknown.event
    {
        *outcome = Outcome::Unknown;
        *duration_ms = None;
        *first_output_ms = None;
        *usage = None;
    }
    let mut events = vec![begun.clone(), terminal, pending, zero, estimated, unknown];
    for (before, after, shadow) in [(100, 60, false), (0, 10, false), (90, 30, true)] {
        events.push(
            Envelope::new(
                begun.source.clone(),
                begun.identity.clone(),
                Event::ContextDecision {
                    estimated_before_tokens: before,
                    estimated_after_tokens: after,
                    threshold_tokens: 100,
                    compacted: true,
                    shadow,
                    policy: None,
                },
            )
            .unwrap(),
        );
    }
    events.push(
        Envelope::new(
            Source {
                kind: SourceKind::LegacyStats,
                id: OpaqueId::random().unwrap(),
                profile: "gobstopper-stats-v0".into(),
            },
            Identity::default(),
            Event::LegacyContext {
                estimated_before_tokens: 100,
                estimated_after_tokens: 50,
                compacted: true,
            },
        )
        .unwrap(),
    );
    for (index, event) in events.iter_mut().enumerate() {
        event.observed_at_ms = 100 + index as u64 * 10;
    }
    store.append_batch(&events).unwrap();
    for query in [
        Query::default(),
        Query {
            since_ms: Some(110),
            until_ms: Some(150),
            session_id: None,
        },
        Query {
            session_id: begun.identity.session_id.clone(),
            ..Query::default()
        },
    ] {
        let selected = store.events(&query).unwrap();
        assert_eq!(
            serde_json::to_value(store.metrics(&query).unwrap()).unwrap(),
            serde_json::to_value(metrics::metrics(&selected)).unwrap()
        );
    }
    let mut old = serde_json::to_value(store.metrics(&Query::default()).unwrap()).unwrap();
    old["cohorts"][0]["measures"]
        .as_array_mut()
        .unwrap()
        .truncate(6);
    assert_eq!(
        old["cohorts"][0],
        serde_json::json!({
            "source_kind": "live_proxy", "source_profile": "gobstopper-proxy-v1",
            "attempts": 5, "incomplete_attempts": 1, "unknown_outcomes": 2,
            "reported_input_tokens": "100", "reported_output_tokens": "50",
            "estimated_input_tokens": "150", "estimated_output_tokens": "70",
            "requests_with_input": 3, "requests_with_output": 3,
            "applied_compactions": 2, "shadow_compactions": 1, "legacy_observations": 0,
            "context_tokens_removed_estimate": "40",
            "measures": [
                expected_measure("request_duration_mean", "ms", Some(5000.0), ("15000", "3", 3, 2), None),
                expected_measure("first_output_latency_mean", "ms", Some(500.0), ("1000", "2", 2, 3), None),
                expected_measure("reported_input_tokens_mean", "tokens", Some(50.0), ("100", "2", 2, 3), None),
                expected_measure("reported_output_tokens_mean", "tokens", Some(25.0), ("50", "2", 2, 3), None),
                expected_measure("generation_output_tokens_per_second", "tokens/s", Some(20.0), ("40", "2000", 1, 4), None),
                expected_measure("request_output_tokens_per_second", "tokens/s", Some(5.0), ("50", "10000", 1, 4), None),
            ],
        })
    );
    let legacy = &old["cohorts"][1];
    assert_eq!(legacy["attempts"], 0);
    assert_eq!(legacy["legacy_observations"], 1);
    for item in legacy["measures"].as_array().unwrap() {
        assert_eq!(item["value"], serde_json::Value::Null);
        assert_eq!(item["numerator"], "0");
        assert_eq!(item["denominator"], "0");
        assert_eq!(item["missing"], 0);
        assert_eq!(item["status"], "unavailable");
    }
}

#[test]
fn materialized_queries_refuse_over_64_mib_without_limiting_streaming_metrics() {
    use sha2::Digest;
    let temp = Temp::new();
    let mut store = temp.store();
    let events = (0..4097).map(|_| start()).collect::<Vec<_>>();
    store.append_batch(&events).unwrap();
    let mut connection = rusqlite::Connection::open(temp.database()).unwrap();
    let transaction = connection.transaction().unwrap();
    {
        let mut update = transaction
            .prepare("UPDATE events SET envelope=?1,digest=?2 WHERE event_id=?3")
            .unwrap();
        for event in &events {
            let mut body = serde_json::to_string(event).unwrap();
            body.extend(std::iter::repeat_n(
                ' ',
                schema::MAX_EVENT_BYTES - body.len(),
            ));
            let digest = sha2::Sha256::digest(body.as_bytes());
            update
                .execute(rusqlite::params![body, digest.as_slice(), event.event_id.0])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
    let error = match store.events(&Query::default()) {
        Ok(rows) => panic!("expected query byte limit, returned {} rows", rows.len()),
        Err(error) => error,
    };
    assert!(error.to_string().contains("data_query_limit_narrow_window"));
    let report = store.metrics(&Query::default()).unwrap();
    assert_eq!(report.cohorts.len(), 1);
    assert_eq!(report.cohorts[0].attempts, 4097);
    assert_eq!(store.check().unwrap().checked_events, 4097);
}

#[test]
fn streaming_metrics_cover_more_than_100000_events_with_exact_counters() {
    let temp = Temp::new();
    let mut store = temp.store();
    let template = start();
    let mut terminal = finish(&template);
    if let Event::RequestFinished { usage, .. } = &mut terminal.event {
        *usage = Some(Usage {
            input_tokens: Some(Quantity::reported(schema::MAX_COUNTER)),
            output_tokens: Some(Quantity::reported(schema::MAX_COUNTER)),
            ..Usage::default()
        });
    }
    let attempts = 50_001u64;
    let mut batch = Vec::with_capacity(store::MAX_BATCH_EVENTS);
    for index in 1..=attempts {
        let mut begun = template.clone();
        begun.event_id = OpaqueId(format!("{:064x}", index * 2));
        begun.identity.request_id = Some(OpaqueId(format!("{index:064x}")));
        begun.identity.attempt_id = Some(OpaqueId(format!("{index:064x}")));
        begun.observed_at_ms = index * 2;
        let mut finished = terminal.clone();
        finished.event_id = OpaqueId(format!("{:064x}", index * 2 + 1));
        finished.identity = begun.identity.clone();
        finished.observed_at_ms = index * 2 + 1;
        batch.extend([begun, finished]);
        if batch.len() == store::MAX_BATCH_EVENTS {
            store.append_batch(&batch).unwrap();
            batch.clear();
        }
    }
    store.append_batch(&batch).unwrap();
    let report = store.metrics(&Query::default()).unwrap();
    assert_eq!(store::MAX_QUERY_EVENTS, 100_000);
    assert_eq!(store.status().unwrap().events, attempts * 2);
    assert_eq!(report.cohorts.len(), 1);
    let cohort = &report.cohorts[0];
    let total = (attempts as u128 * schema::MAX_COUNTER as u128).to_string();
    assert_eq!(cohort.attempts, attempts);
    assert_eq!(cohort.incomplete_attempts, 0);
    assert_eq!(cohort.reported_input_tokens, total);
    assert_eq!(cohort.reported_output_tokens, total);
    let input = measure(&report, "reported_input_tokens_mean");
    assert_eq!(input.numerator, total);
    assert_eq!(input.denominator, attempts.to_string());
    assert_eq!(input.missing, 0);
    let request = measure(&report, "request_output_tokens_per_second");
    assert_eq!(request.numerator, total);
    assert_eq!(request.denominator, (attempts as u128 * 10_000).to_string());
    let generation = measure(&report, "generation_output_tokens_per_second");
    assert_eq!(generation.measured, 0);
    assert_eq!(generation.missing, attempts);
    assert_eq!(
        generation.unavailable_reason,
        Some("no_matching_generation_spans")
    );
    assert!(store
        .export(&Query::default(), Vec::new())
        .unwrap_err()
        .to_string()
        .contains("export_limit"));
    let page = store
        .event_page(
            &Query::default(),
            &PageOptions {
                after_sequence: 100_000,
                limit: 3,
                ..PageOptions::default()
            },
        )
        .unwrap();
    assert_eq!(page.events.len(), 2);
    assert!(page.complete);
}

#[test]
fn timing_profiles_remain_distinct_and_streaming_matches_the_slice_lens() {
    let temp = Temp::new();
    let mut store = temp.store();
    let legacy = start();
    let legacy_finish = finish(&legacy);
    let mut current = start();
    current.source.profile = "gobstopper-proxy-v2".into();
    let mut current_finish = finish(&current);
    if let Event::RequestFinished {
        reason, timings, ..
    } = &mut current_finish.event
    {
        *reason = Some(schema::FinishReason::Completed);
        *timings = Some(schema::RequestTimings {
            preparation_ms: 10,
            upstream_headers_ms: 20,
            transform_ms: None,
        });
    }
    store
        .append_batch(&[legacy, legacy_finish, current, current_finish])
        .unwrap();
    let selected = store.events(&Query::default()).unwrap();
    let streamed = store.metrics(&Query::default()).unwrap();
    assert_eq!(
        serde_json::to_value(&streamed).unwrap(),
        serde_json::to_value(metrics::metrics(&selected)).unwrap()
    );
    assert_eq!(streamed.cohorts.len(), 2);
    assert_eq!(streamed.cohorts[0].source_profile, "gobstopper-proxy-v1");
    assert_eq!(streamed.cohorts[1].source_profile, "gobstopper-proxy-v2");
    assert_eq!(streamed.cohorts[0].attempts, 1);
    assert_eq!(streamed.cohorts[1].attempts, 1);
}

#[test]
fn finish_reasons_and_stage_timings_are_additive_and_missing_stays_missing() {
    use schema::{FinishReason, RequestTimings};
    let temp = Temp::new();
    let mut store = temp.store();
    let begun = start();
    let mut finished = finish(&begun);
    if let Event::RequestFinished {
        reason, timings, ..
    } = &mut finished.event
    {
        *reason = Some(FinishReason::Completed);
        *timings = Some(RequestTimings {
            preparation_ms: 100,
            upstream_headers_ms: 200,
            transform_ms: Some(50),
        });
    }
    let interrupted = start();
    let mut interrupted_finish = finish(&interrupted);
    if let Event::RequestFinished {
        outcome,
        reason,
        timings,
        ..
    } = &mut interrupted_finish.event
    {
        *outcome = Outcome::Interrupted;
        *reason = Some(FinishReason::UpstreamReadFailed);
        *timings = Some(RequestTimings {
            preparation_ms: 0,
            upstream_headers_ms: 0,
            transform_ms: None,
        });
    }
    let legacy = finish(&start());
    let old_json = serde_json::to_vec(&legacy).unwrap();
    let old_fields: serde_json::Value = serde_json::from_slice(&old_json).unwrap();
    assert!(old_fields["event"].get("reason").is_none());
    assert!(old_fields["event"].get("timings").is_none());
    let legacy: Envelope = serde_json::from_slice(&old_json).unwrap();
    let pending = start();
    store
        .append_batch(&[
            begun,
            finished,
            interrupted,
            interrupted_finish,
            legacy,
            pending,
        ])
        .unwrap();
    let report = store.metrics(&Query::default()).unwrap();
    let success = measure(&report, "request_outcome_success_fraction");
    assert_eq!(
        (
            success.numerator.as_str(),
            success.denominator.as_str(),
            success.measured,
            success.missing
        ),
        ("2", "3", 3, 1)
    );
    let completed = measure(&report, "request_finish_completed_fraction");
    assert_eq!(
        (
            completed.numerator.as_str(),
            completed.denominator.as_str(),
            completed.measured,
            completed.missing
        ),
        ("1", "2", 2, 2)
    );
    let preparation = measure(&report, "request_preparation_mean");
    assert_eq!(preparation.value, Some(50.0));
    assert_eq!((preparation.measured, preparation.missing), (2, 2));
    assert_eq!(
        measure(&report, "upstream_headers_latency_mean").value,
        Some(100.0)
    );
    let transform = measure(&report, "request_transform_mean");
    assert_eq!(
        (transform.value, transform.measured, transform.missing),
        (Some(50.0), 1, 3)
    );
}

#[test]
fn raw_event_pagination_is_bounded_half_open_and_stable_across_appends() {
    let temp = Temp::new();
    let mut store = temp.store();
    let session = OpaqueId::random().unwrap();
    let events: Vec<_> = (1..=5)
        .map(|time| {
            let mut event = start();
            event.observed_at_ms = time;
            event.identity.session_id = Some(session.clone());
            event
        })
        .collect();
    store.append_batch(&events).unwrap();
    let query = Query {
        since_ms: Some(2),
        until_ms: Some(5),
        session_id: Some(session.clone()),
    };
    let first = store
        .event_page(
            &query,
            &PageOptions {
                limit: 2,
                ..PageOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        first.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(first.next_after_sequence, Some(3));
    assert_eq!(first.snapshot_sequence, 5);
    assert!(!first.complete);
    let mut appended = start();
    appended.observed_at_ms = 4;
    appended.identity.session_id = Some(session);
    store.append_batch(&[appended]).unwrap();
    let next = store
        .event_page(
            &query,
            &PageOptions {
                after_sequence: first.next_after_sequence.unwrap(),
                through_sequence: Some(first.snapshot_sequence),
                limit: 2,
                ..PageOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        next.events.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        [4]
    );
    assert!(next.complete);
    assert_eq!(next.next_after_sequence, None);
    assert_eq!(next.revision, 2);
    assert_eq!(store.events(&query).unwrap().len(), 4);
    for page in [
        PageOptions {
            limit: 0,
            ..PageOptions::default()
        },
        PageOptions {
            limit: 10_001,
            ..PageOptions::default()
        },
        PageOptions {
            after_sequence: 7,
            ..PageOptions::default()
        },
        PageOptions {
            timeout_ms: 0,
            ..PageOptions::default()
        },
        PageOptions {
            timeout_ms: 600_001,
            ..PageOptions::default()
        },
    ] {
        assert!(store.event_page(&query, &page).is_err());
    }
    assert!(store.metrics_with_timeout(&query, 0).is_err());
    assert!(store.metrics_with_timeout(&query, 600_001).is_err());
}

#[test]
fn segmented_archive_reimports_beyond_one_batch_and_never_changes_source() {
    let source = Temp::new();
    let restored = Temp::new();
    let mut writer = source.store();
    let template = start();
    let total = 11_001u64;
    for offset in (0..total).step_by(store::MAX_BATCH_EVENTS) {
        let events: Vec<_> = (offset..(offset + store::MAX_BATCH_EVENTS as u64).min(total))
            .map(|index| {
                let mut event = template.clone();
                event.event_id = OpaqueId(format!("{:064x}", index + 1));
                event.observed_at_ms = index;
                event.event = Event::ContextDecision {
                    estimated_before_tokens: 100,
                    estimated_after_tokens: 50,
                    threshold_tokens: 100,
                    compacted: true,
                    shadow: false,
                    policy: None,
                };
                event
            })
            .collect();
        writer.append_batch(&events).unwrap();
    }
    let original = std::fs::read(source.database()).unwrap();
    let wal = source.0.join("sessions.sqlite3-wal");
    let original_wal = std::fs::read(&wal).unwrap();
    let reader = Store::open_readonly(&source.database()).unwrap();
    let archive = source.0.join("archive");
    let receipt = reader
        .archive(
            &Query::default(),
            &archive,
            store::MAX_BATCH_EVENTS,
            DEFAULT_READ_TIMEOUT_MS,
        )
        .unwrap();
    assert!(receipt.complete);
    assert_eq!((receipt.event_count, receipt.segments), (total, 2));
    assert_eq!(
        check_archive(&archive, DEFAULT_READ_TIMEOUT_MS)
            .unwrap()
            .manifest_sha256,
        receipt.manifest_sha256
    );
    let mut destination = restored.store();
    for index in 1..=receipt.segments {
        let file = std::fs::File::open(archive.join(format!("segment-{index:06}.jsonl"))).unwrap();
        destination.import(BufReader::new(file)).unwrap();
    }
    assert_eq!(destination.status().unwrap().events, total);
    assert_eq!(
        serde_json::to_value(reader.metrics(&Query::default()).unwrap()).unwrap(),
        serde_json::to_value(destination.metrics(&Query::default()).unwrap()).unwrap()
    );
    assert_eq!(std::fs::read(source.database()).unwrap(), original);
    assert_eq!(std::fs::read(wal).unwrap(), original_wal);
    assert_eq!(writer.status().unwrap().revision, 2);
    assert!(reader
        .archive(&Query::default(), &archive, 2, DEFAULT_READ_TIMEOUT_MS)
        .unwrap_err()
        .to_string()
        .contains("must_be_new"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&archive).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for entry in std::fs::read_dir(&archive).unwrap() {
            assert_eq!(
                entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    let segment = archive.join("segment-000002.jsonl");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(segment)
        .unwrap();
    file.set_len(20).unwrap();
    assert!(check_archive(&archive, DEFAULT_READ_TIMEOUT_MS).is_err());
}

#[test]
fn corrupt_rows_fail_metrics_pagination_and_leave_archives_incomplete() {
    let temp = Temp::new();
    let mut writer = temp.store();
    writer
        .append_batch(&(0..4).map(|_| start()).collect::<Vec<_>>())
        .unwrap();
    let connection = rusqlite::Connection::open(temp.database()).unwrap();
    connection
        .execute("UPDATE events SET digest=zeroblob(32) WHERE sequence=4", [])
        .unwrap();
    drop(connection);
    let reader = Store::open_readonly(&temp.database()).unwrap();
    assert!(reader
        .metrics(&Query::default())
        .unwrap_err()
        .to_string()
        .contains("integrity_failed"));
    assert!(reader
        .event_page(&Query::default(), &PageOptions::default())
        .is_err());
    let archive = temp.0.join("incomplete");
    assert!(reader
        .archive(&Query::default(), &archive, 2, DEFAULT_READ_TIMEOUT_MS)
        .is_err());
    assert!(!archive.join("complete.json").exists());
    assert!(archive.join("segment-000001.jsonl").is_file());
    assert!(check_archive(&archive, DEFAULT_READ_TIMEOUT_MS)
        .unwrap_err()
        .to_string()
        .contains("incomplete"));
    let restored = Temp::new();
    assert_eq!(
        restored
            .store()
            .import(BufReader::new(
                std::fs::File::open(archive.join("segment-000001.jsonl")).unwrap()
            ))
            .unwrap()
            .inserted,
        2
    );
    let other = Temp::new();
    let mut writer = other.store();
    writer.append_batch(&[start()]).unwrap();
    let connection = rusqlite::Connection::open(other.database()).unwrap();
    connection
        .execute("UPDATE events SET kind='legacy_context'", [])
        .unwrap();
    assert!(writer
        .metrics(&Query::default())
        .unwrap_err()
        .to_string()
        .contains("projection_inconsistent"));
}

#[test]
fn invalid_event_errors_never_echo_untrusted_field_values() {
    use sha2::Digest;
    let temp = Temp::new();
    let mut store = temp.store();
    let event = start();
    store.append_batch(std::slice::from_ref(&event)).unwrap();
    let mut body = serde_json::to_value(&event).unwrap();
    body["source"]["kind"] = serde_json::json!("PRIVATE_PROMPT_DO_NOT_LOG");
    let body = serde_json::to_string(&body).unwrap();
    let digest = sha2::Sha256::digest(body.as_bytes());
    let connection = rusqlite::Connection::open(temp.database()).unwrap();
    connection
        .execute(
            "UPDATE events SET envelope=?1,digest=?2",
            rusqlite::params![body, digest.as_slice()],
        )
        .unwrap();
    let error = store.metrics(&Query::default()).unwrap_err();
    assert_eq!(error.to_string(), "data_invalid_event");
    assert!(!format!("{error:#}").contains("PRIVATE_PROMPT"));
}

#[test]
fn oversized_private_database_remains_readable_and_exportable_but_not_writable() {
    let temp = Temp::new();
    let mut writer = temp.store();
    let begun = start();
    writer
        .append_batch(&[begun.clone(), finish(&begun)])
        .unwrap();
    let namespace = writer.opaque("session", "unchanged");
    drop(writer);
    let bytes = 1024 * 1024 * 1024 + 4096u64;
    std::fs::OpenOptions::new()
        .write(true)
        .open(temp.database())
        .unwrap()
        .set_len(bytes)
        .unwrap();
    assert!(Store::open(&temp.database())
        .err()
        .unwrap()
        .to_string()
        .contains("database_limit"));
    let mut reader = Store::open_readonly(&temp.database()).unwrap();
    let status = reader.status().unwrap();
    assert_eq!(status.capacity_warning, Some("write_limit_exceeded"));
    assert_eq!(status.remaining_capacity_bytes, 0);
    assert!(status.at_write_capacity && status.read_only);
    assert_eq!(status.write_limit_bytes, 1024 * 1024 * 1024);
    assert_eq!(reader.check().unwrap().checked_events, 2);
    assert_eq!(
        reader.metrics(&Query::default()).unwrap().cohorts[0].attempts,
        1
    );
    assert_eq!(
        reader
            .event_page(&Query::default(), &PageOptions::default())
            .unwrap()
            .events
            .len(),
        2
    );
    let mut exported = Vec::new();
    assert_eq!(reader.export(&Query::default(), &mut exported).unwrap(), 2);
    let recovered = Temp::new();
    assert_eq!(
        recovered
            .store()
            .import(Cursor::new(exported))
            .unwrap()
            .inserted,
        2
    );
    let archive = temp.0.join("copy-out");
    assert!(
        reader
            .archive(&Query::default(), &archive, 1, DEFAULT_READ_TIMEOUT_MS)
            .unwrap()
            .complete
    );
    let target = Temp::new();
    assert_eq!(reader.backup(&target.database()).unwrap().checked_events, 2);
    assert_eq!(target.store().opaque("session", "unchanged"), namespace);
    assert!(reader
        .append_batch(&[start()])
        .unwrap_err()
        .to_string()
        .contains("read_only"));
    assert_eq!(std::fs::metadata(temp.database()).unwrap().len(), bytes);
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
    assert!(run(&destination, &["requests"]).is_array());
    assert!(run(&destination, &["tools"]).is_array());
    let page = run(&source, &["events", "--limit", "1"]);
    assert_eq!(page["complete"], true);
    assert_eq!(page["snapshot_sequence"], 1);
    assert_eq!(page["events"][0]["sequence"], 1);
    let archive = source.0.join("archive-cli");
    assert_eq!(
        run(&source, &["archive", "--output", archive.to_str().unwrap()])["event_count"],
        1
    );
    let untouched = Temp::new();
    assert_eq!(
        run(&untouched, &["archive-check", archive.to_str().unwrap()])["complete"],
        true
    );
    assert!(!untouched.database().exists());
    let segment = archive.join("segment-000001.jsonl");
    assert_eq!(
        run(&destination, &["import", segment.to_str().unwrap()])["duplicates"],
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

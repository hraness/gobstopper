//! Private, transactional context reservations. Capabilities never go upstream.
//! This is operational state, independent of the portable observation journal.
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use gobstopper_adapters::request::EvidenceObservation;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SCHEMA: i64 = 1;
const APPLICATION_ID: i64 = 0x47424354;
const MAX_SCOPES: i64 = 4096;
const MAX_TOKENS: u64 = 8_000_000;
const MAX_REQUESTS: u32 = 4096;
const MAX_TTL: u64 = 86_400;

#[derive(Args)]
pub struct ContextArgs {
    #[command(subcommand)]
    command: ContextCmd,
}
#[derive(Subcommand)]
enum ContextCmd {
    /// Create a private scope; use its base URL or X-Gobstopper-Scope header.
    Create {
        /// Operator-configured provider capacity; never inferred from a model name.
        #[arg(long)]
        context_window: Option<u64>,
        #[arg(long)]
        client_context_window: Option<u64>,
        #[arg(long, default_value_t = 32_000)]
        output_reserve: u64,
        /// Allow bounded rescue after repeated reads of unchanged evicted evidence.
        #[arg(long)]
        adaptive: bool,
        #[arg(long, default_value_t = crate::proxy::DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        json: bool,
    },
    /// Remove a scope when every client using it has stopped.
    Close {
        #[arg(long)]
        scope: Option<String>,
    },
    /// Reserve a temporary input budget for this scope, including its descendants.
    Reserve {
        /// Defaults to GOBSTOPPER_SCOPE set by proxy run.
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        tokens: u64,
        #[arg(long, default_value_t = 20)]
        requests: u32,
        #[arg(long, default_value_t = 1800)]
        ttl_seconds: u64,
        #[arg(long)]
        json: bool,
    },
    /// Inspect the reservation without consuming it.
    Status {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// End the reservation and return to the proxy's ordinary threshold.
    Release {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct Decision {
    pub scope_id: String,
    pub requested_input_tokens: Option<u64>,
    pub effective_input_tokens: u64,
    pub input_capacity_tokens: Option<u64>,
    pub limiting_reason: String,
    pub remaining_requests: u32,
    pub expires_at_ms: Option<u64>,
    pub policy_generation: u64,
    pub adaptive: bool,
    pub rescue_count: u32,
    pub configured_provider_window: Option<u64>,
    pub configured_client_window: Option<u64>,
    pub output_reserve_tokens: u64,
    pub base_threshold_tokens: u64,
}
impl Decision {
    pub fn policy_identity(&self) -> String {
        // Scope-independent policies may share deterministic summaries. A scope
        // generation invalidates earlier summaries when reservations change.
        format!(
            "budget-v1:{}:{}:{:?}:{}",
            self.scope_id,
            self.policy_generation,
            self.input_capacity_tokens,
            self.effective_input_tokens
        )
    }
}

#[derive(Debug)]
struct Scope {
    window: Option<u64>,
    client_window: Option<u64>,
    output: u64,
    requested: Option<u64>,
    remaining: u32,
    created: u64,
    expires: Option<u64>,
    generation: u64,
    adaptive: bool,
    rereads: u32,
    reread_since: u64,
    rescues: u32,
    cooldown: u64,
    base: u64,
}

pub struct Control {
    db: Mutex<Connection>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
pub fn random_id() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("OS randomness unavailable: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn scope_key(capability: &str) -> Result<String> {
    if capability.len() != 64 || !capability.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid context scope capability");
    }
    Ok(format!("{:x}", Sha256::digest(capability.as_bytes())))
}
fn checked_window(window: Option<u64>) -> Result<()> {
    if window.is_some_and(|n| !(1..=MAX_TOKENS).contains(&n)) {
        bail!("context window must be 1..={MAX_TOKENS} tokens");
    }
    Ok(())
}
pub fn default_path() -> Result<PathBuf> {
    Ok(crate::session_data::default_path()?.with_file_name("context.sqlite3"))
}

impl Control {
    pub fn open(path: &Path) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match Self::open_once(path) {
                Err(error)
                    if error
                        .downcast_ref::<rusqlite::Error>()
                        .is_some_and(|error| {
                            matches!(
                                error.sqlite_error_code(),
                                Some(
                                    rusqlite::ErrorCode::DatabaseBusy
                                        | rusqlite::ErrorCode::DatabaseLocked
                                )
                            )
                        })
                        && Instant::now() < deadline =>
                {
                    // Concurrent startup can briefly contend on schema creation
                    // or the WAL transition. Never retry identity/privacy errors.
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => return result,
            }
        }
    }

    fn open_once(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .context("context state needs a parent directory")?;
        crate::session_data::prepare_private_dir(parent)?;
        if path.is_symlink() || parent.is_symlink() {
            bail!("context state must not be a symbolic link");
        }
        if !path.exists() {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = std::fs::symlink_metadata(path)?;
            if metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
                bail!("context state file must be private (mode 0600)");
            }
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(250))?;
        db.pragma_update(None, "synchronous", "FULL")?;
        // Schema identity and creation share one write transaction. A second
        // first opener must inspect the first opener's committed schema, never
        // combine an old user_version with a newer sqlite_master snapshot.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA {
            bail!("context state uses a newer schema ({version})");
        }
        let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if (version > 0 && application != APPLICATION_ID) || (version == 0 && application != 0) {
            bail!("context state belongs to another application");
        }
        if version == 0 {
            let tables: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if tables != 0 {
                bail!("context state is not an empty database");
            }
        }
        if version == 0 {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS scopes (
                id TEXT PRIMARY KEY, provider_window INTEGER, client_window INTEGER,
                output_reserve INTEGER NOT NULL, requested INTEGER, remaining INTEGER NOT NULL DEFAULT 0,
                created INTEGER NOT NULL, expires INTEGER, generation INTEGER NOT NULL DEFAULT 0,
                adaptive INTEGER NOT NULL, rereads INTEGER NOT NULL DEFAULT 0, reread_since INTEGER NOT NULL DEFAULT 0,
                rescues INTEGER NOT NULL DEFAULT 0, cooldown INTEGER NOT NULL DEFAULT 0,
                base INTEGER NOT NULL DEFAULT 128000);
                CREATE TABLE IF NOT EXISTS evidence (
                scope TEXT NOT NULL, source TEXT NOT NULL, digest TEXT NOT NULL,
                evicted INTEGER NOT NULL, seen INTEGER NOT NULL,
                PRIMARY KEY(scope,source));
                CREATE INDEX IF NOT EXISTS evidence_digest ON evidence(scope,digest,evicted);
                PRAGMA user_version=1;")?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        }
        tx.commit()?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { db: Mutex::new(db) })
    }
    pub fn create(
        &self,
        window: Option<u64>,
        client_window: Option<u64>,
        output: u64,
        adaptive: bool,
    ) -> Result<String> {
        checked_window(window)?;
        checked_window(client_window)?;
        if output > MAX_TOKENS
            || window.is_some_and(|n| output >= n)
            || client_window.is_some_and(|n| output >= n)
        {
            bail!("output reserve must leave room for input within each configured window");
        }
        let capability = random_id()?;
        let key = scope_key(&capability)?;
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT count(*) FROM scopes", [], |r| r.get(0))?;
        if count >= MAX_SCOPES {
            bail!("context scope limit reached; reuse an existing scope");
        }
        tx.execute(
            "INSERT INTO scopes(id,provider_window,client_window,output_reserve,created,adaptive)
            VALUES(?,?,?,?,?,?)",
            params![key, window, client_window, output, now_ms(), adaptive],
        )?;
        tx.commit()?;
        Ok(capability)
    }
    pub fn close(&self, capability: &str) -> Result<()> {
        let key = scope_key(capability)?;
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM evidence WHERE scope=?", [&key])?;
        tx.execute("DELETE FROM scopes WHERE id=?", [&key])?;
        tx.commit()?;
        Ok(())
    }
    pub fn reserve(
        &self,
        capability: &str,
        tokens: u64,
        requests: u32,
        ttl: u64,
    ) -> Result<Decision> {
        if !(1..=MAX_TOKENS).contains(&tokens)
            || !(1..=MAX_REQUESTS).contains(&requests)
            || !(1..=MAX_TTL).contains(&ttl)
        {
            bail!(
                "reservation requires tokens 1..={MAX_TOKENS}, requests 1..={MAX_REQUESTS}, ttl-seconds 1..={MAX_TTL}"
            );
        }
        let key = scope_key(capability)?;
        let now = now_ms();
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE scopes SET requested=?,remaining=?,created=?,expires=?,generation=generation+1
            WHERE id=?",
            params![tokens, requests, now, now.saturating_add(ttl * 1000), key],
        )? != 1
        {
            bail!("unknown context scope; create one first");
        }
        let row = read_scope(&tx, &key)?;
        let decision = decide(&key, &row, row.base, None, 0, now);
        tx.commit()?;
        Ok(decision)
    }
    pub fn release(&self, capability: &str) -> Result<Decision> {
        let key = scope_key(capability)?;
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE scopes SET requested=NULL,remaining=0,expires=NULL,generation=generation+1,
            rereads=0,cooldown=? WHERE id=?",
            params![now_ms() + 600_000, key],
        )? != 1
        {
            bail!("unknown context scope");
        }
        let row = read_scope(&tx, &key)?;
        let result = decide(&key, &row, row.base, None, 0, now_ms());
        tx.commit()?;
        Ok(result)
    }
    pub fn status(&self, capability: &str) -> Result<Decision> {
        let key = scope_key(capability)?;
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let row = read_scope(&db, &key)?;
        Ok(decide(&key, &row, row.base, None, 0, now_ms()))
    }
    /// Called once per accepted logical inference request, never on retries.
    pub fn consume(
        &self,
        capability: &str,
        base: u64,
        window: Option<u64>,
        output: u64,
    ) -> Result<Decision> {
        let key = scope_key(capability)?;
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = read_scope(&tx, &key)?;
        let mut result = decide(&key, &row, base, window, output, now_ms());
        if result.requested_input_tokens.is_some() {
            tx.execute(
                "UPDATE scopes SET remaining=remaining-1,base=? WHERE id=? AND remaining>0",
                params![base, key],
            )?;
            result.remaining_requests = result.remaining_requests.saturating_sub(1);
        } else {
            tx.execute("UPDATE scopes SET base=? WHERE id=?", params![base, key])?;
        }
        tx.commit()?;
        Ok(result)
    }
    /// Exact rereads after observed eviction, within an explicit scope. Only the
    /// newest 128 observations are inspected; 2048 identities per scope bound
    /// storage. Historical messages cannot increment the counter twice.
    pub fn observe(
        &self,
        capability: &str,
        observations: &[EvidenceObservation],
        evicted: &[String],
        decision: &Decision,
    ) -> Result<bool> {
        if !decision.adaptive {
            return Ok(false);
        }
        let key = scope_key(capability)?;
        let now = now_ms();
        let mut db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = read_scope(&tx, &key)?;
        let mut hits = 0u32;
        for observation in observations.iter().rev().take(128).rev() {
            let source = format!("{:x}", Sha256::digest(observation.source_id.as_bytes()));
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM evidence WHERE scope=? AND source=?)",
                params![key, source],
                |r| r.get(0),
            )?;
            if exists {
                continue;
            }
            let repeated: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM evidence WHERE scope=? AND digest=? AND evicted=1 AND seen>=?)",
                params![key,observation.content_digest,now.saturating_sub(900_000)], |r| r.get(0))?;
            if repeated {
                hits = hits.saturating_add(1);
            }
            tx.execute(
                "INSERT INTO evidence(scope,source,digest,evicted,seen) VALUES(?,?,?,?,?)",
                params![key, source, observation.content_digest, false, now],
            )?;
        }
        for digest in evicted.iter().take(2048) {
            tx.execute(
                "UPDATE evidence SET evicted=1 WHERE scope=? AND digest=?",
                params![key, digest],
            )?;
        }
        tx.execute(
            "DELETE FROM evidence WHERE scope=? AND rowid NOT IN
            (SELECT rowid FROM evidence WHERE scope=? ORDER BY seen DESC,rowid DESC LIMIT 2048)",
            params![key, key],
        )?;
        tx.execute("DELETE FROM evidence WHERE rowid NOT IN (SELECT rowid FROM evidence ORDER BY seen DESC,rowid DESC LIMIT 65536)",[])?;
        let prior = if now >= row.reread_since && now.saturating_sub(row.reread_since) < 900_000 {
            row.rereads
        } else {
            0
        };
        let since = if prior == 0 && hits > 0 {
            now
        } else {
            row.reread_since
        };
        let rereads = if now >= row.cooldown {
            prior.saturating_add(hits)
        } else {
            0
        };
        let target = decision.effective_input_tokens.saturating_mul(2).min(
            decision
                .input_capacity_tokens
                .unwrap_or(decision.effective_input_tokens),
        );
        let rescue = row.generation == decision.policy_generation
            && !(row.remaining > 0 && row.expires.is_some_and(|until| now < until))
            && rereads >= 3
            && now >= row.cooldown
            && target > decision.effective_input_tokens
            && decision.requested_input_tokens.is_none();
        if rescue {
            tx.execute("UPDATE scopes SET requested=?,remaining=8,created=?,expires=?,generation=generation+1,
                rereads=0,rescues=rescues+1,cooldown=? WHERE id=?",
                params![target,now,now+600_000,now+900_000,key])?;
        } else {
            tx.execute(
                "UPDATE scopes SET rereads=?,reread_since=? WHERE id=?",
                params![rereads, since, key],
            )?;
        }
        tx.commit()?;
        Ok(rescue)
    }
}
fn read_scope(db: &Connection, key: &str) -> Result<Scope> {
    db.query_row("SELECT provider_window,client_window,output_reserve,requested,remaining,created,
        expires,generation,adaptive,rereads,rescues,cooldown,base,reread_since FROM scopes WHERE id=?", [key], |r| {
        Ok(Scope { window:r.get(0)?,client_window:r.get(1)?,output:r.get(2)?,requested:r.get(3)?,
            remaining:r.get(4)?,created:r.get(5)?,expires:r.get(6)?,generation:r.get(7)?,adaptive:r.get(8)?,
            rereads:r.get(9)?,rescues:r.get(10)?,cooldown:r.get(11)?,base:r.get(12)?,reread_since:r.get(13)? })
    }).optional()?.context("unknown context scope")
}
fn decide(
    key: &str,
    row: &Scope,
    base: u64,
    window: Option<u64>,
    output: u64,
    now: u64,
) -> Decision {
    let ceiling = [row.window, row.client_window, window]
        .into_iter()
        .flatten()
        .min();
    let output = output.max(row.output);
    let capacity = ceiling.map(|n| n.saturating_sub(output));
    let active =
        row.remaining > 0 && now >= row.created && row.expires.is_some_and(|until| now < until);
    let requested = if active { row.requested } else { None };
    // Unknown capacity permits the existing configured trigger, but cannot
    // grant a larger reservation without an explicit capacity declaration.
    let effective = requested.unwrap_or(base).min(capacity.unwrap_or(base));
    let reason = if requested.is_some_and(|n| n > effective) {
        if capacity.is_some() {
            "capacity_or_output_headroom"
        } else {
            "capacity_unknown"
        }
    } else if active {
        "reserved"
    } else if row.requested.is_some() {
        "reservation_expired_or_exhausted"
    } else {
        "configured_threshold"
    };
    Decision {
        scope_id: key.into(),
        requested_input_tokens: requested,
        effective_input_tokens: effective,
        input_capacity_tokens: capacity,
        limiting_reason: reason.into(),
        remaining_requests: if active { row.remaining } else { 0 },
        expires_at_ms: if active { row.expires } else { None },
        policy_generation: row.generation,
        adaptive: row.adaptive,
        rescue_count: row.rescues,
        configured_provider_window: row.window,
        configured_client_window: row.client_window,
        output_reserve_tokens: output,
        base_threshold_tokens: base,
    }
}
fn capability(scope: &Option<String>) -> Result<String> {
    scope
        .clone()
        .or_else(|| std::env::var("GOBSTOPPER_SCOPE").ok())
        .context("use --scope or run the client through gobstopper proxy run")
}
pub fn run(args: &ContextArgs) -> Result<()> {
    let control = Control::open(&default_path()?)?;
    let result = match &args.command {
        ContextCmd::Close { scope } => {
            control.close(&capability(scope)?)?;
            serde_json::json!({"closed":true})
        }
        ContextCmd::Create {
            context_window,
            client_context_window,
            output_reserve,
            adaptive,
            port,
            ..
        } => {
            let scope = control.create(
                *context_window,
                *client_context_window,
                *output_reserve,
                *adaptive,
            )?;
            serde_json::json!({"scope":scope,"base_url":format!("http://127.0.0.1:{port}/__gobstopper/s/{scope}"),
                "scope_boundary":"wrapper and any descendants sharing the capability",
                "capacity_source":"operator_configuration","status":control.status(&scope)?})
        }
        ContextCmd::Reserve {
            scope,
            tokens,
            requests,
            ttl_seconds,
            ..
        } => serde_json::to_value(control.reserve(
            &capability(scope)?,
            *tokens,
            *requests,
            *ttl_seconds,
        )?)?,
        ContextCmd::Status { scope, .. } => {
            serde_json::to_value(control.status(&capability(scope)?)?)?
        }
        ContextCmd::Release { scope, .. } => {
            serde_json::to_value(control.release(&capability(scope)?)?)?
        }
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn control() -> (Control, PathBuf) {
        let path = std::env::temp_dir()
            .join(format!("gobstopper-context-{}", random_id().unwrap()))
            .join("context.sqlite3");
        (Control::open(&path).unwrap(), path)
    }
    #[test]
    fn reservation_consumes_once_and_reopens_without_restoring_allowance() {
        let (control, path) = control();
        let scope = control
            .create(Some(1_000_000), Some(700_000), 32_000, false)
            .unwrap();
        assert_eq!(
            control
                .reserve(&scope, 800_000, 2, 60)
                .unwrap()
                .effective_input_tokens,
            668_000
        );
        let first = control.consume(&scope, 128_000, None, 100_000).unwrap();
        assert_eq!(first.effective_input_tokens, 600_000);
        assert_eq!(first.remaining_requests, 1);
        drop(control);
        let control = Control::open(&path).unwrap();
        assert_eq!(
            control
                .consume(&scope, 128_000, None, 32_000)
                .unwrap()
                .remaining_requests,
            0
        );
        assert_eq!(
            control
                .consume(&scope, 128_000, None, 32_000)
                .unwrap()
                .effective_input_tokens,
            128_000
        );
        drop(control);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn concurrent_first_openers_preserve_every_scope_and_reservation() {
        use std::sync::{Arc, Barrier};
        let path = std::env::temp_dir()
            .join(format!("gobstopper-context-open-{}", random_id().unwrap()))
            .join("context.sqlite3");
        let barrier = Arc::new(Barrier::new(16));
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let control = Control::open(&path)?;
                    let scope = control.create(Some(1_000_000), None, 32_000, false)?;
                    control.reserve(&scope, 500_000, 3, 60)?;
                    Ok::<_, anyhow::Error>(scope)
                })
            })
            .collect();
        // Join all writers before inspecting their results or cleaning up.
        let results: Vec<_> = workers.into_iter().map(|worker| worker.join()).collect();
        let control = Control::open(&path).unwrap();
        for result in results {
            let scope = result.unwrap().unwrap();
            let decision = control.consume(&scope, 128_000, None, 32_000).unwrap();
            assert_eq!(decision.requested_input_tokens, Some(500_000));
            assert_eq!(decision.remaining_requests, 2);
        }
        drop(control);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn unknown_capacity_never_promises_a_larger_budget_and_scope_isolation() {
        let (control, path) = control();
        let a = control.create(None, None, 32_000, false).unwrap();
        let b = control
            .create(Some(1_000_000), None, 32_000, false)
            .unwrap();
        let result = control.reserve(&a, 500_000, 20, 60).unwrap();
        assert_eq!(result.effective_input_tokens, 128_000);
        assert_eq!(result.limiting_reason, "capacity_unknown");
        assert_eq!(control.status(&b).unwrap().requested_input_tokens, None);
        assert!(control.reserve("../../bad", 500_000, 20, 60).is_err());
        drop(control);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn clock_reversal_expiry_and_output_headroom_are_bounded() {
        let row = Scope {
            window: Some(1000),
            client_window: None,
            output: 200,
            requested: Some(900),
            remaining: 2,
            created: 100,
            expires: Some(200),
            generation: 1,
            adaptive: false,
            rereads: 0,
            reread_since: 0,
            rescues: 0,
            cooldown: 0,
            base: 100,
        };
        assert_eq!(
            decide("a", &row, 100, None, 0, 99).requested_input_tokens,
            None
        );
        assert_eq!(
            decide("a", &row, 100, None, 0, 200).requested_input_tokens,
            None
        );
        assert_eq!(
            decide("a", &row, 100, None, 500, 150).effective_input_tokens,
            500
        );
        assert_eq!(
            decide("a", &row, 100, None, 1500, 150).input_capacity_tokens,
            Some(0)
        );
    }
    #[test]
    fn concurrent_consumers_cannot_overdraw() {
        let (control, path) = control();
        let scope = control
            .create(Some(1_000_000), None, 32_000, false)
            .unwrap();
        control.reserve(&scope, 500_000, 3, 60).unwrap();
        drop(control);
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let scope = scope.clone();
                std::thread::spawn(move || {
                    Control::open(&path)
                        .unwrap()
                        .consume(&scope, 128_000, None, 0)
                        .unwrap()
                        .requested_input_tokens
                        .is_some()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum::<usize>(),
            3
        );
        // Separate connections exercise SQLite's transactional writer exclusion.
        let control = Control::open(&path).unwrap();
        assert_eq!(control.status(&scope).unwrap().remaining_requests, 0);
        drop(control);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn stale_adaptive_decision_cannot_replace_a_new_reservation() {
        let (control, path) = control();
        let scope = control.create(Some(1_000_000), None, 32_000, true).unwrap();
        let old = control.consume(&scope, 128_000, None, 0).unwrap();
        let observation = |id: &str| EvidenceObservation {
            source_id: id.into(),
            content_digest: "unchanged-evidence".into(),
            kind: "text".into(),
        };
        control
            .observe(
                &scope,
                &[observation("old")],
                &["unchanged-evidence".into()],
                &old,
            )
            .unwrap();
        control.reserve(&scope, 500_000, 20, 600).unwrap();
        assert!(!control
            .observe(
                &scope,
                &[
                    observation("reread-1"),
                    observation("reread-2"),
                    observation("reread-3")
                ],
                &[],
                &old
            )
            .unwrap());
        let current = control.status(&scope).unwrap();
        assert_eq!(current.requested_input_tokens, Some(500_000));
        assert_eq!(current.remaining_requests, 20);
        drop(control);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

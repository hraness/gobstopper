//! Local observation storage and content-free, evidence-aware data commands.
pub mod importers;
pub mod metrics;
pub mod schema;
mod store;

pub use schema::*;
pub use store::{default_path, prepare_private_dir, IdentityNamespace, PageOptions, Query, Store};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Write};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct DataArgs {
    /// Private local state directory (default: GOBSTOPPER_DATA_DIR or XDG data).
    #[arg(long, global = true)]
    pub state_dir: Option<PathBuf>,
    /// Emit JSON; data commands always use versioned machine-readable output.
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: DataCommand,
}

#[derive(Debug, Args, Default)]
pub struct Selection {
    /// Inclusive Unix timestamp in milliseconds.
    #[arg(long)]
    pub since_ms: Option<u64>,
    /// Exclusive Unix timestamp in milliseconds.
    #[arg(long)]
    pub until_ms: Option<u64>,
    /// Opaque local session ID printed by data sessions.
    #[arg(long)]
    pub session: Option<String>,
}
impl Selection {
    fn query(&self) -> Query {
        Query {
            since_ms: self.since_ms,
            until_ms: self.until_ms,
            session_id: self.session.clone().map(OpaqueId),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum DataCommand {
    /// Local schema, event count and incomplete inference attempts.
    Status,
    /// Observed sessions; requests without a known provider session stay unassigned.
    Sessions(Selection),
    /// Inference attempts, including retries and incomplete attempts.
    Requests(Selection),
    /// Observed tool invocations and explicit outcomes.
    Tools(Selection),
    /// Pure metric lenses with measured/missing coverage and explicit denominators.
    Metrics {
        #[command(flatten)]
        selection: Selection,
        #[arg(
            long,
            help = "Read deadline in milliseconds (default 120000; 1..=600000)."
        )]
        timeout_ms: Option<u64>,
    },
    #[command(
        about = "Page through validated observations without changing session, request or tool reports."
    )]
    Events {
        #[command(flatten)]
        selection: Selection,
        #[arg(long, default_value_t = 0)]
        after_sequence: u64,
        #[arg(
            long,
            help = "Snapshot sequence from the first page; repeat it on later pages."
        )]
        through_sequence: Option<u64>,
        #[arg(
            long,
            default_value_t = 1000,
            help = "Maximum observations in this page (1..=10000)."
        )]
        limit: usize,
        #[arg(long, default_value_t = store::DEFAULT_READ_TIMEOUT_MS, help = "Read deadline in milliseconds (1..=600000).")]
        timeout_ms: u64,
    },
    #[command(
        about = "Copy observations into a new private directory of checksummed import files; leave the database unchanged."
    )]
    Archive {
        #[command(flatten)]
        selection: Selection,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = store::MAX_BATCH_EVENTS, help = "Maximum observations per segment (1..=10000).")]
        segment_events: usize,
        #[arg(long, default_value_t = store::DEFAULT_READ_TIMEOUT_MS, help = "Read deadline in milliseconds (1..=600000).")]
        timeout_ms: u64,
    },
    #[command(about = "Verify an archive's completion marker, manifest and every import segment.")]
    ArchiveCheck {
        input: PathBuf,
        #[arg(long, default_value_t = store::DEFAULT_READ_TIMEOUT_MS, help = "Read deadline in milliseconds (1..=600000).")]
        timeout_ms: u64,
    },
    /// Export a bounded, portable, checksummed observation snapshot.
    Export {
        #[command(flatten)]
        selection: Selection,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Atomically import a portable export; exact replay is idempotent.
    Import { input: PathBuf },
    /// Import legacy proxy stats as context estimates, never measured usage.
    ImportLegacy {
        input: PathBuf,
        #[arg(long)]
        source_id: Option<String>,
    },
    /// Read content-free metadata from one Claude or Codex transcript.
    ImportNative {
        #[arg(long, value_enum)]
        provider: importers::NativeProvider,
        input: PathBuf,
    },
    /// Check SQLite, event digests, schema versions and projection integrity.
    Check,
    /// Create and verify a new SQLite backup including committed WAL data.
    Backup { output: PathBuf },
}

fn print(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn input(path: &PathBuf) -> Result<BufReader<File>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).context("data_input_unavailable")?;
    if !file.metadata()?.is_file() {
        bail!("data_input_regular_file_required");
    }
    Ok(BufReader::new(file))
}

pub fn run(args: &DataArgs) -> Result<()> {
    if let DataCommand::ArchiveCheck { input, timeout_ms } = &args.command {
        return print(&store::check_archive(input, *timeout_ms)?);
    }
    let path = match &args.state_dir {
        Some(dir) => dir.join("sessions.sqlite3"),
        None => default_path()?,
    };
    let mut store = if matches!(
        &args.command,
        DataCommand::Import { .. }
            | DataCommand::ImportLegacy { .. }
            | DataCommand::ImportNative { .. }
    ) {
        Store::open(&path)?
    } else {
        Store::open_for_read(&path)?
    };
    match &args.command {
        DataCommand::Status => print(&store.status()?),
        DataCommand::Sessions(selection) => {
            print(&metrics::sessions(&store.events(&selection.query())?))
        }
        DataCommand::Requests(selection) => {
            print(&metrics::requests(&store.events(&selection.query())?))
        }
        DataCommand::Tools(selection) => print(&metrics::tools(&store.events(&selection.query())?)),
        DataCommand::Metrics {
            selection,
            timeout_ms,
        } => {
            let query = selection.query();
            let report = match timeout_ms {
                Some(timeout_ms) => store.metrics_with_timeout(&query, *timeout_ms)?,
                None => store.metrics(&query)?,
            };
            print(&report)
        }
        DataCommand::Events {
            selection,
            after_sequence,
            through_sequence,
            limit,
            timeout_ms,
        } => print(&store.event_page(
            &selection.query(),
            &PageOptions {
                after_sequence: *after_sequence,
                through_sequence: *through_sequence,
                limit: *limit,
                timeout_ms: *timeout_ms,
            },
        )?),
        DataCommand::Archive {
            selection,
            output,
            segment_events,
            timeout_ms,
        } => print(&store.archive(&selection.query(), output, *segment_events, *timeout_ms)?),
        DataCommand::ArchiveCheck { .. } => {
            unreachable!("archive inspection does not open a database")
        }
        DataCommand::Export { selection, output } => {
            let mut bytes = Vec::new();
            let count = store.export(&selection.query(), &mut bytes)?;
            if let Some(path) = output {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                let mut file = options
                    .open(path)
                    .context("data_export_destination_must_be_new")?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                print(&serde_json::json!({"schema_version":1,"exported_events":count}))
            } else {
                std::io::stdout().lock().write_all(&bytes)?;
                Ok(())
            }
        }
        DataCommand::Import { input: path } => print(&store.import(input(path)?)?),
        DataCommand::ImportLegacy {
            input: path,
            source_id,
        } => {
            let identity = source_id.clone().unwrap_or(
                std::fs::canonicalize(path)
                    .context("data_input_unavailable")?
                    .to_string_lossy()
                    .into_owned(),
            );
            let source = store.opaque("legacy-source", &identity);
            print(&importers::import_legacy(&mut store, source, input(path)?)?)
        }
        DataCommand::ImportNative {
            provider,
            input: path,
        } => print(&importers::import_native(
            &mut store,
            *provider,
            input(path)?,
        )?),
        DataCommand::Check => print(&store.check()?),
        DataCommand::Backup { output } => print(&store.backup(output)?),
    }
}

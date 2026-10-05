use anyhow::{bail, Result};
use gobstopper_core::events::{
    default_log_path, read_events_diagnostics, EventDiagnostics, EventDiagnosticsFilter,
};

pub(crate) fn run(
    session: Option<&str>,
    tail: usize,
    since: Option<&str>,
    json: bool,
) -> Result<()> {
    let since_ts = since
        .map(crate::parse_since)
        .transpose()?
        .map(|duration| crate::now_secs().saturating_sub(duration));
    let result = match read_events_diagnostics(
        &default_log_path(),
        EventDiagnosticsFilter {
            session_prefix: session,
            since_ts,
            tail,
        },
    ) {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            EventDiagnostics::absent(tail)
        }
        Err(_) => bail!("Event diagnostics are unavailable: the log could not be read safely."),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!(
        "Event diagnostics: {} rows shown from {} scanned records ({} readable, {} invalid, {} oversized).",
        result.coverage.exported_rows,
        result.coverage.scanned_records,
        result.coverage.readable_records,
        result.coverage.invalid_records,
        result.coverage.oversized_records,
    );
    println!("This view does not measure savings or retention.");
    if let Some(reason) = result.unavailable_reason {
        println!("Unavailable: {reason}");
    }
    if result.coverage.partial {
        println!(
            "Partial history: {}",
            result.coverage.partial_reasons.join(", ")
        );
    }
    for row in result.rows {
        println!(
            "  {}:{} {} {:<12} {:<18} {:<8} {} {}",
            row.generation,
            row.line_number,
            row.ts.map_or_else(|| "-".to_owned(), |ts| ts.to_string()),
            row.provider.map_or("-", |provider| provider.as_str()),
            row.action.unwrap_or("-"),
            row.outcome.unwrap_or("-"),
            row.session_id
                .as_deref()
                .map(|id| {
                    format!(
                        "id:{}",
                        &gobstopper_adapters::copy::sha256(id.as_bytes())[..12]
                    )
                })
                .as_deref()
                .unwrap_or("-"),
            row.reason
                .map_or("valid_metadata", |reason| reason.as_str()),
        );
    }
    Ok(())
}

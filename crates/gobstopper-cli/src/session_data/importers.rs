//! Explicit local imports. Native transcripts are read once, never modified.
use super::schema::*;
use super::store::{AppendReceipt, Store, MAX_BATCH_EVENTS};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::BufRead;

const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_LINE: usize = 16 * 1024 * 1024;
const MAX_SOURCE_LINES: u64 = 100_000;

#[derive(Debug, Serialize)]
pub struct ImportReceipt {
    pub append: AppendReceipt,
    pub parsed_lines: u64,
    pub unsupported_lines: u64,
    pub profile: &'static str,
}

fn timestamp(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|v| u64::try_from(v.timestamp_millis()).ok())
}

fn source_line(input: &mut impl BufRead, remaining: &mut u64) -> Result<Option<Vec<u8>>> {
    let mut result = Vec::new();
    loop {
        let buffer = input.fill_buf().context("data_source_read_failed")?;
        if buffer.is_empty() {
            if result.is_empty() {
                return Ok(None);
            }
            // Active writers may be in the middle of a record; importing such a
            // snapshot must not admit a partial line or alter already stored data.
            bail!("data_source_incomplete_line_retry_when_closed");
        }
        let n = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(buffer.len());
        if n as u64 > *remaining || result.len() + n > MAX_SOURCE_LINE {
            bail!("data_source_limit");
        }
        let end = buffer[n - 1] == b'\n';
        result.extend_from_slice(&buffer[..n]);
        input.consume(n);
        *remaining -= n as u64;
        if end {
            result.pop();
            return Ok(Some(result));
        }
    }
}

#[derive(Deserialize)]
struct Legacy {
    ts: String,
    est_tokens_in: u64,
    est_tokens_out: u64,
    compacted: bool,
}

/// Legacy timestamps and numeric context estimates are retained, without
/// inventing a request identity, token usage, terminal outcome or generation rate.
pub fn import_legacy(
    store: &mut Store,
    source_id: OpaqueId,
    mut input: impl BufRead,
) -> Result<ImportReceipt> {
    let mut remaining = MAX_SOURCE_BYTES;
    let mut rows = Vec::new();
    let mut parsed = 0;
    let mut occurrences = BTreeMap::<String, u64>::new();
    let source = Source {
        kind: SourceKind::LegacyStats,
        id: source_id,
        profile: "gobstopper-stats-v0".into(),
    };
    while let Some(line) = source_line(&mut input, &mut remaining)? {
        parsed += 1;
        if parsed > MAX_SOURCE_LINES || rows.len() == MAX_BATCH_EVENTS {
            bail!("data_source_limit");
        }
        let item: Legacy = serde_json::from_slice(&line).context("data_legacy_invalid_record")?;
        let observed_at_ms =
            timestamp(&item.ts).ok_or_else(|| anyhow::anyhow!("data_legacy_invalid_timestamp"))?;
        let event = Event::LegacyContext {
            estimated_before_tokens: item.est_tokens_in,
            estimated_after_tokens: item.est_tokens_out,
            compacted: item.compacted,
        };
        // Identical numeric records can be distinct observations. Preserve their
        // occurrence rank, so reimporting the same growing log remains idempotent.
        let identity = format!("{observed_at_ms}:{}", serde_json::to_string(&event)?);
        let occurrence = occurrences.entry(identity.clone()).or_default();
        *occurrence += 1;
        let row = Envelope {
            schema_version: EVENT_VERSION,
            event_id: store.opaque(
                "legacy-event",
                &format!("{}:{identity}:{occurrence}", source.id.0),
            ),
            source: source.clone(),
            observed_at_ms,
            identity: Identity::default(),
            event,
        };
        row.validate()?;
        rows.push(row);
    }
    Ok(ImportReceipt {
        append: store.append(&rows)?,
        parsed_lines: parsed,
        unsupported_lines: 0,
        profile: "gobstopper-stats-v0",
    })
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum NativeProvider {
    Claude,
    Codex,
}

/// Narrow metadata decoder: serde skips prompts, arguments, outputs, images,
/// paths and unrelated fields without retaining them in the parsed structure.
#[derive(Deserialize, Default)]
struct NativeLine {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    timestamp: Option<String>,
    message: Option<NativeMessage>,
    payload: Option<NativePayload>,
}
#[derive(Deserialize, Default)]
struct NativeMessage {
    id: Option<String>,
    stop_reason: Option<String>,
    usage: Option<NativeUsage>,
    #[serde(default, deserialize_with = "content_blocks")]
    content: Vec<NativeBlock>,
}
#[derive(Deserialize, Default)]
struct NativeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

fn content_blocks<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<NativeBlock>, D::Error> {
    struct Blocks;
    impl<'de> serde::de::Visitor<'de> for Blocks {
        type Value = Vec<NativeBlock>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("content text or blocks")
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_string<E: serde::de::Error>(
            self,
            _: String,
        ) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut blocks = Vec::new();
            while let Some(block) = seq.next_element()? {
                blocks.push(block);
                if blocks.len() > 10_000 {
                    return Err(serde::de::Error::custom("data_source_limit"));
                }
            }
            Ok(blocks)
        }
    }
    deserializer.deserialize_any(Blocks)
}
#[derive(Deserialize, Default)]
struct NativeBlock {
    #[serde(rename = "type", default)]
    kind: String,
    id: Option<String>,
    tool_use_id: Option<String>,
    name: Option<String>,
    is_error: Option<bool>,
}
#[derive(Deserialize, Default)]
struct NativePayload {
    #[serde(rename = "type", default)]
    kind: String,
    id: Option<String>,
    session_id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
}

fn native_id(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
}

pub fn import_native(
    store: &mut Store,
    provider: NativeProvider,
    mut input: impl BufRead,
) -> Result<ImportReceipt> {
    let (provider_name, profile) = match provider {
        NativeProvider::Claude => ("claude_code", "claude-metadata-v1"),
        NativeProvider::Codex => ("codex", "codex-metadata-v1"),
    };
    let mut remaining = MAX_SOURCE_BYTES;
    let mut rows = BTreeMap::<OpaqueId, Envelope>::new();
    let mut parsed = 0;
    let mut unsupported = 0;
    let mut codex_session: Option<String> = None;
    while let Some(line) = source_line(&mut input, &mut remaining)? {
        parsed += 1;
        if parsed > MAX_SOURCE_LINES {
            bail!("data_source_limit");
        }
        let item: NativeLine =
            serde_json::from_slice(&line).context("data_native_invalid_record")?;
        if matches!(provider, NativeProvider::Codex) && item.kind == "session_meta" {
            if let Some(payload) = &item.payload {
                if let Some(id) = native_id(payload.id.as_deref().or(payload.session_id.as_deref()))
                {
                    if codex_session.as_deref().is_some_and(|old| old != id) {
                        bail!("data_native_session_changed");
                    }
                    codex_session = Some(id.to_owned());
                }
            }
            continue;
        }
        let session = match provider {
            NativeProvider::Claude => native_id(item.session_id.as_deref()),
            NativeProvider::Codex => codex_session.as_deref(),
        };
        let Some(session) = session else {
            unsupported += 1;
            continue;
        };
        let Some(observed_at_ms) = item.timestamp.as_deref().and_then(timestamp) else {
            unsupported += 1;
            continue;
        };
        let source = Source {
            kind: SourceKind::NativeTranscript,
            id: store.opaque(
                "native-source",
                &serde_json::to_string(&(provider_name, session))?,
            ),
            profile: profile.into(),
        };
        let session_id = store.opaque(
            "native-session",
            &serde_json::to_string(&(provider_name, session))?,
        );
        let mut recognized = false;
        if matches!(provider, NativeProvider::Claude) && item.kind == "assistant" {
            if let (Some(request), Some(message)) =
                (native_id(item.request_id.as_deref()), item.message.as_ref())
            {
                if let (Some(response), Some(stop), Some(usage)) = (
                    native_id(message.id.as_deref()),
                    message.stop_reason.as_deref(),
                    message.usage.as_ref(),
                ) {
                    if matches!(
                        stop,
                        "end_turn"
                            | "tool_use"
                            | "max_tokens"
                            | "stop_sequence"
                            | "pause_turn"
                            | "refusal"
                    ) {
                        // Missing cache components make inclusive input unknown.
                        // Never substitute zero for an omitted provider counter.
                        let input = usage
                            .input_tokens
                            .zip(usage.cache_read_input_tokens)
                            .zip(usage.cache_creation_input_tokens)
                            .and_then(|((input, read), write)| {
                                input.checked_add(read)?.checked_add(write)
                            });
                        let row = Envelope {
                            schema_version: EVENT_VERSION,
                            event_id: store.opaque(
                                "native-terminal",
                                &serde_json::to_string(&(
                                    provider_name,
                                    session,
                                    request,
                                    response,
                                ))?,
                            ),
                            source: source.clone(),
                            observed_at_ms,
                            identity: Identity {
                                session_id: Some(session_id.clone()),
                                request_id: Some(store.opaque(
                                    "native-request",
                                    &serde_json::to_string(&(provider_name, session, request))?,
                                )),
                                attempt_id: Some(store.opaque(
                                    "native-attempt",
                                    &serde_json::to_string(&(
                                        provider_name,
                                        session,
                                        request,
                                        response,
                                    ))?,
                                )),
                                ..Identity::default()
                            },
                            event: Event::RequestFinished {
                                outcome: if stop == "refusal" {
                                    Outcome::Refused
                                } else {
                                    Outcome::Success
                                },
                                http_status: None,
                                duration_ms: None,
                                first_output_ms: None,
                                generation: None,
                                usage: Some(Usage {
                                    input_tokens: input.map(Quantity::reported),
                                    output_tokens: usage.output_tokens.map(Quantity::reported),
                                    cache_read_tokens: usage
                                        .cache_read_input_tokens
                                        .map(Quantity::reported),
                                    cache_write_tokens: usage
                                        .cache_creation_input_tokens
                                        .map(Quantity::reported),
                                    reasoning_tokens: None,
                                }),
                            },
                        };
                        row.validate()?;
                        if let Some(previous) = rows.insert(row.event_id.clone(), row.clone()) {
                            if previous != row {
                                bail!("data_native_observation_changed");
                            }
                        }
                        recognized = true;
                    }
                }
            }
        }
        if rows.len() > MAX_BATCH_EVENTS {
            bail!("data_source_limit");
        }
        let mut observed = Vec::<(String, ToolStage, Outcome, Option<String>)>::new();
        match provider {
            NativeProvider::Claude => {
                if matches!(item.kind.as_str(), "assistant" | "user") {
                    for block in item.message.map(|m| m.content).unwrap_or_default() {
                        let (id, stage, outcome) = match block.kind.as_str() {
                            "tool_use" => (
                                native_id(block.id.as_deref()),
                                ToolStage::Requested,
                                Outcome::Unknown,
                            ),
                            "tool_result" => (
                                native_id(block.tool_use_id.as_deref()),
                                ToolStage::Terminal,
                                match block.is_error {
                                    Some(true) => Outcome::Error,
                                    Some(false) => Outcome::Success,
                                    None => Outcome::Unknown,
                                },
                            ),
                            _ => continue,
                        };
                        if let Some(id) = id {
                            observed.push((
                                id.to_owned(),
                                stage,
                                outcome,
                                block.name.filter(|name| safe_label(name)),
                            ));
                        }
                    }
                }
            }
            NativeProvider::Codex => {
                if item.kind == "response_item" {
                    if let Some(payload) = item.payload {
                        let stage = match payload.kind.as_str() {
                            "function_call" | "custom_tool_call" | "local_shell_call" => {
                                Some(ToolStage::Requested)
                            }
                            "function_call_output" | "custom_tool_call_output" => {
                                Some(ToolStage::Terminal)
                            }
                            _ => None,
                        };
                        if let (Some(stage), Some(id)) =
                            (stage, native_id(payload.call_id.as_deref()))
                        {
                            observed.push((
                                id.to_owned(),
                                stage,
                                Outcome::Unknown,
                                payload.name.filter(|name| safe_label(name)),
                            ));
                        }
                    }
                }
            }
        }
        if observed.is_empty() {
            if !recognized {
                unsupported += 1;
            }
            continue;
        }
        for (tool, stage, outcome, tool_name) in observed {
            let row = Envelope {
                schema_version: EVENT_VERSION,
                event_id: store.opaque(
                    "native-tool-observation",
                    &serde_json::to_string(&(provider_name, session, &tool, stage))?,
                ),
                source: source.clone(),
                observed_at_ms,
                identity: Identity {
                    session_id: Some(session_id.clone()),
                    tool_id: Some(store.opaque(
                        "native-tool",
                        &serde_json::to_string(&(provider_name, session, &tool))?,
                    )),
                    ..Identity::default()
                },
                event: Event::ToolObserved {
                    stage,
                    outcome,
                    tool_name,
                },
            };
            row.validate()?;
            if let Some(previous) = rows.insert(row.event_id.clone(), row.clone()) {
                if previous != row {
                    bail!("data_native_observation_changed");
                }
            }
            if rows.len() > MAX_BATCH_EVENTS {
                bail!("data_source_limit");
            }
        }
    }
    let events: Vec<_> = rows.into_values().collect();
    Ok(ImportReceipt {
        append: store.append(&events)?,
        parsed_lines: parsed,
        unsupported_lines: unsupported,
        profile,
    })
}

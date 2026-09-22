use crate::{copy, transaction, verify};
use anyhow::{bail, ensure, Context, Result};
use gobstopper_core::strategy::{ElideStrategy, PolicyConfig, Strategy};
use gobstopper_core::validation::MAX_DIGEST_BYTES;
use gobstopper_core::{DigestBlock, Edit, Provider, SessionHandle, Transcript};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::time::Instant;

pub const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeKind {
    Constraint,
    Procedure,
    OpenTask,
    Fact,
    Preference,
    Episode,
}

impl KnowledgeKind {
    fn pinned(self) -> bool {
        matches!(self, Self::Constraint | Self::Procedure | Self::OpenTask)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionCheck {
    pub id: String,
    pub kind: KnowledgeKind,
    pub record_index: usize,
    pub pointer: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Growth {
    pub after_round: usize,
    pub records: Vec<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub source_sha256: String,
    pub label_source: String,
    pub checks: Vec<RetentionCheck>,
    #[serde(default)]
    pub growth: Vec<Growth>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Origin {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

struct Slot {
    record: usize,
    pointer: String,
    origin: Origin,
    text: String,
}

struct BoundCheck {
    id: String,
    kind: KnowledgeKind,
    record: usize,
    pointer: String,
    origin: Origin,
    elidable: bool,
    text: String,
}

/// Replay arms: plain observation masking; typed masking (pinned records
/// stay); typed digest (pinned records are elided but their spans are carried
/// verbatim on an injected state card — same provenance downgrade a summary
/// would impose, measured explicitly by the retention tiers).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    Masking,
    Typed,
    Digest,
}

impl Arm {
    fn name(self) -> &'static str {
        match self {
            Self::Masking => "observation_masking",
            Self::Typed => "typed_masking",
            Self::Digest => "typed_digest",
        }
    }
}

/// Build a state card carrying the pinned check spans verbatim. Returns the
/// digest and how many pinned checks fit under `MAX_DIGEST_BYTES`.
fn retention_card(checks: &[BoundCheck], covers_items: usize) -> (DigestBlock, usize) {
    let mut digest = DigestBlock {
        summary: Some("typed retention card: pinned spans verbatim".to_string()),
        covers_items,
        ..Default::default()
    };
    let mut size = digest.summary.as_ref().map_or(0, |s| s.len());
    let mut carried = 0;
    for check in checks.iter().filter(|c| c.kind.pinned()) {
        let text = match check.kind {
            KnowledgeKind::Constraint => format!("constraint: {}", check.text),
            KnowledgeKind::Procedure => format!("procedure: {}", check.text),
            _ => check.text.clone(),
        };
        if size + text.len() > MAX_DIGEST_BYTES {
            break;
        }
        size += text.len();
        carried += 1;
        match check.kind {
            KnowledgeKind::OpenTask => digest.open_tasks.push(text),
            KnowledgeKind::Procedure => digest.concepts.push(text),
            _ => digest.decisions.push(text),
        }
    }
    (digest, carried)
}

#[derive(Debug, Default, Serialize)]
pub struct RetentionScore {
    pub total: usize,
    pub retained: usize,
    pub same_origin_retained: usize,
    pub source_bound_retained: usize,
    pub elidable_total: usize,
    pub elidable_retained: usize,
    pub missing_ids: Vec<String>,
    pub by_kind: BTreeMap<KnowledgeKind, [usize; 2]>,
}

#[derive(Debug, Serialize)]
pub struct RoundResult {
    pub arm: &'static str,
    /// Pinned checks carried on the injected retention card (`typed_digest`
    /// arm only).
    pub card_items: usize,
    pub round: usize,
    pub status: &'static str,
    pub applied_rounds: usize,
    pub source_bytes: usize,
    pub result_bytes: usize,
    pub result_sha256: String,
    pub estimated_context_before: u64,
    pub estimated_context_after: u64,
    pub floor_reached: bool,
    pub pinned_records: usize,
    pub verify_errors: usize,
    pub verify_warnings: usize,
    pub new_verify_errors: usize,
    pub retention: RetentionScore,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct StudyReport {
    pub schema: &'static str,
    pub provider: Provider,
    pub source_sha256: String,
    pub manifest_sha256: String,
    pub source_verify_errors: usize,
    pub source_verify_warnings: usize,
    pub min_savings_tokens: u64,
    pub label_source: String,
    pub replay_mode: &'static str,
    pub provider_calls: usize,
    pub billed_cost_usd: Option<f64>,
    pub continuation_success: Option<bool>,
    pub trigger_tokens: u64,
    pub floor_tokens: u64,
    pub keep_recent_tool_outputs: usize,
    pub rows: Vec<RoundResult>,
    pub unmeasured: Vec<&'static str>,
    pub limitations: Vec<&'static str>,
}

pub fn read_manifest(path: &Path) -> Result<(Manifest, String)> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_MANIFEST_BYTES,
        "manifest must be a bounded regular file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST_BYTES,
        "manifest exceeds byte limit"
    );
    let manifest = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid retention manifest"))?;
    Ok((manifest, copy::sha256(&bytes)))
}

fn classify_candidate(text: &str) -> Option<KnowledgeKind> {
    let lower = text.to_ascii_lowercase();
    if ["never ", "must not ", "do not ", "only if "]
        .iter()
        .any(|key| lower.contains(key))
    {
        Some(KnowledgeKind::Constraint)
    } else if lower.contains("- [ ]") || lower.starts_with("todo:") || lower.starts_with("pending:")
    {
        Some(KnowledgeKind::OpenTask)
    } else if [
        "cargo ", "npm ", "bun ", "pytest ", "python3 ", "git ", "run `",
    ]
    .iter()
    .any(|key| lower.trim_start_matches(['-', ' ', '`']).starts_with(key))
    {
        Some(KnowledgeKind::Procedure)
    } else if lower.starts_with("error:")
        || lower.starts_with("decision:")
        || lower.starts_with("failed")
    {
        Some(KnowledgeKind::Fact)
    } else {
        None
    }
}

pub fn prepare_manifest(handle: SessionHandle, bytes: &[u8], output: &Path) -> Result<usize> {
    let transcript = parse(handle, bytes)?;
    let mut texts = slots(&transcript, bytes)?;
    let elidable: HashSet<_> = transcript
        .items
        .iter()
        .filter(|i| i.elidable_bytes.is_some())
        .map(|i| i.line_index)
        .collect();
    texts.sort_by_key(|slot| (!elidable.contains(&slot.record), slot.record));
    let mut checks = Vec::new();
    let mut kinds = BTreeMap::<KnowledgeKind, usize>::new();
    let mut seen = HashSet::new();
    for slot in texts {
        let mut offset = 0;
        for line in slot.text.split_inclusive('\n') {
            let text = line.trim();
            if let Some(kind) = classify_candidate(text) {
                let count = kinds.entry(kind).or_default();
                if *count < 16 && text.len() <= 4096 && seen.insert(text.to_string()) {
                    let start = offset + line.find(text).unwrap_or(0);
                    checks.push(RetentionCheck {
                        id: format!("check-{}", checks.len()),
                        kind,
                        record_index: slot.record,
                        pointer: slot.pointer.clone(),
                        start_byte: start,
                        end_byte: start + text.len(),
                        sha256: copy::sha256(text.as_bytes()),
                    });
                    *count += 1;
                }
            }
            offset += line.len();
        }
    }
    ensure!(
        !checks.is_empty(),
        "no heuristic retention candidates; supply reviewed annotations"
    );
    let count = checks.len();
    let manifest = Manifest {
        schema: "gobstopper-retention-v1".to_string(),
        source_sha256: copy::sha256(bytes),
        label_source: "heuristic".to_string(),
        checks,
        growth: Vec::new(),
    };
    let data = serde_json::to_vec_pretty(&manifest)?;
    transaction::publish_new(output, &data)?;
    Ok(count)
}

fn origin(role: &str) -> Option<Origin> {
    match role {
        "system" => Some(Origin::System),
        "developer" => Some(Origin::Developer),
        "user" => Some(Origin::User),
        "assistant" => Some(Origin::Assistant),
        "tool" => Some(Origin::Tool),
        _ => None,
    }
}

fn content(value: &Value, pointer: String, role: Origin, record: usize, out: &mut Vec<Slot>) {
    match value {
        Value::String(text) => out.push(Slot {
            record,
            pointer,
            origin: role,
            text: text.clone(),
        }),
        Value::Array(blocks) => {
            for (i, block) in blocks.iter().enumerate() {
                let path = format!("{pointer}/{i}");
                match block.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text" | "output_text") => {
                        if let Some(text) = block.get("text") {
                            content(text, format!("{path}/text"), role, record, out);
                        }
                    }
                    Some("tool_result") => {
                        if let Some(text) = block.get("content") {
                            content(text, format!("{path}/content"), Origin::Tool, record, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn message(value: &Value, pointer: &str, record: usize, out: &mut Vec<Slot>) {
    if let Some(role) = value.get("role").and_then(Value::as_str).and_then(origin) {
        if let Some(body) = value.get("content") {
            content(body, format!("{pointer}/content"), role, record, out);
        }
    }
}

fn slots(transcript: &Transcript, bytes: &[u8]) -> Result<Vec<Slot>> {
    let live: HashSet<_> = transcript
        .items
        .iter()
        .filter(|i| i.est_tokens > 0)
        .map(|i| i.line_index)
        .collect();
    let mut out = Vec::new();
    for (record, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if !live.contains(&record) {
            continue;
        }
        let value: Value = serde_json::from_slice(line).context("invalid live record")?;
        match transcript.session.provider {
            Provider::ClaudeCode => message(&value["message"], "/message", record, &mut out),
            Provider::Devin => message(&value["chat_message"], "/chat_message", record, &mut out),
            Provider::Codex => {
                if value["type"] == "compacted" {
                    if let Some(history) = value
                        .pointer("/payload/replacement_history")
                        .and_then(Value::as_array)
                    {
                        for (i, item) in history.iter().enumerate() {
                            codex_item(
                                item,
                                &format!("/payload/replacement_history/{i}"),
                                record,
                                &mut out,
                            );
                        }
                    }
                } else if value["type"] == "response_item" {
                    codex_item(&value["payload"], "/payload", record, &mut out);
                }
            }
        }
    }
    Ok(out)
}

fn codex_item(item: &Value, pointer: &str, record: usize, out: &mut Vec<Slot>) {
    match item["type"].as_str() {
        Some("message") => message(item, pointer, record, out),
        Some("function_call_output" | "custom_tool_call_output") => {
            content(
                &item["output"],
                format!("{pointer}/output"),
                Origin::Tool,
                record,
                out,
            );
        }
        _ => {}
    }
}

fn parse(mut handle: SessionHandle, bytes: &[u8]) -> Result<Transcript> {
    ensure!(
        bytes.len() <= MAX_SOURCE_BYTES,
        "study source exceeds 64 MiB"
    );
    ensure!(
        bytes.split(|b| *b == b'\n').count() <= gobstopper_core::validation::MAX_ITEMS + 1,
        "study source exceeds record limit"
    );
    handle.age_secs = u64::MAX;
    let mut transcript = match handle.provider {
        Provider::Codex => crate::codex::load_bytes(handle, bytes),
        Provider::ClaudeCode => crate::claude::load_bytes(handle, bytes),
        Provider::Devin => crate::devin::load_bytes(handle, bytes),
    }?;
    transcript.usage.context_tokens = transcript.estimated_context_tokens();
    Ok(transcript)
}

fn bind(manifest: &Manifest, transcript: &Transcript, bytes: &[u8]) -> Result<Vec<BoundCheck>> {
    ensure!(
        manifest.schema == "gobstopper-retention-v1",
        "unsupported retention schema"
    );
    ensure!(
        matches!(
            manifest.label_source.as_str(),
            "reviewed" | "heuristic" | "synthetic"
        ),
        "unsupported label source"
    );
    ensure!(
        manifest.source_sha256 == copy::sha256(bytes),
        "retention source hash mismatch"
    );
    ensure!(
        !manifest.checks.is_empty() && manifest.checks.len() <= 256,
        "retention checks must contain 1..=256 entries"
    );
    let texts = slots(transcript, bytes)?;
    let mut ids = HashSet::new();
    let mut checks = Vec::new();
    for check in &manifest.checks {
        ensure!(
            !check.id.is_empty()
                && check.id.len() <= 64
                && check
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                && ids.insert(&check.id),
            "invalid or duplicate retention id"
        );
        ensure!(
            check.pointer.len() <= 512
                && check.end_byte.saturating_sub(check.start_byte) <= 4096
                && check.end_byte > check.start_byte,
            "invalid retention span"
        );
        let slot = texts
            .iter()
            .find(|s| s.record == check.record_index && s.pointer == check.pointer)
            .context("retention reference is not live textual content")?;
        let text = slot
            .text
            .get(check.start_byte..check.end_byte)
            .context("retention span is outside UTF-8 boundaries")?;
        ensure!(
            !text.trim().is_empty() && copy::sha256(text.as_bytes()) == check.sha256,
            "retention text hash mismatch"
        );
        checks.push(BoundCheck {
            id: check.id.clone(),
            kind: check.kind,
            record: check.record_index,
            pointer: check.pointer.clone(),
            origin: slot.origin,
            elidable: transcript
                .items
                .iter()
                .any(|i| i.line_index == check.record_index && i.elidable_bytes.is_some()),
            text: text.to_string(),
        });
    }
    Ok(checks)
}

fn score(checks: &[BoundCheck], texts: &[Slot]) -> RetentionScore {
    let mut score = RetentionScore {
        total: checks.len(),
        ..Default::default()
    };
    for check in checks {
        let retained = texts.iter().any(|s| s.text.contains(&check.text));
        let same_origin = texts
            .iter()
            .any(|s| s.origin == check.origin && s.text.contains(&check.text));
        let source_bound = texts.iter().any(|s| {
            s.record == check.record
                && s.pointer == check.pointer
                && s.origin == check.origin
                && s.text.contains(&check.text)
        });
        score.retained += usize::from(retained);
        score.same_origin_retained += usize::from(same_origin);
        score.source_bound_retained += usize::from(source_bound);
        score.elidable_total += usize::from(check.elidable);
        score.elidable_retained += usize::from(check.elidable && source_bound);
        let count = score.by_kind.entry(check.kind).or_default();
        count[0] += 1;
        count[1] += usize::from(source_bound);
        if !source_bound {
            score.missing_ids.push(check.id.clone());
        }
    }
    score
}

pub fn evaluate(
    handle: SessionHandle,
    bytes: &[u8],
    manifest: Manifest,
    manifest_sha256: String,
    policy: &PolicyConfig,
    rounds: usize,
) -> Result<StudyReport> {
    ensure!((1..=10).contains(&rounds), "rounds must be 1..=10");
    let initial = parse(handle.clone(), bytes)?;
    let checks = bind(&manifest, &initial, bytes)?;
    let source_findings = verify::verify(handle.provider, bytes);
    let mut growth_rounds = HashSet::new();
    for growth in &manifest.growth {
        ensure!(
            handle.provider != Provider::Devin,
            "Devin study growth requires provider-authored chain snapshots; static replay only"
        );
        ensure!(
            growth.after_round > 0
                && growth.after_round < rounds
                && growth_rounds.insert(growth.after_round)
                && !growth.records.is_empty(),
            "invalid growth schedule"
        );
        ensure!(
            growth.records.iter().all(Value::is_object),
            "growth must contain JSON objects"
        );
    }
    let pinned: HashSet<_> = checks
        .iter()
        .filter(|c| c.kind.pinned())
        .map(|c| c.record)
        .collect();
    let mut rows = Vec::new();
    for arm in [Arm::Masking, Arm::Typed, Arm::Digest] {
        let typed = arm == Arm::Typed;
        let temp = transaction::Temporary::new(&std::env::temp_dir(), bytes)?;
        let mut applied = 0;
        let began = Instant::now();
        for round in 1..=rounds {
            ensure!(
                began.elapsed().as_secs() < 120,
                "study arm exceeded time budget"
            );
            let before = transaction::read(&temp.path)?;
            let mut transcript = parse(handle.clone(), &before)?;
            let before_tokens = transcript.context_tokens();
            let mut card_items = 0;
            let mut selection_policy = policy.clone();
            selection_policy.floor_tokens = 0;
            let mut plan = ElideStrategy.evaluate(&transcript, &selection_policy);
            if let Some(plan) = &mut plan {
                let savings: BTreeMap<_, _> = transcript
                    .items
                    .iter()
                    .map(|item| (item.line_index, item.estimated_elision_savings()))
                    .collect();
                let mut projected = before_tokens;
                for edit in &mut plan.edits {
                    if let Edit::Elide { line_indexes, .. } = edit {
                        line_indexes.retain(|line| {
                            let saved = savings.get(line).copied().unwrap_or(0);
                            if projected <= policy.floor_tokens
                                || saved == 0
                                || (typed && pinned.contains(line))
                            {
                                return false;
                            }
                            projected = projected.saturating_sub(saved);
                            true
                        });
                    }
                }
                plan.edits.retain(
                    |e| !matches!(e, Edit::Elide { line_indexes, .. } if line_indexes.is_empty()),
                );
                if arm == Arm::Digest && !plan.edits.is_empty() {
                    let covered: usize = plan
                        .edits
                        .iter()
                        .map(|e| match e {
                            Edit::Elide { line_indexes, .. } => line_indexes.len(),
                            _ => 0,
                        })
                        .sum();
                    let (digest, carried) = retention_card(&checks, covered);
                    card_items = carried;
                    projected += digest.estimate_overhead();
                    plan.edits.push(Edit::InjectDigest { digest });
                }
                plan.context_tokens_after = projected;
                if !policy.accepts_savings(before_tokens, projected) {
                    plan.edits.clear();
                }
            }
            let start = Instant::now();
            if let Some(plan) = &plan {
                gobstopper_core::validation::validate_edits(&transcript, policy, &plan.edits)
                    .map_err(anyhow::Error::msg)?;
                match handle.provider {
                    Provider::Codex => crate::codex::apply(&temp.path, &plan.edits),
                    Provider::ClaudeCode => crate::claude::apply(&temp.path, &plan.edits),
                    Provider::Devin => crate::devin::apply(&temp.path, &plan.edits),
                }?;
            }
            let after = transaction::read(&temp.path)?;
            let changed = before != after;
            applied += usize::from(changed);
            transcript = parse(handle.clone(), &after)?;
            let findings = verify::verify(handle.provider, &after);
            let after_slots = slots(&transcript, &after)?;
            let retention = score(&checks, &after_slots);
            match arm {
                Arm::Typed => ensure!(
                    checks
                        .iter()
                        .filter(|c| c.kind.pinned())
                        .all(|c| !retention.missing_ids.contains(&c.id)),
                    "typed replay lost pinned content"
                ),
                Arm::Digest => ensure!(
                    checks
                        .iter()
                        .filter(|c| c.kind.pinned())
                        .take(card_items)
                        .all(|c| after_slots.iter().any(|s| s.text.contains(&c.text))),
                    "digest replay lost card-carried text"
                ),
                Arm::Masking => {}
            }
            rows.push(RoundResult {
                arm: arm.name(),
                card_items,
                round,
                status: if changed { "applied" } else { "no_change" },
                applied_rounds: applied,
                source_bytes: before.len(),
                result_bytes: after.len(),
                result_sha256: copy::sha256(&after),
                estimated_context_before: before_tokens,
                estimated_context_after: transcript.context_tokens(),
                floor_reached: transcript.context_tokens() <= policy.floor_tokens,
                pinned_records: if typed { pinned.len() } else { 0 },
                verify_errors: findings
                    .iter()
                    .filter(|f| f.severity == verify::Severity::Error)
                    .count(),
                verify_warnings: findings
                    .iter()
                    .filter(|f| f.severity == verify::Severity::Warning)
                    .count(),
                new_verify_errors: findings
                    .iter()
                    .filter(|f| {
                        f.severity == verify::Severity::Error && !source_findings.contains(f)
                    })
                    .count(),
                retention,
                duration_ms: start.elapsed().as_millis() as u64,
            });
            if let Some(growth) = manifest.growth.iter().find(|g| g.after_round == round) {
                let mut next =
                    String::from_utf8(after.clone()).context("invalid UTF-8 transcript")?;
                for record in &growth.records {
                    transaction::append_record(&mut next, record)?;
                }
                let future = parse(handle.clone(), next.as_bytes())?;
                let findings = verify::verify(handle.provider, next.as_bytes());
                ensure!(
                    findings
                        .iter()
                        .all(|f| f.severity != verify::Severity::Error),
                    "growth introduces invalid transcript"
                );
                if typed {
                    let future_slots = slots(&future, next.as_bytes())?;
                    ensure!(
                        checks
                            .iter()
                            .filter(|c| c.kind.pinned())
                            .all(|c| future_slots.iter().any(|s| s.record == c.record
                                && s.origin == c.origin
                                && s.text.contains(&c.text))),
                        "growth retired pinned context without an explicit revision"
                    );
                }
                transaction::replace(&temp.path, &after, next.as_bytes())?;
            }
        }
    }
    if rows.is_empty() {
        bail!("study produced no rows");
    }
    Ok(StudyReport {
        schema: "gobstopper-retention-study-v1", provider: handle.provider,
        source_sha256: copy::sha256(bytes), manifest_sha256, label_source: manifest.label_source,
        source_verify_errors: source_findings.iter().filter(|f| f.severity == verify::Severity::Error).count(),
        source_verify_warnings: source_findings.iter().filter(|f| f.severity == verify::Severity::Warning).count(),
        min_savings_tokens: policy.min_savings_tokens,
        replay_mode: if manifest.growth.is_empty() { "static_stress" } else { "supplied_growth" },
        provider_calls: 0, billed_cost_usd: None, continuation_success: None,
        trigger_tokens: policy.effective_trigger(), floor_tokens: policy.floor_tokens,
        keep_recent_tool_outputs: policy.keep_recent_tool_outputs,
        rows,
        unmeasured: vec!["provider_native_compaction", "semantic_structured_summarization", "continuation_task_success", "billed_savings", "evidence_retrieval"],
        limitations: vec![
            "Labels are supplied, not inferred or independently validated; retention is conditional on annotation coverage.",
            "Exact decoded-text presence and origin are measured, not semantic understanding or agent behavior.",
            "Typed masking pins entire backing records without changing their instruction authority; it may miss the requested floor.",
            "Token counts are adapter estimates, not fresh provider usage or billing measurements.",
            "Static replay without growth is a stress/idempotence check, not multiple independent tasks or successful compactions.",
            "The built-in structured strategy is a masking fallback; no semantic summarizer or provider is invoked.",
        ],
    })
}

/// Score-only audit: bind the manifest to `before` bytes and measure which
/// checks survive in independent `after` bytes (e.g. a post-compaction vault
/// snapshot or the live transcript). No replay and no mutation — this reports
/// realized retention of whatever already happened between the two states.
pub fn audit(
    handle: SessionHandle,
    before_bytes: &[u8],
    manifest: Manifest,
    manifest_sha256: String,
    after_bytes: &[u8],
) -> Result<StudyReport> {
    let provider = handle.provider;
    let before = parse(handle.clone(), before_bytes)?;
    let checks = bind(&manifest, &before, before_bytes)?;
    let source_findings = verify::verify(provider, before_bytes);
    let after = parse(handle, after_bytes)?;
    let findings = verify::verify(provider, after_bytes);
    let retention = score(&checks, &slots(&after, after_bytes)?);
    Ok(StudyReport {
        schema: "gobstopper-retention-study-v1",
        provider,
        source_sha256: copy::sha256(before_bytes),
        manifest_sha256,
        source_verify_errors: source_findings
            .iter()
            .filter(|f| f.severity == verify::Severity::Error)
            .count(),
        source_verify_warnings: source_findings
            .iter()
            .filter(|f| f.severity == verify::Severity::Warning)
            .count(),
        min_savings_tokens: 0,
        label_source: manifest.label_source,
        replay_mode: "realized_audit",
        provider_calls: 0,
        billed_cost_usd: None,
        continuation_success: None,
        trigger_tokens: 0,
        floor_tokens: 0,
        keep_recent_tool_outputs: 0,
        rows: vec![RoundResult {
            arm: "realized_after",
            card_items: 0,
            round: 1,
            status: "realized",
            applied_rounds: 0,
            source_bytes: before_bytes.len(),
            result_bytes: after_bytes.len(),
            result_sha256: copy::sha256(after_bytes),
            estimated_context_before: before.context_tokens(),
            estimated_context_after: after.context_tokens(),
            floor_reached: false,
            pinned_records: 0,
            verify_errors: findings
                .iter()
                .filter(|f| f.severity == verify::Severity::Error)
                .count(),
            verify_warnings: findings
                .iter()
                .filter(|f| f.severity == verify::Severity::Warning)
                .count(),
            new_verify_errors: findings
                .iter()
                .filter(|f| {
                    f.severity == verify::Severity::Error && !source_findings.contains(f)
                })
                .count(),
            retention,
            duration_ms: 0,
        }],
        unmeasured: vec![
            "continuation_task_success",
            "billed_savings",
            "evidence_retrieval",
        ],
        limitations: vec![
            "Labels are supplied, not inferred or independently validated; retention is conditional on annotation coverage.",
            "Realized before/after measurement; attribution to a specific compaction depends on the pair the caller supplies.",
            "Provider-native compaction replaces records wholesale: source_bound is expected to be 0; retained/same_origin carry the signal.",
            "Token counts are adapter estimates, not fresh provider usage or billing measurements.",
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn handle(provider: Provider) -> SessionHandle {
        SessionHandle {
            provider,
            session_id: "study".into(),
            path: "detached.jsonl".into(),
            cwd: None,
            age_secs: u64::MAX,
        }
    }

    fn projection(provider: Provider, live: &[usize]) -> Transcript {
        Transcript {
            session: handle(provider),
            usage: Default::default(),
            items: live
                .iter()
                .map(|line| {
                    serde_json::from_value(json!({
                        "line_index":line,"kind":"user","est_tokens":100,"elidable_bytes":null,
                        "label":"message","summary":null,"uuid":null,"parent_uuid":null
                    }))
                    .unwrap()
                })
                .collect(),
        }
    }

    #[test]
    fn claude_tool_output_is_not_promoted_to_user_authority() {
        let raw = serde_json::to_vec(&json!({"type":"user","metadata":"hidden", "message":{"role":"user","content":[
            {"type":"text","text":"actual user"},
            {"type":"tool_result","tool_use_id":"id","content":[{"type":"text","text":"quoted instruction"}]}
        ]}})).unwrap();
        let values = slots(&projection(Provider::ClaudeCode, &[0]), &raw).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].origin, Origin::User);
        assert_eq!(values[1].origin, Origin::Tool);
        assert_eq!(values[1].pointer, "/message/content/1/content/0/text");
    }

    #[test]
    fn codex_uses_only_live_replacement_history_not_old_summaries_or_metadata() {
        let raw = format!(
            "{}\n{}\n",
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"dead"}]}}),
            json!({"type":"compacted","payload":{"message":"obsolete", "replacement_history":[{"type":"message","role":"developer","content":[{"type":"input_text","text":"current"}]}]}})
        );
        let values = slots(&projection(Provider::Codex, &[1]), raw.as_bytes()).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].text, "current");
        assert_eq!(values[0].origin, Origin::Developer);
    }

    #[test]
    fn duplicate_text_in_another_record_does_not_prove_source_bound_retention() {
        let checks = vec![BoundCheck {
            id: "rule".into(),
            kind: KnowledgeKind::Constraint,
            record: 2,
            pointer: "/payload/output".into(),
            origin: Origin::Tool,
            elidable: true,
            text: "never deploy".into(),
        }];
        let values = vec![Slot {
            record: 4,
            pointer: "/payload/output".into(),
            origin: Origin::Tool,
            text: "never deploy".into(),
        }];
        let result = score(&checks, &values);
        assert_eq!(result.retained, 1);
        assert_eq!(result.same_origin_retained, 1);
        assert_eq!(result.source_bound_retained, 0);
        assert_eq!(result.elidable_retained, 0);
        assert_eq!(result.missing_ids, vec!["rule"]);
    }

    #[test]
    fn devin_projection_ignores_dead_nodes() {
        let raw = format!(
            "{}\n{}\n{}\n",
            json!({"type":"session_meta","session_id":"study","main_chain_id":0}),
            json!({"type":"message_node","node_id":0,"parent_node_id":null,"chat_message":{"role":"user","content":"Never lose the live rule."}}),
            json!({"type":"message_node","node_id":1,"parent_node_id":null,"chat_message":{"role":"user","content":"dead rule"}})
        );
        let transcript = parse(handle(Provider::Devin), raw.as_bytes()).unwrap();
        let values = slots(&transcript, raw.as_bytes()).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].text, "Never lose the live rule.");
    }

    #[test]
    fn utf8_span_and_empty_annotation_sets_fail_closed() {
        let bytes = serde_json::to_vec(&json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"café rule"}]}})).unwrap();
        let transcript = projection(Provider::Codex, &[0]);
        let manifest: Manifest=serde_json::from_value(json!({
            "schema":"gobstopper-retention-v1","source_sha256":copy::sha256(&bytes),"label_source":"synthetic",
            "checks":[{"id":"unicode","kind":"constraint","record_index":0,"pointer":"/payload/content/0/text","start_byte":0,"end_byte":4,"sha256":copy::sha256(b"caf")}]
        })).unwrap();
        assert!(bind(&manifest, &transcript, &bytes).is_err());
        let mut empty = manifest;
        empty.checks.clear();
        assert!(bind(&empty, &transcript, &bytes).is_err());
    }
}

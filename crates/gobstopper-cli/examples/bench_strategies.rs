//! Local strategy benchmark.
//!
//! Generates synthetic transcripts with varying tool-output history,
//! runs every built-in strategy against each in an isolated temp workdir,
//! and prints a CSV of actual byte reduction, projected token reduction,
//! elapsed time and structural integrity. This is an offline proxy
//! benchmark: it does not exercise provider APIs, cache economics, or
//! task completion.

use gobstopper_core::strategy::{builtin_strategies, PolicyConfig};
use gobstopper_core::Transcript;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

fn bench_root() -> PathBuf {
    let mut dir = std::env::temp_dir();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    dir.push(format!("gobstopper-bench-{now}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create bench workdir");
    dir
}

fn cleanup(root: &Path) {
    let _ = fs::remove_dir_all(root);
}

fn make_codex(tool_outputs: usize, output_size: usize) -> (Transcript, Vec<String>) {
    let mut lines = vec![json!({"type":"session_meta","payload":{"id":"bench"}}).to_string()];
    for i in 0..tool_outputs {
        let call_id = format!("call-{i:04}");
        lines.push(
            json!({"type":"response_item","payload":{"type":"function_call","call_id":call_id,"function":"f","arguments":{}}})
                .to_string(),
        );
        lines.push(
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":call_id,"output":"x".repeat(output_size)}})
                .to_string(),
        );
    }
    // Keep the transcript JSONL in memory and parse it back through the adapter.
    let raw = lines.join("\n") + "\n";
    let handle = gobstopper_core::SessionHandle {
        provider: gobstopper_core::Provider::Codex,
        session_id: "bench".into(),
        path: std::path::PathBuf::from("/tmp/bench.jsonl"),
        cwd: None,
        age_secs: 0,
    };
    let transcript = gobstopper_adapters::codex::load_bytes(handle, raw.as_bytes())
        .expect("valid bench transcript");
    (transcript, lines)
}

fn main() {
    let root = bench_root();
    let policy = PolicyConfig {
        trigger_tokens: 1,
        floor_tokens: 0,
        keep_recent_tool_outputs: 2,
        min_interval_secs: 0,
        ..Default::default()
    };
    let strategies = builtin_strategies();
    let sizes = [(50, 400), (200, 400), (500, 400), (1000, 400)];

    let mut out = std::io::stdout();
    writeln!(
        out,
        "tool_pairs,output_bytes,strategy,input_bytes,output_bytes,elapsed_us,projected_tokens_before,projected_tokens_after,verify_errors"
    )
    .unwrap();

    for (pairs, output_size) in sizes {
        let (transcript, lines) = make_codex(pairs, output_size);
        let input_bytes = (lines.join("\n") + "\n").len();
        for strat in &strategies {
            let start = Instant::now();
            let plan = strat.evaluate(&transcript, &policy);
            let elapsed = start.elapsed().as_micros();
            let before = transcript.context_tokens();
            let mut after = before;
            let mut output_bytes = input_bytes;
            let mut errors = 0usize;
            if let Some(plan) = plan {
                after = plan.context_tokens_after;
                let mut tmp = root.clone();
                tmp.push(format!("{}-{pairs}-{output_size}.jsonl", strat.id()));
                fs::write(&tmp, lines.join("\n") + "\n").ok();
                match gobstopper_adapters::codex::apply(&tmp, &plan.edits) {
                    Ok(_) => {
                        if let Ok(rendered) = fs::read_to_string(&tmp) {
                            output_bytes = rendered.len();
                        }
                    }
                    Err(_) => errors += 1,
                }
                let _ = fs::remove_file(&tmp);
            }
            writeln!(
                out,
                "{},{},{},{},{},{},{},{},{}",
                pairs,
                output_size,
                strat.id(),
                input_bytes,
                output_bytes,
                elapsed,
                before,
                after,
                errors
            )
            .unwrap();
        }
    }
    cleanup(&root);
}

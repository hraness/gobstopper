#!/usr/bin/env python3
"""Check assurance inventory structure and surface coverage, without claiming proof."""

import argparse
import json
from pathlib import Path
import re
import sys

FILES = {
    "ledger": "gobstopper.assurance.v1",
    "effects": "gobstopper.effects.v1",
    "claims": "gobstopper.claims.v1",
    "codeql-triage": "gobstopper.codeql-triage.v1",
}
STATUSES = {"specified", "source_reviewed", "specification", "bounded_check", "historical_observation"}
ALERT_IDS = {8, 12, 13, 14, 15, 26, 27, 30, 31, 33, 34, 35, 36, 39, 40, 41, 42, 62, 63, 68, 71}
BASELINE = "ffc71480564f0d0077f27e59a04df3174d5335ef"
WRITE_MODES = {"emit", "in_memory", "trusted_code", "spawn", "create", "create_delete", "replace", "create_replace", "permissions", "append", "delete", "sql_transaction", "native", "rename"}


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_documents(root):
    return {
        name: json.loads((root / "docs/assurance" / f"{name}.json").read_text(), object_pairs_hook=unique_object)
        for name in FILES
    }


def enum_variants(text, name):
    match = re.search(r"\benum " + re.escape(name) + r"\s*\{([\s\S]*?)\n\}", text)
    if not match:
        raise ValueError(f"missing source enum {name}")
    return set(re.findall(r"^    ([A-Z][A-Za-z0-9_]*)\s*(?:\{|,)", match.group(1), re.M))


def production_text(text):
    # Conservative declaration inventory, not a Rust parser/effect analyzer.
    return re.split(r"\n#\[cfg\(test\)\]\s*\nmod tests\b", text, maxsplit=1)[0]


def source_callables(root):
    result = set()
    for package in ("gobstopper-adapters", "gobstopper-cli", "gobstopper-core"):
        for path in (root / "crates" / package / "src").rglob("*.rs"):
            names = re.findall(
                r'\bpub(?:\([^)]*\))?\s+(?:(?:async|const|unsafe)\s+|extern(?:\s+"[^"]*")?\s+)*fn\s+([a-zA-Z_][a-zA-Z_0-9]*)',
                production_text(path.read_text()),
            )
            result.update((str(path.relative_to(root)), name) for name in names)
    return result


def validate(root, documents):
    """Return all structural errors; no test, network or provider execution."""
    errors = []

    def require(condition, message):
        if not condition:
            errors.append(message)

    def text(value):
        return isinstance(value, str) and bool(value.strip())

    def nonempty(row, fields, label):
        for field in fields:
            value = row.get(field)
            require((text(value) or isinstance(value, list) and bool(value)), f"{label}: missing {field}")

    def index(rows, key, label):
        result = {}
        require(isinstance(rows, list), f"{label}: expected list")
        for row in rows if isinstance(rows, list) else []:
            require(isinstance(row, dict), f"{label}: expected object")
            if not isinstance(row, dict):
                continue
            value = row.get(key)
            require(text(value) or isinstance(value, int), f"{label}: missing {key}")
            require(value not in result, f"{label}: duplicate {value}")
            result[value] = row
        return result

    def refs(values, allowed, label):
        require(isinstance(values, list) and bool(values), f"{label}: empty references")
        for value in values if isinstance(values, list) else []:
            require(value in allowed, f"{label}: unknown reference {value}")

    def source_ref(ref, label):
        if "url" in ref:
            require(isinstance(ref["url"], str) and ref["url"].startswith("https://"), f"{label}: invalid evidence URL")
            return
        path = ref.get("path", "")
        require(text(path) and not Path(path).is_absolute() and ".." not in Path(path).parts, f"{label}: invalid source path")
        if not text(path) or Path(path).is_absolute() or ".." in Path(path).parts:
            return
        candidate = root / path
        require(candidate.is_file(), f"{label}: missing source {path}")
        if candidate.is_file() and "anchor" in ref:
            require(text(ref["anchor"]) and ref["anchor"] in candidate.read_text(), f"{label}: absent anchor in {path}: {ref['anchor']}")

    def next_gate(row, label):
        require(re.fullmatch(r"C(?:[1-9]|1[0-5])", str(row.get("next_gate", ""))) is not None, f"{label}: invalid next gate")

    for name, schema in FILES.items():
        document = documents.get(name, {})
        require(document.get("schema") == schema, f"{name}: unsupported schema")
        require(document.get("baseline_revision") == BASELINE, f"{name}: baseline identity changed without validator review")
    ledger = documents.get("ledger", {})
    states = index(ledger.get("states", []), "id", "states")
    assumptions = index(ledger.get("assumptions", []), "id", "assumptions")
    evidence = index(ledger.get("evidence", []), "id", "evidence")
    invariants = index(ledger.get("invariants", []), "id", "invariants")
    for name, row in states.items():
        nonempty(row, ["meaning", "owner", "boundary"], f"state {name}")
    for name, row in assumptions.items():
        nonempty(row, ["statement"], f"assumption {name}")
        next_gate(row, f"assumption {name}")
    for name, row in evidence.items():
        nonempty(row, ["kind", "revision", "scope", "bounds", "references", "exclusions"], f"evidence {name}")
        for ref in row.get("references", []):
            source_ref(ref, f"evidence {name}")
        if row.get("kind") == "bounded_model_receipt":
            require(re.fullmatch(r"[0-9a-f]{40}", row.get("revision", "")) is not None, f"evidence {name}: model receipt needs exact revision")
            require(re.fullmatch(r"[0-9a-f]{64}", row.get("tool", {}).get("sha256", "")) is not None, f"evidence {name}: missing tool pin")
            require(bool(row.get("inputs")), f"evidence {name}: missing model inputs")
            for item in row.get("inputs", []):
                source_ref(item, f"evidence {name}")
                require(re.fullmatch(r"[0-9a-f]{64}", item.get("sha256", "")) is not None, f"evidence {name}: missing historical input digest")

    def obligation(row, label):
        nonempty(row, ["owner", "bounds", "exclusions", "evidence", "assumptions"], label)
        require(row.get("status") in STATUSES, f"{label}: unsupported assurance status {row.get('status')!r}")
        refs(row.get("evidence"), evidence, f"{label} evidence")
        refs(row.get("assumptions"), assumptions, f"{label} assumptions")
        next_gate(row, label)

    for name, row in invariants.items():
        obligation(row, f"invariant {name}")
        nonempty(row, ["predicate", "negative_control", "review_trigger"], f"invariant {name}")
        require(row.get("status") == "specified", f"invariant {name}: universal obligation cannot inherit a bounded result")
    plan = (root / "docs/correctness-plan.md").read_text()
    expected_invariants = set(re.findall(r"^\| ([A-Z]+-\d+) \|", plan, re.M))
    require(set(invariants) == expected_invariants and bool(expected_invariants), "invariant inventory differs from correctness plan")
    coverage = index(ledger.get("coverage", []), "name", "coverage")
    audit = (root / "docs/correctness-audit.md").read_text().split("## Coverage map", 1)[1].split("\n## ", 1)[0]
    expected_coverage = {line.split("|")[1].strip() for line in audit.splitlines() if line.startswith("| ") and not line.startswith("| Capability")}
    require(set(coverage) == expected_coverage, "coverage inventory differs from audit coverage map")
    for name, row in coverage.items():
        obligation(row, f"coverage {name}")
        refs(row.get("invariants"), invariants, f"coverage {name} invariants")
        nonempty(row, ["source"], f"coverage {name}")
        for ref in row.get("source", []):
            source_ref(ref, f"coverage {name}")
    for row in ledger.get("compatibility", []):
        nonempty(row, ["id", "version", "source", "read_write_behavior", "upgrade_downgrade"], "compatibility")
        next_gate(row, "compatibility")
        for ref in row.get("source", []):
            source_ref(ref, "compatibility")
    require(len(ledger.get("compatibility", [])) >= 10, "missing schema compatibility boundaries")
    for row in ledger.get("activation_gates", []):
        nonempty(row, ["id", "owner", "requires", "permits", "does_not_permit"], "activation gate")
        next_gate(row, "activation gate")
    require({r.get("id") for r in ledger.get("activation_gates", [])} == {"artifact", "native", "direct-write", "provider-study"}, "missing activation boundaries")

    effects = documents.get("effects", {})
    profiles = index(effects.get("profiles", []), "id", "profiles")
    for name, row in profiles.items():
        obligation(row, f"effect {name}")
        nonempty(row, ["authority", "source"], f"effect {name}")
        refs(row.get("invariants"), invariants, f"effect {name} invariants")
        for state in row.get("reads", []):
            require(state in states, f"effect {name}: unknown read state {state}")
        require(isinstance(row.get("writes"), list), f"effect {name}: writes must be explicit")
        require(isinstance(row.get("outbound"), list), f"effect {name}: outbound must be explicit")
        for write in row.get("writes", []):
            require(write.get("state") in states, f"effect {name}: unknown write state")
            require(write.get("mode") in WRITE_MODES, f"effect {name}: unknown write mode")
            nonempty(write, ["condition"], f"effect {name} write")
        for ref in row.get("source", []):
            source_ref(ref, f"effect {name}")

    cli = index(effects.get("cli", []), "name", "CLI")
    mcp = index(effects.get("mcp", []), "name", "MCP")
    hooks = index(effects.get("hooks", []), "name", "hooks")
    require(set(cli) == enum_variants((root / "crates/gobstopper-cli/src/main.rs").read_text(), "Cmd"), "CLI inventory differs from command enum")
    require(set(hooks) == enum_variants((root / "crates/gobstopper-cli/src/hooks.rs").read_text(), "HookTarget"), "hook inventory differs from HookTarget")
    mcp_text = (root / "crates/gobstopper-cli/src/mcp.rs").read_text().split("fn tools(", 1)[1].split("\nfn ", 1)[0]
    require(set(mcp) == set(re.findall(r'"name": "([a-z_]+)"', mcp_text)), "MCP inventory differs from advertised tools")
    for label, surfaces in (("CLI", cli), ("MCP", mcp), ("hook", hooks)):
        for name, row in surfaces.items():
            refs(row.get("effects"), profiles, f"{label} {name}")
            source_ref(row.get("source", {}), f"{label} {name}")
            if label == "CLI":
                nonempty(row, ["contract"], f"CLI {name}")
            if label == "MCP":
                require(row.get("content_opt_in") is (name in {"read_snapshot", "search_snapshot"}), f"MCP {name}: content gate mismatch")
    scripts = index(effects.get("scripts", []), "path", "scripts")
    expected_scripts = {str(p.relative_to(root)) for p in (root / "scripts").rglob("*.py") if not p.name.startswith("test_")}
    expected_scripts.update(str(p.relative_to(root)) for p in (root / "verify").rglob("check.py"))
    require(set(scripts) == expected_scripts, "script effect inventory differs from production Python entry points")
    for path, row in scripts.items():
        source_ref({"path": path}, "script")
        refs(row.get("effects"), profiles, f"script {path}")
    callables = effects.get("library_callables", [])
    actual_callables = {(r.get("path"), r.get("symbol")) for r in callables}
    require(len(actual_callables) == len(callables), "duplicate callable effect row")
    require(actual_callables == source_callables(root), "Rust public callable inventory drift; classify added/removed declarations")
    for row in callables:
        refs(row.get("effects"), profiles, f"callable {row.get('path')}::{row.get('symbol')}")

    claims = index(documents.get("claims", {}).get("claims", []), "id", "claims")
    require(bool(claims), "missing claim registry")
    for name, row in claims.items():
        obligation(row, f"claim {name}")
        nonempty(row, ["assertion", "public_sources", "review_trigger"], f"claim {name}")
        require(not re.search(r"\b(?:provably correct|fully proven|universally safe|zero bugs)\b", str(row.get("assertion", "")), re.I), f"claim {name}: unsupported universal assertion")
        if row.get("status") == "bounded_check":
            require(any(evidence.get(e, {}).get("kind") == "bounded_model_receipt" for e in row.get("evidence", [])), f"claim {name}: bounded check lacks a pinned receipt")
        if row.get("status") == "historical_observation":
            require(any(evidence.get(e, {}).get("kind") in {"historical_receipt", "scanner_inventory"} for e in row.get("evidence", [])), f"claim {name}: historical observation lacks evidence")
        for ref in row.get("public_sources", []):
            source_ref(ref, f"claim {name}")

    triage = documents.get("codeql-triage", {})
    alerts = index(triage.get("alerts", []), "number", "alerts")
    require(set(alerts) == ALERT_IDS, "baseline alert inventory must preserve all 21 exact alert IDs")
    require(triage.get("snapshot", {}).get("count") == 21, "baseline alert count mismatch")
    require(re.fullmatch(r"[0-9a-f]{64}", triage.get("snapshot", {}).get("sha256", "")) is not None, "missing baseline scanner snapshot digest")
    classes = {"requested_identity", "synthetic_test_only", "credential_fragment", "background_identity"}
    counts = {kind: 0 for kind in classes}
    for number, row in alerts.items():
        require(row.get("baseline_state") == "open" and row.get("baseline_revision") == BASELINE, f"alert {number}: cannot rewrite baseline scanner state")
        require(row.get("scanner_action") == "none", f"alert {number}: triage must not dismiss or suppress")
        require(row.get("rule") == "rust/cleartext-logging", f"alert {number}: baseline rule changed")
        require(row.get("classification") in classes, f"alert {number}: missing individual disposition")
        if row.get("classification") in classes:
            counts[row["classification"]] += 1
        nonempty(row, ["authorized_output_contract", "decision", "resolution", "url"], f"alert {number}")
        require(row.get("resolution") in {"reviewed_retained_output", "source_repaired_pending_exact_head_scan"}, f"alert {number}: unsupported closure claim")
        nonempty(row.get("source", {}), ["path", "origin"], f"alert {number} source")
        nonempty(row.get("sink", {}), ["channel", "operation", "baseline_code"], f"alert {number} sink")
        nonempty(row.get("remediation", {}), ["owner", "source_revision", "evidence", "validation_gate", "remaining_risk"], f"alert {number} remediation")
        refs(row.get("remediation", {}).get("evidence"), evidence, f"alert {number} evidence")
        next_gate(row, f"alert {number}")
    require(counts == triage.get("counts"), "alert disposition counts mismatch")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        docs = load_documents(args.root)
        errors = validate(args.root, docs)
    except (OSError, ValueError, KeyError, IndexError, TypeError, AttributeError) as error:
        errors = [f"invalid inventory/source structure: {error}"]
    if errors:
        for error in errors:
            print(f"assurance: {error}", file=sys.stderr)
        return 1
    print("assurance inventory: passed (structure and surface coverage only; not a correctness proof)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

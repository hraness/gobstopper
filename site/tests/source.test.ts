import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { parsePublishedRelease } from "../app/publication";

const site = join(import.meta.dir, "..");
const read = async (path: string): Promise<string> => await readFile(join(site, path), "utf8");

function record(value: unknown, label: string): Readonly<Record<string, unknown>> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object.`);
  }
  return value as Readonly<Record<string, unknown>>;
}

function stableVersion(value: unknown, label: string): readonly [bigint, bigint, bigint] {
  if (typeof value !== "string") throw new TypeError(`${label} must be a string.`);
  const match = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/u.exec(value);
  if (match === null) throw new TypeError(`${label} must be a stable version.`);
  return [BigInt(match[1]!), BigInt(match[2]!), BigInt(match[3]!)];
}

/** The repository is a Cargo workspace; its version lives in [workspace.package]. */
function workspaceVersion(manifest: string): readonly [bigint, bigint, bigint] {
  const version = /^\[workspace\.package\][\s\S]*?^version = "([^"]+)"/mu.exec(manifest)?.[1];
  return stableVersion(version, "workspace version");
}

function compare(left: readonly [bigint, bigint, bigint], right: readonly [bigint, bigint, bigint]): number {
  for (let index = 0; index < 3; index += 1) {
    if (left[index] !== right[index]) return left[index]! > right[index]! ? 1 : -1;
  }
  return 0;
}

describe("Gobstopper site source contract", () => {
  test("advertises only a verified published release that does not exceed the source version", async () => {
    const [home, publication, manifest] = await Promise.all([
      read("app/page.tsx"),
      read("published-release.json"),
      readFile(join(site, "..", "Cargo.toml"), "utf8"),
    ]);
    const publishedRelease = record(JSON.parse(publication) as unknown, "published release");
    expect(Object.keys(publishedRelease).sort()).toEqual(["verificationRun", "version"]);
    const admitted = parsePublishedRelease(publishedRelease);
    if (admitted === null) {
      expect(home).toContain("No release yet");
      return;
    }
    const published = stableVersion(admitted.version, "published version");
    const source = workspaceVersion(manifest);
    expect(compare(published, source)).toBeLessThanOrEqual(0);
    expect(publishedRelease.verificationRun).toMatch(/^https:\/\/github\.com\/hraness\/gobstopper\/actions\/runs\/[1-9][0-9]*$/u);
    expect(home).toContain('import { publishedRelease } from "./publication"');
    expect(home).toContain("const releaseVersion = publishedRelease?.version;");
    expect(home).not.toContain("package.json");
    expect(home).toContain("href={publishedRelease.verificationRun}");
  });

  test("renders the README landing identity and the shared Ask AI links", async () => {
    const [packageJson, chrome, generated] = await Promise.all([
      read("package.json"),
      read("app/_components/site-chrome.tsx"),
      read("app/readme.generated.ts"),
    ]);
    expect(packageJson).toContain('"@hraness/ui": "github:hraness/ui#v0.5.16"');
    expect(packageJson).toContain('"@hraness/design-kit": "github:hraness/design-kit#v0.15.0"');
    expect(chrome).toContain('import { AskAiAboutThis } from "@hraness/ui"');
    expect(chrome).toContain('url={absoluteUrl(path)}');
    expect(generated).toContain('export const readmeTitle = "gobstopper";');
    expect(generated).toContain("export const readmeHtml = ");
  });

  test("uses the shared design-kit fonts and marketing grammar", async () => {
    const globals = await read("app/globals.css");
    expect(globals).toContain('@import "@hraness/design-kit/fonts.css"');
    expect(globals).toContain('@import "@hraness/design-kit/product-marketing.css"');
    expect(globals).toContain('@import "../vendor/paper-theme/paper-theme.css"');
    expect(globals).not.toMatch(/Georgia|Times New Roman/u);
  });

  test("keeps the sitemap and robots on the canonical origin", async () => {
    const [sitemap, robots] = await Promise.all([read("public/sitemap.xml"), read("public/robots.txt")]);
    expect(sitemap).toContain("<loc>https://gobstopper.sh/</loc>");
    expect(sitemap).toContain("<loc>https://gobstopper.sh/docs</loc>");
    expect(sitemap).toContain("<loc>https://gobstopper.sh/methodology</loc>");
    expect(sitemap).toContain("<loc>https://gobstopper.sh/benchmarks</loc>");
    expect(robots).toContain("Sitemap: https://gobstopper.sh/sitemap.xml");
  });

  test("keeps social previews and the agent map on the canonical origin", async () => {
    const [layout, docs, llms, homeCard, docsCard] = await Promise.all([
      read("app/layout.tsx"),
      read("app/docs/page.tsx"),
      read("public/llms.txt"),
      read("app/opengraph-image/route.ts"),
      read("app/docs/opengraph-image/route.ts"),
    ]);
    for (const page of [layout, docs]) {
      expect(page).toContain('card: "summary_large_image"');
      expect(page).toContain("opengraph-image");
    }
    expect(layout).toContain('url: "/opengraph-image"');
    expect(docs).toContain('url: "/docs/opengraph-image"');
    for (const card of [homeCard, docsCard]) {
      expect(card).toContain("createSocialImage");
      expect(card).toContain('"force-static"');
    }
    expect(llms).toContain("https://gobstopper.sh/");
    expect(llms).toContain("https://gobstopper.sh/docs");
    expect(llms).not.toContain("http://");
  });
});


test("publication metadata fails closed on malformed or partially verified releases", () => {
  expect(parsePublishedRelease({ version: null, verificationRun: null })).toBeNull();
  const verificationRun = "https://github.com/hraness/gobstopper/actions/runs/123";
  expect(parsePublishedRelease({ version: "0.20.0", verificationRun })).toEqual({ version: "0.20.0", verificationRun });
  for (const value of [null, {}, { version: "0.20.0", verificationRun: null }, { version: "0.20.0", verificationRun: "https://example.com" }, { version: "9007199254740992.0.0", verificationRun }, { version: "0.20.0", verificationRun, extra: true }]) {
    expect(() => parsePublishedRelease(value)).toThrow();
  }
});


test("loads the immutable material after Paper and editorial styling", async () => {
  const [css, layout, checker] = await Promise.all([read("app/globals.css"), read("app/layout.tsx"), read("scripts/check-paper-theme.mjs")]);
  const materialImport = '@import "../vendor/hraness-lantern/lantern-material.css";';
  expect(css).toContain(materialImport);
  expect(css.indexOf(materialImport)).toBeGreaterThan(css.indexOf('product-marketing-preset.css";'));
  expect(layout).toContain('data-hraness-material="lantern"');
  expect(checker).toContain('import { checkLanternMaterialSnapshot } from "../vendor/hraness-lantern/check.mjs"');
  expect(checker).toContain("await checkLanternMaterialSnapshot();");
});

test("registers the footer layer after UI layers in one stylesheet", async () => {
  const [css, layout] = await Promise.all([read("app/globals.css"), read("app/layout.tsx")]);
  const footer = '@import "@hraness/site-footer/styles.css";';
  expect(css).toContain(footer);
  expect(css.indexOf(footer)).toBeGreaterThan(css.indexOf('@import "@hraness/ui/styles.css";'));
  expect(css.indexOf(footer)).toBeGreaterThan(css.indexOf('lantern-material.css";'));
  expect(layout).not.toContain('import "@hraness/site-footer/styles.css"');
});


describe("published September 19 compaction study", () => {
  const directory = "public/benchmarks/2026-09-19";

  test("publishes aggregate-only data and keeps no-op cases in the totals", async () => {
    const raw = await read(`${directory}/aggregates.json`);
    const study = record(JSON.parse(raw) as unknown, "study");
    expect(Object.keys(study).sort()).toEqual([
      "binary_sha256", "cohorts", "combined", "limitations", "measurement",
      "privacy", "schema", "source_commit", "study_date",
    ]);
    const allowedFields = new Set([
      "provider", "cohort", "strategy", "stratum", "selected_samples",
      "successful_evals", "plans", "no_plan", "failures", "primary_reduction_samples",
      "median_reduction_including_no_plan_percent", "median_reduction_planned_only_percent",
      "min_reduction_including_no_plan_percent", "max_reduction_including_no_plan_percent",
      "median_descriptive_resampling_95pct_interval", "sum_context_before",
      "sum_projected_reclaimed", "baseline_unavailable_samples", "baseline_error_samples",
      "planned_verification_delta_available", "planned_verification_delta_unavailable",
      "post_error_samples", "new_error_count", "new_warning_count", "nonzero_probe_samples",
      "zero_probe_samples", "probe_score_unavailable_plans", "literal_probes_total",
      "literal_probes_retained", "nonzero_tail_probe_samples", "tail_probes_total",
      "tail_probes_retained", "median_wall_ms",
    ]);
    const enums: Readonly<Record<string, readonly string[]>> = {
      provider: ["codex", "claude_code"],
      cohort: ["high_context", "below_trigger_control"],
      strategy: ["elide", "scored", "cache_aware", "compacted", "dedupe"],
      stratum: ["all_selected", "native_compacted", "no_native_compaction", "no_observed_parent"],
    };
    expect(Array.isArray(study.cohorts)).toBe(true);
    let selected = 0;
    let plans = 0;
    let noPlan = 0;
    let failures = 0;
    for (const value of study.cohorts as unknown[]) {
      const cohort = record(value, "cohort");
      const allowedCohortFields = new Set([
        "name", "selected_sessions", "codex_subagent_sessions", "claude_sessions",
        "high_context_sessions", "below_trigger_controls", "wall_seconds", "aggregates",
      ]);
      expect(Object.keys(cohort).every((key) => allowedCohortFields.has(key))).toBe(true);
      expect(Array.isArray(cohort.aggregates)).toBe(true);
      for (const entry of cohort.aggregates as unknown[]) {
        const row = record(entry, "aggregate");
        expect(Object.keys(row).every((key) => allowedFields.has(key))).toBe(true);
        for (const [key, field] of Object.entries(row)) {
          if (key in enums) expect(enums[key]).toContain(field as string);
          else if (Array.isArray(field)) {
            expect(key).toBe("median_descriptive_resampling_95pct_interval");
            expect(field).toHaveLength(2);
            expect(field.every((item) => typeof item === "number" && Number.isFinite(item))).toBe(true);
          } else expect(field === null || (typeof field === "number" && Number.isFinite(field))).toBe(true);
        }
        if (row.strategy === "compacted" && row.stratum === "all_selected") {
          selected += row.selected_samples as number;
          plans += row.plans as number;
          noPlan += row.no_plan as number;
          failures += row.failures as number;
        }
      }
    }
    const combined = record(study.combined, "combined totals");
    expect(selected).toBe(729);
    expect(plans).toBe(92);
    expect(noPlan).toBe(637);
    expect(failures).toBe(0);
    expect(selected).toBe(plans + noPlan + failures);
    expect(combined.selected_sessions).toBe(selected);
    expect(combined.compacted_plans).toBe(plans);
    expect(combined.compacted_no_plan).toBe(noPlan);
    expect(combined.compacted_median_reduction_including_no_plan_percent).toBe(0);
  });

  test("public downloads exclude private locations and per-session identities", async () => {
    const files = await Promise.all([
      read(`${directory}/aggregates.json`),
      read(`${directory}/protocol.json`),
      read(`${directory}/report.md`),
      read(`${directory}/retention-ablation.json`),
      read(`${directory}/retention-ablation-protocol.json`),
      read(`${directory}/apple-retention-pilot.json`),
      read(`${directory}/apple-retention-protocol.json`),
    ]);
    for (const text of files) {
      expect(text).not.toMatch(/\/Users\/|\/home\/|session-\d{4}|rollout-/u);
      expect(text).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/u);
      expect(text).not.toMatch(/"(?:samples|rows|sample|session_id|path|snapshot|sha256|manifest|manifest_sha256|missed_probes)"\s*:/u);
    }
  });

  test("paired policy evidence keeps all variants, controls and derived retention visible", async () => {
    const study = record(JSON.parse(await read(`${directory}/retention-ablation.json`)) as unknown, "policy study");
    expect(Object.keys(study).sort()).toEqual([
      "aggregates", "completed_at", "limitations", "measurement", "provenance",
      "schema", "study_date", "summary",
    ]);
    const allowedFields = new Set([
      "cohort", "variant", "selected", "successful", "failures", "plans", "no_plan",
      "median_reduction_percent_including_noops", "sum_context_before", "sum_projected_reclaimed",
      "literal_samples", "literal_total", "literal_retained", "literal_unchanged_source_derived_samples",
      "new_errors", "new_warnings", "disabled_equivalence_failures", "median_wall_ms",
      "paired_retention_improved_samples", "paired_retention_decreased_samples",
      "paired_retention_equal_samples", "paired_reduction_increased_samples",
    ]);
    const variants = ["baseline_disabled", "candidate_disabled", "cutoff_0.35", "cutoff_0.5", "cutoff_0.65"];
    expect(Array.isArray(study.aggregates)).toBe(true);
    const rows = (study.aggregates as unknown[]).map((value) => record(value, "policy aggregate"));
    expect(rows).toHaveLength(10);
    for (const row of rows) {
      expect(Object.keys(row).every((key) => allowedFields.has(key))).toBe(true);
      expect(["high_context", "below_trigger_control"]).toContain(row.cohort as string);
      expect(variants).toContain(row.variant as string);
      for (const [key, field] of Object.entries(row)) {
        if (key !== "cohort" && key !== "variant") {
          expect(typeof field === "number" && Number.isFinite(field)).toBe(true);
        }
      }
      expect(row.selected).toBe((row.plans as number) + (row.no_plan as number) + (row.failures as number));
      expect(row.failures).toBe(0);
      expect(row.new_errors).toBe(0);
      expect(row.new_warnings).toBe(0);
      expect(row.disabled_equivalence_failures).toBe(0);
      if (row.cohort === "below_trigger_control") {
        expect(row.selected).toBe(41);
        expect(row.no_plan).toBe(41);
        expect(row.literal_total).toBe(0);
      } else {
        expect(row.selected).toBe(73);
        expect(row.literal_samples).toBe(73);
        expect(row.literal_total).toBe(4667);
        expect(row.literal_unchanged_source_derived_samples).toBe(row.no_plan);
      }
    }
    for (const cohort of ["high_context", "below_trigger_control"]) {
      expect(rows.filter((row) => row.cohort === cohort).map((row) => row.variant).sort()).toEqual([...variants].sort());
    }
    const provenance = record(study.provenance, "policy provenance");
    expect(Object.keys(provenance).sort()).toEqual([
      "baseline_binary_sha256", "candidate_base_commit", "candidate_binary_sha256",
      "candidate_source_state", "local_model_calls", "remote_model_calls", "selected_samples", "started_at",
    ]);
    expect(provenance.local_model_calls).toBe(0);
    expect(provenance.remote_model_calls).toBe(0);
    const summary = record(study.summary, "policy totals");
    expect(summary.total_evaluations).toBe(rows.reduce((sum, row) => sum + (row.selected as number), 0));
    expect(summary.failed_evaluations).toBe(0);
    expect(summary.disabled_equivalence).toBe(true);
  });

  test("Apple receipt exposes aggregates without per-input data and labels no-op retention", async () => {
    const study = record(JSON.parse(await read(`${directory}/apple-retention-pilot.json`)) as unknown, "Apple pilot");
    expect(Object.keys(study).sort()).toEqual([
      "aggregates", "completed_at", "coverage", "interpretation", "limitations", "paired",
      "privacy", "provenance", "retention_accounting", "schema", "scope", "started_at", "study_date", "summary",
    ]);
    const provenance = record(study.provenance, "Apple provenance");
    expect(Object.keys(provenance).sort()).toEqual([
      "binary_and_bridge_unchanged", "binary_sha256", "bridge_sha256", "inputs_unchanged",
    ]);
    const allowedFields = new Set([
      "variant", "selected_inputs", "successful_evaluations", "failed_evaluations", "plans", "no_plan",
      "sum_context_before", "sum_projected_reclaimed", "median_projected_reduction_percent_including_noops",
      "min_projected_reduction_percent", "max_projected_reduction_percent", "literal_samples", "literal_total",
      "literal_retained", "literal_unchanged_source_derived_samples", "tail_samples_with_probes", "tail_total",
      "tail_retained", "new_errors", "new_warnings", "selected_outputs", "score_protected_outputs_reported_for_plans",
      "median_wall_ms", "sum_wall_ms", "sum_apply_ms", "apple_summary_available_samples",
      "apple_overlay_successful_samples", "fallback_diagnostic_samples", "apple_totals",
    ]);
    expect(Array.isArray(study.aggregates)).toBe(true);
    const rows = (study.aggregates as unknown[]).map((value) => record(value, "Apple aggregate"));
    expect(rows.map((row) => row.variant).sort()).toEqual(["apple", "heuristic"]);
    for (const row of rows) {
      expect(Object.keys(row).every((key) => allowedFields.has(key))).toBe(true);
      for (const [key, field] of Object.entries(row)) {
        if (key !== "variant" && key !== "apple_totals") {
          expect(typeof field === "number" && Number.isFinite(field)).toBe(true);
        }
      }
      expect(row.selected_inputs).toBe(3);
      expect((row.plans as number) + (row.no_plan as number) + (row.failed_evaluations as number)).toBe(3);
      expect(row.literal_total).toBe(192);
      expect(row.literal_unchanged_source_derived_samples).toBe(row.no_plan);
      if (row.variant === "apple") {
        expect(row.no_plan).toBe(2);
        const totals = record(row.apple_totals, "Apple totals");
        expect(Object.keys(totals).sort()).toEqual([
          "cached_batches", "failed_batches", "items_overlaid", "model_calls", "scoring_duration_ms", "selected_candidates", "unique_lines",
        ]);
        expect(Object.values(totals).every((value) => typeof value === "number" && Number.isFinite(value))).toBe(true);
        expect(totals.items_overlaid).toBe(totals.selected_candidates);
        expect(totals.model_calls).toBe(12);
        expect(totals.failed_batches).toBe(0);
      }
    }
    const summary = record(study.summary, "Apple summary");
    expect(summary.total_evaluations).toBe(6);
    expect(summary.failed_evaluations).toBe(0);
    expect(summary.remote_model_calls).toBe(0);
    expect(study.retention_accounting).toContain("128 of 192 retained probes are derived");
  });
});

describe("published September 20 synthetic recovery study", () => {
  const directory = "public/benchmarks/2026-09-20";

  test("keeps API check and state-card denominators distinct", async () => {
    const study = record(JSON.parse(await read(`${directory}/recovery-study-results.json`)) as unknown, "recovery study");
    const protocol = record(JSON.parse(await read(`${directory}/recovery-study-protocol.json`)) as unknown, "recovery protocol");
    expect(Object.keys(study).sort()).toEqual([
      "baseline_recovery_capabilities", "bounds", "checks", "immutability", "interpretation", "limitations",
      "privacy", "provenance", "recall", "schema", "scope", "started_at", "study_date", "summary", "timing",
    ]);
    expect(Object.keys(protocol).sort()).toEqual([
      "additional_robustness_snapshots", "bounds", "candidate_rule", "cross_chunk_fact_samples", "escaped_unicode_samples",
      "evaluation", "families_per_provider", "fixture_seed", "limitations", "malformed_record_accounting", "metrics",
      "privacy", "provider_counts", "public_reproduction", "read_contract", "registered_before_outcomes", "sample_count",
      "schema", "search_contract", "selection", "separate_from_private_session_studies", "unsupported_baseline_rule", "versions_per_family",
    ]);
    expect(study.schema).toBe("gobstopper-public-recovery-study-result-v1");
    expect(protocol.schema).toBe("gobstopper-public-recovery-study-protocol-v1");
    const summary = record(study.summary, "recovery totals");
    expect(Object.keys(summary).sort()).toEqual([
      "additional_robustness_snapshots", "baseline_state_card_queries_found", "bounded_search_checks",
      "candidate_state_card_queries_found", "commands", "exact_record_recoveries", "exact_record_recovery_targets",
      "failed_checks", "family_count", "local_model_calls", "negative_query_checks", "passed_checks",
      "positive_search_targets", "positive_search_targets_found", "provider_count", "remote_model_calls",
      "sample_count", "state_card_query_denominator_per_backend", "total_checks", "wall_seconds",
    ]);
    expect(Object.values(summary).every((value) => typeof value === "number" && Number.isFinite(value))).toBe(true);
    expect(summary.sample_count).toBe(protocol.sample_count);
    expect(summary.additional_robustness_snapshots).toBe(1);
    expect(protocol.additional_robustness_snapshots).toBe(1);
    const providers = record(protocol.provider_counts, "provider counts");
    expect(providers).toEqual({ codex: 18, claude_code: 18 });
    expect(summary.sample_count).toBe(Object.values(providers).reduce<number>((sum, value) => sum + (value as number), 0));
    expect(summary.family_count).toBe((protocol.families_per_provider as number) * (summary.provider_count as number));
    expect(summary.sample_count).toBe((summary.family_count as number) * (protocol.versions_per_family as number));
    expect(protocol.registered_before_outcomes).toBe(true);
    expect(protocol.separate_from_private_session_studies).toBe(true);
    expect(summary.local_model_calls).toBe(0);
    expect(summary.remote_model_calls).toBe(0);
    expect(Array.isArray(study.checks)).toBe(true);
    const checks = (study.checks as unknown[]).map((value) => record(value, "recovery check"));
    for (const check of checks) {
      expect(Object.keys(check).sort()).toEqual(["check", "failed", "passed", "total"]);
      expect(check.check).toMatch(/^(?:baseline_v3_read_compatibility|exact_read_(?:fact|long|unicode)|integrity_fail_closed|invalid_input_[0-8]|limited_search|malformed_physical_records|mcp_default_denial_and_opt_in|negative_search_(?:absent|case_sensitive|keys_not_values|other_version)|positive_search_(?:fact|long|unicode)|recall_(?:current|error|goal)|unicode_boundary_pagination)$/u);
      for (const key of ["total", "passed", "failed"]) expect(Number.isInteger(check[key]) && (check[key] as number) >= 0).toBe(true);
      expect(check.total).toBe((check.passed as number) + (check.failed as number));
    }
    expect(new Set(checks.map((check) => check.check)).size).toBe(checks.length);
    const sumChecks = (prefix: string, key: string): number => checks
      .filter((check) => (check.check as string).startsWith(prefix))
      .reduce((sum, check) => sum + (check[key] as number), 0);
    expect(summary.total_checks).toBe(sumChecks("", "total"));
    expect(summary.passed_checks).toBe(sumChecks("", "passed"));
    expect(summary.failed_checks).toBe(sumChecks("", "failed"));
    expect(summary.exact_record_recovery_targets).toBe(sumChecks("exact_read_", "total"));
    expect(summary.exact_record_recoveries).toBe(sumChecks("exact_read_", "passed"));
    expect(summary.positive_search_targets).toBe(sumChecks("positive_search_", "total"));
    expect(summary.positive_search_targets_found).toBe(sumChecks("positive_search_", "passed"));
    expect(summary.negative_query_checks).toBe(sumChecks("negative_search_", "total"));
    expect(summary.bounded_search_checks).toBe(sumChecks("limited_search", "total"));
    expect(Array.isArray(study.recall)).toBe(true);
    const recall = (study.recall as unknown[]).map((value) => record(value, "state-card aggregate"));
    for (const row of recall) {
      expect(Object.keys(row).sort()).toEqual(["absent", "backend", "field", "found", "provider", "queries"]);
      expect(["baseline", "candidate"]).toContain(row.backend as string);
      expect(["codex", "claude_code"]).toContain(row.provider as string);
      expect(["goal", "error", "current"]).toContain(row.field as string);
      expect(row.queries).toBe((row.found as number) + (row.absent as number));
    }
    expect(new Set(recall.map((row) => `${row.backend}/${row.provider}/${row.field}`)).size).toBe(12);
    expect(recall).toHaveLength(12);
    for (const backend of ["baseline", "candidate"]) {
      const rows = recall.filter((row) => row.backend === backend);
      expect(rows.reduce((sum, row) => sum + (row.queries as number), 0)).toBe(summary.state_card_query_denominator_per_backend as number);
      expect(rows.reduce((sum, row) => sum + (row.found as number), 0)).toBe(summary[`${backend}_state_card_queries_found`] as number);
    }
    expect(study.baseline_recovery_capabilities).toEqual({ "search-snapshot": "unsupported", "read-snapshot": "unsupported" });
    const provenance = record(study.provenance, "recovery provenance");
    expect(Object.keys(provenance).sort()).toEqual(["baseline_binary_sha256", "candidate_binary_sha256", "runner_sha256"]);
    for (const value of Object.values(provenance)) expect(value).toMatch(/^[a-f0-9]{64}$/u);
    expect(study.bounds).toEqual(protocol.bounds);
  });

  test("exports no private inputs, per-case records or machine locations", async () => {
    for (const name of ["recovery-study-results.json", "recovery-study-protocol.json"]) {
      const text = await read(`${directory}/${name}`);
      expect(text).not.toMatch(/\/Users\/|\/home\/|rollout-|session-\d{4}/u);
      expect(text).not.toMatch(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/u);
      expect(text).not.toMatch(/"(?:samples|rows|sample|session_id|path|snapshot|source_sha256|snapshot_sha256|manifest|manifest_sha256|missed_probes|argv|environment)"\s*:/u);
    }
  });
});

import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";

const title = "Benchmarks";
const socialTitle = "Gobstopper benchmarks";
const description =
  "Results from offline replays of 729 archived agent sessions, request-proxy replays and live counters, a synthetic snapshot-recovery study, and single-session resume trials, each dated and scoped.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/benchmarks" },
  openGraph: {
    title: socialTitle,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/benchmarks",
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: socialTitle }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: "/opengraph-image", alt: socialTitle }],
  },
};

export default function Benchmarks() {
  return (
    <>
      <SiteHeader path="/benchmarks" />
      <main id="main" tabIndex={-1} className="document-page">
        <article>
          <h1>Benchmarks</h1>
          <p>
            These results come from offline replays, which project context
            size and check which text survives, and from a few live trials
            that resumed real sessions. We don&apos;t claim subscription or
            billing savings from any of them.
          </p>
          <p>
            Latest: <a href="#archived-recovery-2026-09-20">September 20 snapshot search and recovery checks</a>.
          </p>

          <section aria-labelledby="retrospective-2026-09-19">
            <h2 id="retrospective-2026-09-19">729-session retrospective · September 19, 2026</h2>
            <p>
              <strong>36.4% median projected context reduction</strong> across
              73 high-context archived Codex root tasks, with <strong>76.9%
              sampled-string retention</strong>. All 73 produced portable{" "}
              <code>compacted</code> plans. It was an offline replay on one Mac
              with deterministic rules, so it did not measure billing savings,
              successful continuation, or model quality.
            </p>
            <p>
              Across all 729 sessions, 92 produced plans and 637 produced no
              plan, and <strong>the overall median reduction was 0%.</strong>{" "}
              Every median in the table includes no-plan cases, so the root-task
              result does not describe every session.
            </p>
            <table>
              <caption>Portable compacted strategy; projected current-context tokens</caption>
              <thead>
                <tr>
                  <th scope="col">Cohort</th>
                  <th scope="col">Sessions</th>
                  <th scope="col">Plans</th>
                  <th scope="col">No plan</th>
                  <th scope="col">Median reduction</th>
                </tr>
              </thead>
              <tbody>
                <tr><th scope="row">Archived Codex roots, high context</th><td>73</td><td>73</td><td>0</td><td>36.44%</td></tr>
                <tr><th scope="row">Codex subagents, high context</th><td>512</td><td>2</td><td>510</td><td>0%</td></tr>
                <tr><th scope="row">Claude sessions, high context</th><td>23</td><td>17</td><td>6</td><td>14.85%</td></tr>
                <tr><th scope="row">Below-trigger controls, all cohorts</th><td>121</td><td>0</td><td>121</td><td>0%</td></tr>
                <tr><th scope="row">All selected sessions</th><td>729</td><td>92</td><td>637</td><td>0%</td></tr>
              </tbody>
            </table>
            <h3>Retention and recovery</h3>
            <p>
              On the 73 root rewrites, 3,664 of 4,762 sampled strings survived;
              1,098 were absent from the smaller context. Literal matching does
              not tell us which missing strings matter for the next task.
              Snapshots make the source history available for explicit recovery;
              they do not guarantee automatic recall or task success.
            </p>
            <p>
              All 122 selected protected-tail probes survived, but only 31 of
              the 73 root rewrites had any such probes. Root baselines and
              rewritten files had no structural errors. Across the whole study,
              verifier severity/code counts did not increase; one pre-existing
              error persisted in a rewritten Claude sample. Structural checks
              do not prove that a fork will resume successfully.
            </p>
            <h3>What was tested</h3>
            <p>
              Five strategies produced 3,645 evaluations with zero evaluation
              failures. The policy used an 80,000-token trigger, a 40,000-token
              target, eight recent outputs protected, and 4,096 minimum
              estimated savings. Projected reductions apply to the live context
              after the latest native boundary, not the complete on-disk history.
              Among 57 high-context roots with earlier native compaction, the
              median projected reduction was 36.05%.
            </p>
            <p>
              The first cohort contained 576 Codex subagents and 39 Claude
              histories. A separate archive audit selected 114 roots: 73 above
              the trigger and 41 controls. It excluded 31 roots over 128 MiB
              and one without usable current usage. The cohorts did not overlap
              by session identity or exact event body. Selection was registered
              before compaction outcomes; the earlier six-root pilot is excluded.
            </p>
            <p>
              These tasks came from one person&apos;s Mac and can share prompts,
              projects and unobserved ancestry. The size limit excludes some long
              tasks. No provider continuation, remote scorer, semantic judge,
              billing, cache benefit, or recovery-time comparison was run. The
              study was not compared with{" "}
              <a href="https://github.com/tamaratran/fast-jev-compaction">fast-jev-compaction</a>,
              a third-party Claude Code plugin that uses Jev to decide which
              tool output to drop.
            </p>
            <p>
              The measured evaluator could emit 65–67 probes despite a nominal
              64-probe limit. The counts here are the actual measured counts.{" "}
              <a href="https://github.com/hraness/gobstopper/pull/32">The later cap correction</a>{" "}
              does not retroactively change this study. The downloads identify
              the measured source commit and executable hash.
            </p>
            <h3>Download the data</h3>
            <ul>
              <li><a href="/benchmarks/2026-09-19/report.md">Study report (Markdown)</a></li>
              <li><a href="/benchmarks/2026-09-19/aggregates.json">Aggregate results for all five strategies (JSON)</a></li>
              <li><a href="/benchmarks/2026-09-19/protocol.json">Selection and analysis protocol (JSON)</a></li>
            </ul>
            <p>
              Downloads contain aggregate statistics and methodology only.
              Private transcripts, session identifiers, file paths, per-session
              hashes and per-session results are excluded. The private corpus
              cannot be independently replayed from these downloads.
            </p>
          </section>

          <section aria-labelledby="retention-policy-2026-09-19">
            <h2 id="retention-policy-2026-09-19">Keep-score cutoff comparison · September 19, 2026</h2>
            <p>
              A separate paired replay tested an optional keep-score cutoff on
              the same 114 archived roots: 73 high-context tasks and 41 controls.
              It used <code>scored</code> with the deterministic heuristic and
              the corrected 64-probe limit. It compares policies on the same
              sample; it is not a new sample, a held-out validation, or a
              comparison of model backends.
            </p>
            <p>
              The 0.35 cutoff raised sampled-string retention from <strong>77.05%
              to 80.78%</strong>, while median projected reduction fell from{" "}
              <strong>36.44% to 34.96%</strong>. Forty of 73 tasks retained more
              sampled strings and none retained fewer. Two tasks produced no
              plan; their retention is derived from leaving the source unchanged.
              The cutoff trades a little size reduction for retention. The run
              did not test task success and was not compared with
              fast-jev-compaction.
            </p>
            <table>
              <caption>All 73 high-context roots; reduction medians include no-plan cases</caption>
              <thead>
                <tr>
                  <th scope="col">Variant</th>
                  <th scope="col">Plans / no plan</th>
                  <th scope="col">Median reduction</th>
                  <th scope="col">Literal retention</th>
                  <th scope="col">Tasks improved / declined</th>
                </tr>
              </thead>
              <tbody>
                <tr><th scope="row">Baseline, cutoff disabled</th><td>73 / 0</td><td>36.44%</td><td>3,596 / 4,667 (77.05%)</td><td>0 / 0</td></tr>
                <tr><th scope="row">Candidate, cutoff disabled</th><td>73 / 0</td><td>36.44%</td><td>3,596 / 4,667 (77.05%)</td><td>0 / 0</td></tr>
                <tr><th scope="row">Cutoff 0.35</th><td>71 / 2</td><td>34.96%</td><td>3,770 / 4,667 (80.78%)</td><td>40 / 0</td></tr>
                <tr><th scope="row">Cutoff 0.5</th><td>73 / 0</td><td>36.35%</td><td>3,614 / 4,667 (77.44%)</td><td>11 / 0</td></tr>
                <tr><th scope="row">Cutoff 0.65</th><td>73 / 0</td><td>36.44%</td><td>3,598 / 4,667 (77.09%)</td><td>1 / 0</td></tr>
              </tbody>
            </table>
            <p>
              Retention counts use the same 4,667 baseline probes in every row;
              improved and declined task counts compare literal retention with
              that baseline. The two unchanged-source cases at 0.35 are included
              in those totals. Controls had no reported literal-probe result.
              This corrected probe set differs from the 4,762 probes in the
              original study above; the two retention percentages cannot be
              compared as a policy effect.
            </p>
            <p>
              All 570 evaluations completed without failures. With the cutoff
              disabled, candidate and baseline plans and probe results matched
              across all 114 sessions. Every variant left all 41 below-trigger
              controls unchanged. No new verifier error or warning counts were
              observed, and frozen inputs were checked before and after replay.
              Structural checks still do not prove successful provider resume.
            </p>
            <p>
              All three cutoffs were registered before this run&apos;s outcomes,
              after the original baseline was already known; none is a tuned
              recommendation based on held-out task performance. The feature
              remains opt-in for <code>scored</code>, and defaults are unchanged.
              Literal matching does not establish semantic importance, correct
              continuation, billing savings, or automatic recovery. No local or
              remote model was called. Timing in the downloads is descriptive
              under shared host load.
            </p>
            <ul>
              <li><a href="/benchmarks/2026-09-19/retention-ablation.json">Paired aggregate results for every variant (JSON)</a></li>
              <li><a href="/benchmarks/2026-09-19/retention-ablation-protocol.json">Registered policy comparison protocol (JSON)</a></li>
            </ul>
            <p>
              The downloads identify the measured baseline and candidate
              executables. The candidate was a local build of the reviewed
              cutoff patch on main commit <code>ded1d0c</code>, separate from
              the baseline installed when this experiment ran. Downloads exclude
              private transcripts, session identifiers,
              paths, corpus manifests and per-session records.
            </p>
          </section>

          <section aria-labelledby="apple-cutoff-2026-09-19">
            <h2 id="apple-cutoff-2026-09-19">On-device Apple scorer pilot · September 19, 2026</h2>
            <p>
              A separate mechanism check used three frozen, convenience-selected
              Codex root-task inputs from one Mac, excluded from the 729-session
              study. It compared the deterministic heuristic with an on-device
              Apple Intelligence scorer that adjusts the heuristic&apos;s scores.
              Both used <code>scored</code> and a 0.5 keep-score cutoff. The pilot
              checks that this configuration runs; it does not measure model
              quality.
            </p>
            <table>
              <caption>All three inputs, including no-plan cases; one pass per variant</caption>
              <thead>
                <tr>
                  <th scope="col">Scorer</th>
                  <th scope="col">Plans / no plan</th>
                  <th scope="col">Median projected reduction</th>
                  <th scope="col">Literal retention</th>
                  <th scope="col">Median wall time</th>
                </tr>
              </thead>
              <tbody>
                <tr><th scope="row">Deterministic heuristic</th><td>3 / 0</td><td>50.35%</td><td>148 / 192</td><td>0.090 s</td></tr>
                <tr><th scope="row">Apple overlay (on-device)</th><td>1 / 2</td><td>0%</td><td>192 / 192</td><td>11.912 s</td></tr>
              </tbody>
            </table>
            <p>
              Of Apple&apos;s 192 retained probes, <strong>128 are derived from
              the two unchanged no-plan inputs</strong>; 64 were checked against
              its one rewritten output. Apple reclaimed only 5,574 projected
              tokens in total, versus 323,532 for the heuristic. It was more
              conservative and took longer in this pass. This is not evidence
              of a better compaction policy or more successful continuation.
            </p>
            <p>
              The model made 12 on-device calls and supplied 92 of 92 selected
              score overlays, with no failed batches or fallback diagnostics.
              Coverage was capped at 32 candidates per input, and candidates
              beyond the cap kept their heuristic scores by design, not because
              a call failed. No remote model was called.
            </p>
            <p>
              All six evaluations completed without failures, source inputs and
              executables stayed unchanged, and verifier error/warning counts
              did not increase. The three available protected-tail probes
              survived under both variants, but only two inputs had such probes.
              There was no provider resume, task-success test, billing measurement,
              repeatability trial, or model cold/warm control. Wall times are
              descriptive observations from one pass.
            </p>
            <ul>
              <li><a href="/benchmarks/2026-09-19/apple-retention-pilot.json">Apple pilot aggregate results (JSON)</a></li>
              <li><a href="/benchmarks/2026-09-19/apple-retention-protocol.json">Registered Apple pilot protocol (JSON)</a></li>
            </ul>
            <p>
              The results file records the SHA-256 of the measured Gobstopper
              executable and of the local Apple bridge. It excludes per-input
              records and hashes, identifiers, private paths, prompts, and model
              responses.
            </p>
          </section>

          <section aria-labelledby="archived-recovery-2026-09-20">
            <h2 id="archived-recovery-2026-09-20">Snapshot search and recovery · September 20, 2026</h2>
            <p>
              Gobstopper found all <strong>108 predeclared search targets</strong>{" "}
              and recovered all <strong>108 target records byte for byte</strong>{" "}
              in a public synthetic API study. All 553 checks passed. These are
              known-query recovery checks, not agent task-quality results.
            </p>
            <p>
              The corpus contains 36 snapshots: 18 in Codex format and 18 in
              Claude Code format, with six families per provider and three
              versions per family. A separate malformed-record fixture tests
              error accounting. Related versions are test cases, not independent
              real tasks. The synthetic corpus is separate from the private
              session studies above.
            </p>
            <table>
              <caption>All registered recovery checks; no outcome-dependent exclusions</caption>
              <thead>
                <tr><th scope="col">Check group</th><th scope="col">Passed / total</th></tr>
              </thead>
              <tbody>
                <tr><th scope="row">Known-target search</th><td>108 / 108</td></tr>
                <tr><th scope="row">Byte-exact record recovery</th><td>108 / 108</td></tr>
                <tr><th scope="row">Absent, case, key and version isolation queries</th><td>144 / 144</td></tr>
                <tr><th scope="row">Result limits and truncation counts</th><td>36 / 36</td></tr>
                <tr><th scope="row">Candidate state-card queries</th><td>108 / 108</td></tr>
                <tr><th scope="row">Baseline vault-format compatibility</th><td>36 / 36</td></tr>
                <tr><th scope="row">Invalid-input, integrity, pagination, and MCP safeguards</th><td>13 / 13</td></tr>
                <tr><th scope="row">All checks</th><td>553 / 553</td></tr>
              </tbody>
            </table>
            <p>
              Half the snapshots encode Unicode as JSON escapes; six place a
              target record across a vault chunk boundary. Recovery reconstructs
              long records through 4 KiB pages. The checks also cover
              corruption rejection, metadata-only search results, and MCP tools
              that expose transcript content only after explicit enablement.
            </p>
            <p>
              The state-card recall command found 18 of 108 declared field
              queries in the baseline build and 108 of 108 in the candidate,
              which added portable Codex-card handling and searches the error
              and current-work fields. The baseline did not have the{" "}
              <code>search-snapshot</code> and <code>read-snapshot</code> commands;
              those are recorded as unsupported capabilities, not retrieval
              failures or a speed comparison.
            </p>
            <p>
              Queries, expected bytes, and limits were fixed before execution.
              All 989 commands completed within the registered bounds; frozen
              fixtures, vault contents and pinned executables stayed unchanged.
              No local model, remote model or provider service was called.
              Runtime figures in the results file describe one pass on one machine.
            </p>
            <p>
              The vault was seeded directly, so this study does not test snapshot
              creation or whether compaction supplies the correct recovery
              pointer. Known queries do not test an agent&apos;s ability to notice
              missing information, choose useful searches, or finish a task.
              This is not a semantic-recall, provider-resume, compaction-savings,
              or matched Jev benchmark.
            </p>
            <ul>
              <li><a href="/benchmarks/2026-09-20/recovery-study-results.json">Recovery results and check counts (JSON)</a></li>
              <li><a href="/benchmarks/2026-09-20/recovery-study-protocol.json">Frozen recovery protocol (JSON)</a></li>
              <li><a href="https://github.com/hraness/gobstopper/tree/main/scripts/recovery-study">Public fixture generator and reproduction instructions</a></li>
            </ul>
            <p>
              Downloads contain aggregate checks, methodology and measured
              executable hashes. They contain no private transcripts, per-case
              records, machine paths or bundled executables.
            </p>
          </section>

          <h2>Request proxy · recorded September 25 and 26, 2026</h2>
          <p>
            On September 25, <code>gobstopper proxy replay</code>, built from
            the main branch that day, ran nine recorded sessions from one Mac
            through the proxy engine with three kept turns, the default on
            that date. Six Claude Code sessions whose
            requests peaked at 273k to 652k estimated tokens stayed at or under
            about 127k, and one Codex session that peaked at 242k stayed under
            about 127k. Two Codex sessions that Codex had already compacted
            itself began with heads near 160k and stayed under about 243k. No
            replayed request was left with an unpaired tool call.
          </p>
          <p>
            On September 26, the same Mac sent about 77 minutes of Claude Code
            traffic through <code>gobstopper proxy serve</code> from the v0.4.1
            release at its defaults. Of 3,136 requests, most were small (median
            about 6,300 estimated tokens). 89 passed the 128,000-token
            threshold; the proxy sent 78 of them smaller, 12.2 million
            estimated tokens in total down to 7.5 million (38% less). The other
            11 went out unchanged; v0.4.1 does not record why, and the likely
            cause is a threshold raised by a large verbatim head. The current
            build records the threshold applied to each request. There were no provider errors and no length retries.
          </p>
          <p>
            Both are estimates at four characters per token from one machine,
            not billed tokens, cache hit rates, or task results.
            CliffCompaction&apos;s authors report cost and benchmark results
            for their proxy on the <a href="/compare/cliffcompaction">comparison
            page</a>; Gobstopper has not rerun them.
          </p>

          <h2>Synthetic strategy benchmark</h2>
          <p>
            <code>cargo run --example bench_strategies --release</code> generates
            synthetic Codex transcripts with varying tool-output history and
            prints a CSV of actual byte reduction, projected token reduction,
            elapsed time, and structural integrity per strategy.
          </p>

          <h2>Historical live trials · recorded September 17, 2026</h2>
          <p>
            The trials below were recorded in the repository on September 17.
            They are separate single-session experiments using earlier builds
            and workflows, not validation of the 729-session replay or a current
            provider-wide performance guarantee. Their accuracy judgments concern
            the specific resume question shown, not general task completion.
          </p>
          <h3>How the live trials worked</h3>
          <p>
            These comparisons resumed real sessions after compaction. The tables
            report answers to resume questions and observed provider tokens.
            Some also include cache counters or transcript record changes.
            Task completion, latency, and re-fetch rates require separate
            measurements.
          </p>

          <h3>One Claude session: live API token comparison</h3>
          <p>
            The same 333k-token Claude session was restored from the gobstopper
            vault and asked the same resume question under four conditions.
            The question was: &quot;What were we working on? Briefly state the
            current task and the most recent concrete decision or conclusion,
            if any.&quot; Token numbers are the observed provider usage for the
            resume turn (input = cache read + cache creation + uncached input;
            output = response tokens).
          </p>
          <table>
            <thead>
              <tr>
                <th>condition</th>
                <th>input tokens</th>
                <th>output tokens</th>
                <th>recalled the standing task?</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>none (original)</td>
                <td>312,722</td>
                <td>1,405</td>
                <td>yes: npm unification and stalled renames</td>
              </tr>
              <tr>
                <td>Gobstopper <code>elide</code></td>
                <td>219,167</td>
                <td>1,052</td>
                <td>yes: same standing task, stalled renames</td>
              </tr>
              <tr>
                <td>Gobstopper <code>compacted</code></td>
                <td>220,447</td>
                <td>621</td>
                <td>yes: same standing task from the state-card digest</td>
              </tr>
              <tr>
                <td>Claude <code>--autocompact 100</code></td>
                <td>56,300</td>
                <td>416</td>
                <td>no: incorrectly claimed the renames were already done and published</td>
              </tr>
            </tbody>
          </table>
          <p>
            Gobstopper&apos;s <code>elide</code> and <code>compacted</code> both cut
            the resume context by about 30% while keeping the answer accurate. Claude&apos;s
            native <code>--autocompact 100</code> cut the resume context by ~82% but
            produced a confident, inaccurate summary of the session.
          </p>

          <h3>One Codex session: resume token comparison</h3>
          <p>
            The same resume question was asked on a 101k-token Codex session
            (a real BEAM-benchmark thread) under three conditions. Provider-native
            Codex compaction was not included in this historical trial.
          </p>
          <table>
            <thead>
              <tr>
                <th>condition</th>
                <th>input tokens</th>
                <th>output tokens</th>
                <th>recalled the standing task?</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>none (original)</td>
                <td>101,275</td>
                <td>244</td>
                <td>yes: Oh&apos;s memory benchmark and the 0.60 expansion gate</td>
              </tr>
              <tr>
                <td>Gobstopper <code>elide</code></td>
                <td>57,980</td>
                <td>83</td>
                <td>yes: same 0.545 score and 0.60 gate</td>
              </tr>
              <tr>
                <td>Gobstopper <code>compacted</code></td>
                <td>34,503</td>
                <td>159</td>
                <td>yes: same BEAM experiment and expansion gate</td>
              </tr>
            </tbody>
          </table>
          <p>
            On Codex, <code>compacted</code> cut resume input tokens by 66% and
            <code>elide</code> cut them by 43%, both with accurate answers.{" "}
            <code>gobstopper diff</code> compares two vault snapshots
            structurally, so you can check what a strategy removed before you
            rely on it.
          </p>

          <h3>One Claude session: prefix preservation and provider cache</h3>
          <p>
            The <code>cache_aware</code> strategy elides the latest stale tool
            outputs before the protected tail instead of the oldest, so the
            conversation prefix stays byte-identical. On a 339k-token Claude
            session at a 310k floor, the same resume question was asked under
            three conditions, restoring the same session between runs with the
            earlier experimental workflow. Current <code>undo</code> restores a
            Claude Code or Codex session into a separate fork and has no
            in-place mode. The provider&apos;s real cache
            counters were read from the API response:
          </p>
          <table>
            <thead>
              <tr>
                <th>condition</th>
                <th>cache read</th>
                <th>cache creation</th>
                <th>file prefix preserved</th>
                <th>cost</th>
                <th>accurate?</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>none (original)</td>
                <td>10,010</td>
                <td>325,647</td>
                <td>n/a</td>
                <td>$6.53</td>
                <td>yes</td>
              </tr>
              <tr>
                <td>Gobstopper <code>cache_aware</code></td>
                <td>13,536</td>
                <td>258,517</td>
                <td>107,884 tokens</td>
                <td>$5.19</td>
                <td>yes</td>
              </tr>
              <tr>
                <td>Gobstopper <code>compacted</code></td>
                <td>13,536</td>
                <td>257,505</td>
                <td>6,639 tokens</td>
                <td>$5.18</td>
                <td>yes</td>
              </tr>
            </tbody>
          </table>
          <p>
            All three answers were accurate. Both strategies cut cache-write
            tokens by ~21% (~20% lower cost on the resume turn). Both compacted
            conditions recorded 13,536 <code>cache_read</code> tokens, versus
            10,010 in the baseline, so the extra
            prefix preservation does not translate into more provider cache
            hits in this trial. The preserved file prefix is an auditability
            result; these measurements do not establish future cache or billing
            benefits.
          </p>

          <h3>Historical transcript structure check</h3>
          <table>
            <thead>
              <tr>
                <th>intervention</th>
                <th>records</th>
                <th>removed</th>
                <th>added</th>
                <th>resumed?</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <td>original session</td>
                <td>5,244</td>
                <td>n/a</td>
                <td>n/a</td>
                <td>yes</td>
              </tr>
              <tr>
                <td>Claude <code>--autocompact 100</code></td>
                <td>5,303</td>
                <td>0</td>
                <td>59</td>
                <td>yes</td>
              </tr>
              <tr>
                <td>Gobstopper <code>compacted</code></td>
                <td>5,247</td>
                <td>43</td>
                <td>46 (43 stubs + 3 digest)</td>
                <td>yes</td>
              </tr>
            </tbody>
          </table>
          <p>
            On the same 333k-token real Claude session, native <code>--autocompact 100</code>
            appended 59 records and removed none. The earlier Gobstopper
            experiment replaced 43 stale tool outputs and injected a
            resumable state-card digest, and <code>claude --resume</code> succeeded
            with the model recalling the last user prompt and current task state.{" "}
            <code>gobstopper diff</code> reports this kind of structural change
            between two vault snapshots.
          </p>
        </article>
      </main>
      <SiteFooter path="/benchmarks" />
    </>
  );
}

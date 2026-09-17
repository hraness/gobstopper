import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";

const title = "Gobstopper benchmarks";
const description =
  "Offline and live benchmark results for Gobstopper's context compaction strategies.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/benchmarks" },
  openGraph: {
    title,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/benchmarks",
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: title }],
  },
  twitter: {
    card: "summary_large_image",
    title,
    description,
    images: [{ url: "/opengraph-image", alt: title }],
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
            Benchmarking compaction is split between offline structural checks
            and live provider trials. We do not claim subscription savings
            without completed live evidence.
          </p>

          <h2>Offline proxy benchmark</h2>
          <p>
            <code>cargo run --example bench_strategies --release</code> generates
            synthetic Codex transcripts with varying tool-output history and
            prints a CSV of actual byte reduction, projected token reduction,
            elapsed time, and structural integrity per strategy.
          </p>

          <h2>Live qualification</h2>
          <p>
            Live trials resume a real session after compaction and measure:
          </p>
          <ul>
            <li>Task completion on a held-out second prompt.</li>
            <li>Observed input and output tokens from the provider.</li>
            <li>Latency and re-fetch count.</li>
            <li>Verbatim recall of elided tool output where relevant.</li>
          </ul>

          <h2>Live API token comparison</h2>
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
                <td>yes — npm unification and stalled renames</td>
              </tr>
              <tr>
                <td>gobstopper <code>elide</code></td>
                <td>219,167</td>
                <td>1,052</td>
                <td>yes — same standing task, stalled renames</td>
              </tr>
              <tr>
                <td>gobstopper <code>compacted</code></td>
                <td>220,447</td>
                <td>621</td>
                <td>yes — same standing task from the state-card digest</td>
              </tr>
              <tr>
                <td>Claude <code>--autocompact 100</code></td>
                <td>56,300</td>
                <td>416</td>
                <td>no — incorrectly claimed the renames were already done and published</td>
              </tr>
            </tbody>
          </table>
          <p>
            gobstopper <code>elide</code> and <code>compacted</code> both cut the
            resume context by about 30% while keeping the answer accurate. Claude&apos;s
            native <code>--autocompact 100</code> cut the resume context by ~82% but
            produced a confident, inaccurate summary of the session. gobstopper is
            built for measured, auditable compaction: every pre- and post-state is in
            the vault, so you can <code>gobstopper diff</code> the exact structural
            changes and choose the strategy that matches your tolerance for recall
            loss.
          </p>

          <h2>Results</h2>
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
                <td>—</td>
                <td>—</td>
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
                <td>gobstopper <code>compacted</code></td>
                <td>5,247</td>
                <td>43</td>
                <td>46 (43 stubs + 3 digest)</td>
                <td>yes</td>
              </tr>
            </tbody>
          </table>
          <p>
            On the same 333k-token real Claude session, native <code>--autocompact 100</code>
            appended 59 records and removed none. gobstopper&apos;s in-place
            <code>compacted</code> strategy removed 43 stale tool records, injected a
            resumable state-card digest, and <code>claude --resume</code> succeeded
            with the model recalling the last user prompt and current task state. The
            structural diff is available in the vault via
            <code>gobstopper diff</code>.
          </p>
        </article>
      </main>
      <SiteFooter path="/benchmarks" />
    </>
  );
}

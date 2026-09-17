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

          <h2>Results</h2>
          <p>
            Live results are added once each strategy completes its qualification
            protocol. The Codex custom <code>compacted</code> record is qualified:
            the provider accepts the gobstopper-written record, performs the
            context swap, and resumes cleanly. Claude and native provider
            comparisons are still in progress.
          </p>
        </article>
      </main>
      <SiteFooter path="/benchmarks" />
    </>
  );
}

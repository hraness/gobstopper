import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";

const title = "Methodology";
const socialTitle = "Gobstopper methodology";
const description =
  "How Gobstopper measures compaction: the occupancy model behind its projections, what each strategy does, and what the published benchmarks can and cannot show.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/methodology" },
  openGraph: {
    title: socialTitle,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/methodology",
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: socialTitle }],
  },
  twitter: {
    card: "summary_large_image",
    title: socialTitle,
    description,
    images: [{ url: "/opengraph-image", alt: socialTitle }],
  },
};

export default function Methodology() {
  return (
    <>
      <SiteHeader path="/methodology" />
      <main id="main" tabIndex={-1} className="document-page">
        <article>
          <h1>Methodology</h1>
          <p>
            Claude Code, Codex, and Devin compact near the top of the context
            window, where each turn costs the most and long-context recall is
            weakest. Gobstopper treats compaction as a policy you set: when to
            compact, how, and on which sessions.
          </p>

          <h2>Occupancy model</h2>
          <p>
            Context is a sawtooth. If you compact at trigger T and the summary
            floor is F, steady-state context occupancy per turn is roughly
            (T+F)/2. A 250k/40k policy is ~3.3x lower occupancy than a 1M-window
            default, and 150k/20k is ~5.6x lower.
          </p>
          <p>
            These are occupancy projections, not measured subscription savings.
            Real cost depends on cache hit rates, billing for summary turns,
            re-fetches from lost detail, and how often compaction itself runs.
          </p>

          <h2>Strategies</h2>
          <ul>
            <li><strong>auto</strong>, the default, leaves a live session to the provider&apos;s own controls and picks the best validated file strategy for an idle one.</li>
            <li><strong>sawtooth</strong> asks the provider to run its own compaction.</li>
            <li><strong>elide</strong> replaces stale tool output with short stubs, oldest first, until the context reaches the floor.</li>
            <li><strong>compacted</strong> does the same and adds a state card summarizing the hidden work. Codex-native <code>compacted</code> records are experimental and need <code>--experimental-compacted</code>.</li>
            <li><strong>structured</strong> writes a conservative state card from transcript metadata. It does not summarize meaning.</li>
            <li><strong>agentic</strong> accepts edits proposed by a command or plugin you trust, and Gobstopper still validates each one.</li>
          </ul>
          <p>
            The <a href="/docs">documentation</a> lists every strategy, including
            <code>cache_aware</code>, <code>scored</code>, <code>dedupe</code>,
            <code>micro</code>, and <code>middle</code>.
          </p>

          <h2>Structural diff and the undo vault</h2>
          <p>
            Before every change, Gobstopper stores the exact transcript in a
            content-addressed vault. Snapshots are split into deduplicated 1 MiB
            chunks, so a transcript that only grew reuses the storage of its
            unchanged beginning.
          </p>
          <p>
            <code>gobstopper diff</code> compares two snapshots by record hash
            and reports which records were added, removed, and kept. Rewrites
            leave unchanged records byte-identical, so the diff shows only the
            intended change rather than a noisy line-by-line text diff.
          </p>

          <h2>What the numbers mean</h2>
          <p>
            We report file-byte changes and observed provider usage where we have
            them, and we don&apos;t claim dollar or quota savings without a
            completed benchmark. <code>gobstopper bench</code> runs every
            strategy over your discovered sessions and writes a CSV of projected
            savings, verify errors, and probe recall. The{" "}
            <a href="/benchmarks">published studies</a> report their cohorts,
            no-ops, and limitations separately.
          </p>
        </article>
      </main>
      <SiteFooter path="/methodology" />
    </>
  );
}

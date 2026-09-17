import type { Metadata } from "next";

import { SiteHeader, SiteFooter } from "../_components/site-chrome";

const title = "Gobstopper methodology";
const description =
  "How Gobstopper measures compaction: occupancy models, offline benchmarks, and live provider qualification.";

export const metadata: Metadata = {
  title,
  description,
  alternates: { canonical: "/methodology" },
  openGraph: {
    title,
    description,
    siteName: "Gobstopper",
    type: "article",
    url: "/methodology",
    images: [{ url: "/opengraph-image", width: 1200, height: 630, alt: title }],
  },
  twitter: {
    card: "summary_large_image",
    title,
    description,
    images: [{ url: "/opengraph-image", alt: title }],
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
            Providers compact late — near the top of the context window, where
            every turn is most expensive and recall is already degrading.
            Gobstopper treats compaction as a policy problem: when to compact,
            how to compact, and where to apply it.
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
            <li><strong>elide</strong> masks stale tool output in place.</li>
            <li><strong>compacted</strong> adds a provider-native context-swap record.</li>
            <li><strong>structured</strong> emits a conservative state-card placeholder.</li>
            <li><strong>agentic</strong> reserves a bounded editor-model backend.</li>
            <li><strong>sawtooth</strong> delegates to the provider&apos;s own compaction.</li>
          </ul>

          <h2>Honest measurement</h2>
          <p>
            We report file-byte changes and observed provider usage where
            available. We do not claim dollar or quota savings without a
            completed benchmark. The local benchmark example emits CSV of byte
            and projected-token reduction across synthetic transcripts; the
            online benchmark measures task completion, latency, re-fetches, and
            cached/uncached split on real provider sessions.
          </p>
        </article>
      </main>
      <SiteFooter path="/methodology" />
    </>
  );
}

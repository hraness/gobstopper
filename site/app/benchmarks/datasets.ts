import { HRANESS_ORGANIZATION, absoluteUrl } from "../_lib/site";

/** The Hugging Face mirror of the published study files. */
export const HF_DATASET_URL = "https://huggingface.co/datasets/hranesscom/gobstopper-benchmarks";
/** The license the Hugging Face dataset card declares for the study files. */
export const DATASET_LICENSE_URL = "https://creativecommons.org/licenses/by/4.0/";

export type BenchmarkStudy = Readonly<{
  date: string;
  name: string;
  /** Summarizes the study as the benchmarks page describes it. */
  description: string;
  /** Section on /benchmarks that reports the study. */
  anchor: string;
  /** JSON files published under public/benchmarks/<date>/. */
  files: readonly string[];
}>;

/** Studies with downloadable files on /benchmarks. A test checks that every file exists. */
export const BENCHMARK_STUDIES: readonly BenchmarkStudy[] = [
  {
    date: "2026-09-19",
    name: "Gobstopper 729-session compaction retrospective, September 19, 2026",
    description:
      "Aggregate results and protocols from an offline replay of 729 archived Codex and Claude Code sessions from one Mac. Across 73 high-context archived Codex root tasks, the portable compacted strategy projected a 36.4% median context reduction with 76.9% sampled-string retention; across all 729 sessions the median reduction was 0%. Also includes a keep-score cutoff comparison on 114 archived roots (those 73 tasks and 41 controls) and a three-input on-device Apple scorer pilot. The replay did not measure billing savings, successful continuation, or model quality.",
    anchor: "#retrospective-2026-09-19",
    files: [
      "aggregates.json",
      "protocol.json",
      "retention-ablation.json",
      "retention-ablation-protocol.json",
      "apple-retention-pilot.json",
      "apple-retention-protocol.json",
    ],
  },
  {
    date: "2026-09-20",
    name: "Gobstopper snapshot search and recovery study, September 20, 2026",
    description:
      "Protocol and results from a public synthetic API study of Gobstopper's snapshot vault. Across 36 snapshots, 18 in Codex format and 18 in Claude Code format, Gobstopper found all 108 predeclared search targets and recovered all 108 target records byte for byte; all 553 checks passed. These are known-query recovery checks, not agent task-quality results.",
    anchor: "#archived-recovery-2026-09-20",
    files: ["recovery-study-results.json", "recovery-study-protocol.json"],
  },
];

export function studyFileUrl(study: BenchmarkStudy, file: string): string {
  return absoluteUrl(`/benchmarks/${study.date}/${file}`);
}

/** schema.org Dataset nodes for the studies, for the /benchmarks JSON-LD. */
export function benchmarkDatasetsJsonLd() {
  return {
    "@context": "https://schema.org",
    "@graph": BENCHMARK_STUDIES.map((study) => ({
      "@type": "Dataset",
      "@id": `${absoluteUrl("/benchmarks")}${study.anchor}`,
      name: study.name,
      description: study.description,
      url: `${absoluteUrl("/benchmarks")}${study.anchor}`,
      creator: HRANESS_ORGANIZATION,
      publisher: HRANESS_ORGANIZATION,
      license: DATASET_LICENSE_URL,
      isAccessibleForFree: true,
      temporalCoverage: study.date,
      sameAs: HF_DATASET_URL,
      distribution: study.files.map((file) => ({
        "@type": "DataDownload",
        name: file,
        contentUrl: studyFileUrl(study, file),
        encodingFormat: "application/json",
      })),
    })),
  };
}

import { COMPANION_PATH } from "../../launch/beats";

/** Where a reader of the introduction goes next, named for the task. */
export const GO_DEEPER: readonly { href: string; label: string }[] = [
  { href: COMPANION_PATH, label: "Read the full Terminal-Bench run, how the proxy trims a request, and its limits" },
  { href: "/docs", label: "Install it and read every command" },
  { href: "/benchmarks", label: "Download every benchmark result" },
  { href: "/blog/vault-models-that-fail-on-purpose", label: "See how the saved-session archive is checked" },
];

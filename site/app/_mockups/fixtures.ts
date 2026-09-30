// Real output from the gobstopper CLI (release build of this repository), run
// against a synthetic Claude Code session in an isolated home directory on
// September 29, 2026. The project ("lanternshop"), the user ("sam"), and every
// file in the session are made up. Paths are shortened to "~". Regenerate by
// following site/app/_mockups/README.md; tests/mockups.test.tsx pins the
// numbers here to each other, not to the site's launch claims.

export const SNAPSHOT = "09ccf7cd7bca78d3273058ee0ea05a59fc5e8ded066099b1ed854b4b443f4080";

export const cli = {
  detect: [
    "PROVIDER     SESSION                                STATE    CTX TOKENS    LIFETIME IN  PATH",
    "claude_code  5f0c2a9e-7b1d-4c3e-9a55-2d8e61f0b4a7   idle         130060        9027000  ~/.claude/projects/-Users-sam-code-lanternshop/5f0c2a9e-7b1d-4c3e-9a55-2d8e61f0b4a7.jsonl",
  ],
  plan: [
    "claude_code 5f0c2a9e-7b1d-4c3e-9a55-2d8e61f0b4a7 (idle)",
    "  context: 130060 -> ~39818 tokens (saves ~90242)",
    "  prefix: 83 tokens cached",
    "  strategy: elide",
    "  context 130060 tokens exceeds trigger 100000; eliding 64 of 140 stale tool outputs",
    "  elide 64 items",
  ],
  apply: [
    "claude_code 5f0c2a9e-7b1d-4c3e-9a55-2d8e61f0b4a7 (idle)",
    "  context: 130060 -> ~39818 tokens (saves ~90242)",
    "  prefix: 83 tokens cached",
    "  strategy: elide",
    "  context 130060 tokens exceeds trigger 100000; eliding 64 of 140 stale tool outputs",
    "  elide 64 items",
    "prepared ~/.claude/projects/-Users-sam-code-lanternshop/2d34930a-813b-4c47-a587-51869c4b85f7.jsonl",
    "recovery snapshot: 09ccf7cd7bca78d3273058ee0ea05a59fc5e8ded066099b1ed854b4b443f4080",
    "resume the new session: claude --resume 2d34930a-813b-4c47-a587-51869c4b85f7",
    "reclaimed ~383318 bytes in the prepared fork",
  ],
  search: [
    "{",
    "  \"snapshot_sha256\": \"09ccf7cd7bca78d3273058ee0ea05a59fc5e8ded066099b1ed854b4b443f4080\",",
    "  \"source_sha256\": \"46eef7bc276c128e3938cee1ba8c8a09f18f2305cdb070c8926f553303adbe92\",",
    "  \"matches\": [",
    "    {",
    "      \"record_index\": 2,",
    "      \"record_sha256\": \"51b814676631f07261d42dac69a269bef2ec3f8d8dc78f7e2a0a6725d7ba0d6c\",",
    "      \"record_bytes\": 4369,",
    "      \"match_count\": 9",
    "    },",
    "    {",
    "      \"record_index\": 8,",
    "      \"record_sha256\": \"516432ae6ac7e39801096b8031101981993146176165efdb0f88d1f01ebad0a9\",",
    "      \"record_bytes\": 4369,",
    "      \"match_count\": 9",
    "    }",
    "  ],",
    "  \"matched_records\": 40,",
    "  \"unsearchable_records\": 0,",
    "  \"truncated\": true",
    "}",
  ],
  undo: [
    "restore snapshot 09ccf7cd7bca78d3 — 952699 bytes, session 5f0c2a9e-7b1d-4c3e-9a55-2d8e61f0b4a7",
    "restored copy ~/.claude/projects/-Users-sam-code-lanternshop/2704bb52-109d-47fb-86ce-a5004b015748.jsonl",
    "claude --resume 2704bb52-109d-47fb-86ce-a5004b015748",
  ],
  proxyRun: [
    "2026-09-30T00:05:54.301Z gobstopper proxy: http://127.0.0.1:61631, threshold 128000 tokens, threshold_1m 256000 tokens, keep_recent 3, keep_tail_percent 0, carry_max_chars 24000, calibrate on",
    "agent sees ANTHROPIC_BASE_URL=http://127.0.0.1:61631",
    "2026-09-30T00:05:54.305Z gobstopper proxy: 0 requests, 0 compacted, 0 reused a compacted prefix",
  ],
} as const;

/** Commands exactly as they were run to produce the output above. */
export const commands = {
  detect: "gobstopper detect",
  plan: "gobstopper plan 5f0c2a9e --strategy elide --trigger 100000",
  apply: "gobstopper apply 5f0c2a9e --strategy elide --trigger 100000",
  search: `gobstopper search-snapshot ${SNAPSHOT} --query "(fail)" --limit 2`,
  undo: "gobstopper undo 5f0c2a9e",
  proxyRun: "gobstopper proxy run -- claude",
  install: "curl -fsSL https://gobstopper.sh/install.sh | sh",
  version: "gobstopper --version",
} as const;

export type ElideRow = Readonly<{ tool: "Bash" | "Read"; arg: string; bytes: number; first: string; after: string | null }>;

/** The session had 140 tool outputs; the elide plan stubbed 64. These are the first four and the last three. */
export const elide = {
  /** The session's first and last messages, from fixture-session/generate.py. */
  prompt: "Add a coupon field to the checkout page and make the tests pass.",
  finish: "The coupon field is in place and all 42 checkout tests pass.",
  total: 140,
  elided: 64,
  rows: [
    {
      "tool": "Bash",
      "arg": "bun test tests/checkout.test.ts",
      "bytes": 3863,
      "first": "tests/checkout.test.ts:",
      "after": "[output elided by gobstopper: 3863 bytes]"
    },
    {
      "tool": "Read",
      "arg": "src/checkout/totals.ts",
      "bytes": 8552,
      "first": "1\t  const line0 = compute0(cart, coupon); // src/checkout/totals.ts",
      "after": "[output elided by gobstopper: 8552 bytes]"
    },
    {
      "tool": "Read",
      "arg": "src/checkout/coupon.ts",
      "bytes": 5012,
      "first": "1\t  const line0 = compute0(cart, coupon); // src/checkout/coupon.ts",
      "after": "[output elided by gobstopper: 5012 bytes]"
    },
    {
      "tool": "Bash",
      "arg": "bun test tests/checkout.test.ts",
      "bytes": 3863,
      "first": "tests/checkout.test.ts:",
      "after": "[output elided by gobstopper: 3863 bytes]"
    },
    {
      "tool": "Read",
      "arg": "src/checkout/coupon.ts",
      "bytes": 4941,
      "first": "1\t  const line0 = compute0(cart, coupon); // src/checkout/coupon.ts",
      "after": null
    },
    {
      "tool": "Bash",
      "arg": "bun test tests/checkout.test.ts",
      "bytes": 3455,
      "first": "tests/checkout.test.ts:",
      "after": null
    },
    {
      "tool": "Read",
      "arg": "src/ui/CheckoutForm.tsx",
      "bytes": 9947,
      "first": "1\t  const line0 = compute0(cart, coupon); // src/ui/CheckoutForm.tsx",
      "after": null
    }
  ] as readonly ElideRow[],
} as const;

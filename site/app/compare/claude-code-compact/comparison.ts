/**
 * The comparison rows shared by this page and the README section
 * "How Gobstopper compares with Claude Code /compact". A test checks that the
 * README table carries the same cells, so edit both together.
 */
export const CLAUDE_COMMANDS = "https://code.claude.com/docs/en/commands";
export const CLAUDE_CONTEXT_WINDOW = "https://code.claude.com/docs/en/context-window";
export const CLAUDE_PROMPT_CACHING = "https://code.claude.com/docs/en/prompt-caching";
export const CLAUDE_CHECKPOINTING = "https://code.claude.com/docs/en/checkpointing";
export const CLAUDE_SESSIONS = "https://code.claude.com/docs/en/sessions";
export const CLAUDE_BLOG =
  "https://claude.com/blog/using-claude-code-session-management-and-1m-context";

export type ComparisonRow = Readonly<{ aspect: string; compact: string; gobstopper: string }>;

export const comparisonRows: readonly ComparisonRow[] = [
  {
    aspect: "What it changes",
    compact: "The running session's in-context history, replaced by a summary the model writes",
    gobstopper:
      "A separate copy of a saved Claude Code or Codex session file; the source is never changed. `gobstopper proxy` compacts each outgoing request and leaves session files alone",
  },
  {
    aspect: "Who writes the summary",
    compact:
      "The model, in a request carrying the same system prompt, tools, and history plus a summarization instruction; `/compact` focus text steers it",
    gobstopper:
      "No model by default: built-in strategies drop or stub stale tool results by local rules, and `structured` and `compacted` add a metadata state card",
  },
  {
    aspect: "Seeing the cut first",
    compact: "No preview; the summary is written and applied in one step, and you read what it kept afterward",
    gobstopper:
      "`gobstopper plan`, `eval`, and `diff` show each strategy's cut on the same frozen bytes before `apply` writes anything",
  },
  {
    aspect: "When it runs",
    compact:
      "On demand, or automatically as the context nears the model's limit; `/autocompact` sets how full the window gets first",
    gobstopper:
      "On demand over saved sessions; `gobstopper proxy` compacts each request over a threshold you choose, so Claude Code's own auto-compaction does not reach its trigger",
  },
  {
    aspect: "Undo",
    compact:
      "`/rewind` returns the conversation to an earlier checkpoint; file snapshots cover the 100 most recent checkpoints and are swept about 30 days after the session last saved one",
    gobstopper:
      "`gobstopper undo` restores a vaulted snapshot into a new fork; the source file is never rewritten",
  },
  {
    aspect: "What holds the originals",
    compact:
      "The session's own transcript file; Claude Code documents that summarizing leaves the original messages in the transcript",
    gobstopper:
      "A content-addressed local vault stores the exact source and candidate bytes before a copy publishes; `search-snapshot` and `read-snapshot` return archived records",
  },
  {
    aspect: "Providers covered",
    compact: "Claude Code",
    gobstopper:
      "Claude Code and Codex session files; the proxy covers any client that speaks Anthropic Messages, OpenAI Responses, or OpenAI Chat Completions with a custom provider address",
  },
  {
    aspect: "Price",
    compact: "Built into Claude Code; the summarization request consumes usage like any other model call",
    gobstopper:
      "Free and open-source (MIT or Apache-2.0); the built-in strategies and the proxy make no model calls",
  },
] as const;

export const comparisonQuestions = [
  {
    question: "Does Gobstopper replace /compact?",
    answer:
      "They work at different layers. `/compact` shrinks the live session's context in place. Gobstopper's file commands prepare a separate compacted copy of a saved Claude Code or Codex session, and `gobstopper proxy` keeps each live request under a threshold you choose, so Claude Code's auto-compaction does not reach its trigger. `/compact` stays available either way.",
  },
  {
    question: "Can I see what /compact will keep before it runs?",
    answer:
      "No. `/compact` generates the summary and applies it in one step; optional instructions such as `/compact focus on the auth refactor` steer what it keeps. `gobstopper plan` and `gobstopper eval` show the projected cut on the session's frozen bytes before any copy is written.",
  },
  {
    question: "Does /compact delete the original conversation?",
    answer:
      "No. Claude Code's checkpointing documentation says summarizing leaves the original messages in the session transcript, and `/rewind` can restore the conversation to an earlier checkpoint while its snapshots remain. What changes is the context the next request carries: whatever the summary leaves out is no longer in it, and nothing lists what was dropped. Gobstopper's vault stores the exact source bytes of every copy, so `search-snapshot` can find a record a strategy left out.",
  },
  {
    question: "What does each cost?",
    answer:
      "`/compact` is built into Claude Code and needs no install; the summarization request consumes usage like any other model call, cheapest while the prompt cache is warm. Gobstopper is free and open-source. Its built-in strategies and the proxy make no model calls, so the cut itself adds no tokens.",
  },
  {
    question: "Can I run gobstopper proxy and still use /compact?",
    answer:
      "Yes. The proxy compacts each request over its threshold, so Claude Code's auto-compaction triggers later or not at all, and `/compact` remains a command you can run. Gobstopper's file commands work on saved session files, outside the running session.",
  },
] as const;

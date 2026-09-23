#!/bin/sh
# Immutable test executable; only the invocation's isolated log is writable.
set -eu
[ "$#" -eq 3 ] && [ "$1" = app-server ] && [ "$2" = --listen ] && [ "$3" = stdio:// ] || exit 64
: "${CODEX_HOME:?test CODEX_HOME is required}"
log="$CODEX_HOME/requests.log"
compact_item() {
  printf '{"method":"item/started","params":{"threadId":"%s","turnId":"compact-turn","item":{"id":"compact-item","type":"contextCompaction"}}}\n' "$thread"
  printf '{"method":"item/completed","params":{"threadId":"%s","turnId":"compact-turn","item":{"id":"compact-item","type":"contextCompaction"}}}\n' "$thread"
}
completed() {
  printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"compact-turn","status":"completed"}}}\n' "$thread"
}
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  case "$line" in
    *'"initialize"'*) printf '{"id":0,"result":{}}\n' ;;
    *'"thread/resume"'*)
      case "$line" in
        *bad-resume*) printf '{"id":1,"error":{"message":"cannot resume an unloaded multi-agent v2 sub-agent through its parent"}}\n' ;;
        *) printf '{"id":1,"result":{}}\n' ;;
      esac ;;
    *'"thread/compact/start"'*)
      thread=${line#*'"threadId":"'}
      thread=${thread%%'"'*}
      case "$thread" in
        fail-thread|structural-thread)
          printf '{"id":2,"result":{}}\n'
          compact_item
          error='usage limit exceeded'
          [ "$thread" != structural-thread ] || error='private-transcript-marker: model not supported'
          printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"compact-turn","status":"failed","error":{"message":"%s"}}}}\n' "$thread" "$error" ;;
        error-thread) printf '{"id":2,"error":{"message":"thread not found"}}\n' ;;
        early-thread)
          compact_item
          completed
          printf '{"id":2,"result":{}}\n' ;;
        early-foreign-failure-thread|uncorrelated-failure-thread)
          printf '{"method":"turn/started","params":{"threadId":"%s","turn":{"id":"foreign-turn","status":"inProgress"}}}\n' "$thread"
          printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"foreign-turn","status":"failed"}}}\n' "$thread"
          printf '{"id":2,"result":{}}\n'
          if [ "$thread" = early-foreign-failure-thread ]; then
            compact_item
            completed
          fi ;;
        wrong-turn-thread)
          printf '{"id":2,"result":{}}\n'
          compact_item
          printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"unrelated-turn","status":"completed"}}}\n' "$thread"
          printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"compact-turn","status":"failed"}}}\n' "$thread" ;;
        no-item-thread)
          printf '{"id":2,"result":{}}\n'
          completed ;;
        oversized-thread)
          printf '{"id":2,"result":{}}\n'
          awk 'BEGIN {for (i=0;i<1048577;i++) printf "x"}' ;;
        lost-response-thread) exit 0 ;;
        malformed-response-thread) printf '{"id":2}\n' ;;
        *)
          if [ "$thread" = descendant-thread ]; then
            sleep 60 &
            printf '%s\n' "$!" > "$CODEX_HOME/descendant.pid"
          fi
          if [ "$thread" = escaped-pipe-thread ]; then
            python3 -c '
import os, pathlib, time
os.setsid()
root = pathlib.Path(os.environ["CODEX_HOME"])
(root / "escaped-ready").write_text("ready")
until = time.monotonic() + 15
while time.monotonic() < until and not (root / "escaped-stop").exists():
    time.sleep(0.005)
(root / "escaped-finished").write_text("finished")
' &
            while [ ! -f "$CODEX_HOME/escaped-ready" ]; do sleep 0.01; done
          fi
          case "${GOBSTOPPER_FIXTURE_MODE:-noop}" in
            checkpoint)
              cp "$XDG_DATA_HOME/gobstopper/watch-state-all.json" "$CODEX_HOME/dispatch-state.json" ;;
            compact)
              printf '{"type":"compacted","payload":{"replacement_history":[]}}\n' >> "$CODEX_HOME/sessions/rollout-fixture.jsonl" ;;
            lower)
              printf '{"type":"token_usage_record","payload":{"usage":{"input_tokens":1000,"output_tokens":0}}}\n' >> "$CODEX_HOME/sessions/rollout-fixture.jsonl" ;;
            missing) rm "$CODEX_HOME/sessions/rollout-fixture.jsonl" ;;
            private-error)
              printf 'private-transcript-marker\n' >&2
              printf '{"id":2,"error":{"message":"private-transcript-marker"}}\n'
              continue ;;
          esac
          printf '{"id":2,"result":{}}\n'
          compact_item
          completed ;;
      esac ;;
  esac
done

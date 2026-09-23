#!/bin/sh
# An isolated fake provider: never consult the real HOME or a provider account.
set -eu
[ "$#" -eq 4 ] && [ "$1" = --resume ] && [ "$3" = -p ] && [ "$4" = /compact ] || exit 64
: "${CLAUDE_CONFIG_DIR:?test Claude home is required}"
printf '%s\n' "$2" >> "$CLAUDE_CONFIG_DIR/compact-requests.log"
case "${GOBSTOPPER_FIXTURE_MODE:-noop}" in
  fail) exit 1 ;;
  lower)
    printf '{"type":"assistant","sessionId":"%s","uuid":"after","parentUuid":"before","message":{"role":"assistant","content":[],"usage":{"input_tokens":1000}}}\n' "$2" >> "$CLAUDE_CONFIG_DIR/projects/fixture/$2.jsonl" ;;
esac

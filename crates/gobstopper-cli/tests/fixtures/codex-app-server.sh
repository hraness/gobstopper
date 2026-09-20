#!/bin/sh
# Immutable test executable; only the invocation's isolated log is writable.
set -eu
[ "$#" -eq 3 ] && [ "$1" = app-server ] && [ "$2" = --listen ] && [ "$3" = stdio:// ] || exit 64
: "${CODEX_HOME:?test CODEX_HOME is required}"
log="$CODEX_HOME/requests.log"
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
      case "$line" in
        *fail-thread*)
          printf '{"id":2,"result":{}}\n'
          printf '{"method":"turn/completed","params":{"threadId":"fail-thread","turn":{"status":"failed","error":{"message":"usage limit exceeded"}}}}\n' ;;
        *error-thread*) printf '{"id":2,"error":{"message":"thread not found"}}\n' ;;
        *)
          printf '{"id":2,"result":{}}\n'
          printf '{"method":"turn/completed","params":{"threadId":"ok-thread","turn":{"status":"completed"}}}\n' ;;
      esac ;;
  esac
done

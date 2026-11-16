#!/bin/sh
# Fake verify step for chute-livetest. Each file under cases/ decides one outcome:
#   ok            pass
#   fail          print "FAIL <name>" and exit 1
#   flaky         fail the first time this name runs, pass afterwards
#                 (marker lives in $LIVETEST_STATE, outside the worktree)
#   hang          sleep far past the step timeout
#   slow:<n>      sleep n seconds, then pass
#   excl:<tag>    pass alone; fail when two files carry the same tag (combination failure)
set -u
status=0
tags=""
for f in cases/*; do
  [ -f "$f" ] || continue
  name=$(basename "$f" .txt)
  body=$(cat "$f")
  case "$body" in
    ok) ;;
    fail) echo "FAIL $name"; status=1 ;;
    flaky)
      mark="${LIVETEST_STATE:-/tmp}/flaky-$name"
      if [ -e "$mark" ]; then :; else
        mkdir -p "$(dirname "$mark")"; : > "$mark"
        echo "FAIL $name"; status=1
      fi ;;
    hang) echo "hanging $name"; sleep 600 ;;
    slow:*) sleep "${body#slow:}" ;;
    excl:*)
      tag="${body#excl:}"
      case " $tags " in
        *" $tag "*) echo "FAIL $name"; status=1 ;;
      esac
      tags="$tags $tag" ;;
    *) echo "FAIL $name"; echo "unknown case body: $body"; status=1 ;;
  esac
done
echo "check done status=$status"
exit $status

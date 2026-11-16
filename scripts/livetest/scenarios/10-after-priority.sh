#!/usr/bin/env bash
# 10. --after holds a PR until its target merges; --priority jumps the queue.
source "$(dirname "$0")/../lib.sh"
sc_setup after-priority

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "먼저" cases/a.txt=slow:5)
b=$(mk_pr b "나중" cases/b.txt=ok)
submit "$a" >"$W/sa.log" 2>&1 || { cat "$W/sa.log"; die "submit a 실패"; }
submit "$b" --after "$a" >"$W/sb.log" 2>&1 || { cat "$W/sb.log"; die "submit b 실패"; }
check "status가 b의 after에 a를 보인다" eq "$(status_json | jq -c --argjson n "$b" '.queue[] | select(.pr == $n) | .after')" "[$a]"
daemon_resume
wait_for 60 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.batch.phase // empty') == verifying ]]"
check "a의 batch에 b가 들어가지 않았다" eq "$(status_json | jq -c '.batch.prs')" "[$a]"
check "그동안 b는 대기다" eq "$(item_state "$b")" queued
wait_for 150 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $b --json state -q .state) == MERGED ]]"
check "a가 MERGED" eq "$(pr_state "$a")" MERGED
check "b가 MERGED" eq "$(pr_state "$b")" MERGED
check "머지 순서가 a, b다" eq "$(events_json | jq -s '[.[]|select(.event=="merged")|.pr]|join(",")' -r)" "$a,$b"
check "batch가 둘이었다" eq "$(batch_ids | wc -l)" 2

printf 'max_batch = 1\n' > "$W/cfg.toml"; cat "$LT_DIR/sandbox/chute.toml" >> "$W/cfg.toml"
push_main chute.toml=@"$W/cfg.toml"
daemon_pause
p1=$(mk_pr p1 "보통 1" cases/p1.txt=ok)
p2=$(mk_pr p2 "보통 2" cases/p2.txt=ok)
p3=$(mk_pr p3 "급한 3" cases/p3.txt=ok)
submit "$p1" >/dev/null 2>&1 || die "submit p1 실패"
submit "$p2" >/dev/null 2>&1 || die "submit p2 실패"
submit "$p3" --priority >/dev/null 2>&1 || die "submit p3 실패"
check "status가 p3에 priority를 보인다" eq "$(status_json | jq -r --argjson n "$p3" '.queue[] | select(.pr == $n) | .priority')" true
daemon_resume
wait_for 240 "[[ \$(gh pr view -R $LT_SANDBOX $p1 --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $p2 --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $p3 --json state -q .state) == MERGED ]]"
order=$(events_json | jq -s '[.[]|select(.event=="merged")|.pr]|.[2:]|join(",")' -r)
check "급한 p3가 먼저 머지됐다" eq "$order" "$p3,$p1,$p2"
sc_done

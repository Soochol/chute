#!/usr/bin/env bash
# 13. A broken main halts the queue; a repair PR submitted with --repair releases it.
source "$(dirname "$0")/../lib.sh"
sc_setup main-broken

daemon_up || die "데몬을 띄우지 못했다"
push_main cases/broken.txt=fail
a=$(mk_pr a "정상 A" cases/a.txt=ok)
submit "$a" >"$W/submit.log" 2>&1 || { cat "$W/submit.log"; die "submit 실패"; }

wait_for 120 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.blocked.reason // empty') == base_broken ]]"
check "큐가 base_broken으로 멈췄다" eq "$(status_json | jq -r '.blocked.reason')" base_broken
check "멈춘 단계가 check다" eq "$(status_json | jq -r '.blocked.step')" check
check "PR A는 반송되지 않고 큐에 남는다" eq "$(item_state "$a")" queued
check "PR A는 머지되지 않았다" eq "$(pr_state "$a")" OPEN
sleep 3
check "orch 터미널이 halted 알림을 받았다" contains "$(term_text "$ORCH")" "chute 결과 halted"

r=$(mk_pr repair "복구" cases/broken.txt=__DEL__)
submit "$r" --repair >"$W/repair.log" 2>&1 || { cat "$W/repair.log"; die "repair submit 실패"; }
wait_for 150 "[[ \$(gh pr view -R $LT_SANDBOX $r --json state -q .state) == MERGED ]]"
check "복구 PR이 MERGED" eq "$(pr_state "$r")" MERGED
check "main에서 broken.txt가 사라졌다" bash -c "cd $W/clone && git fetch -q origin main && ! git cat-file -e origin/main:cases/broken.txt 2>/dev/null"
wait_for 60 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.blocked') == null ]]"
check "멈춤이 풀렸다" eq "$(status_json | jq -r '.blocked')" null
check "resumed 사건(by=repair)이 있다" ev_has resumed "" '.by == "repair"'
sleep 3
check "orch 터미널이 resumed 알림을 받았다" contains "$(term_text "$ORCH")" "chute 결과 resumed"
wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
check "풀린 뒤 PR A도 MERGED" eq "$(pr_state "$a")" MERGED
sc_done

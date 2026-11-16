#!/usr/bin/env bash
# 4. Combination failure: each PR passes alone, together they fail.
source "$(dirname "$0")/../lib.sh"
sc_setup combo

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "조합 A" cases/a.txt=excl:x)
b=$(mk_pr b "조합 B" cases/b.txt=excl:x)
for pr in $a $b; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
daemon_resume

wait_for 180 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
wait_for 90 "[[ \$(ev_count bounced $b) -ge 1 ]]"
check "A가 MERGED" eq "$(pr_state "$a")" MERGED
check "B는 열려 있다" eq "$(pr_state "$b")" OPEN
check "B가 verify로 반송됐다" ev_has bounced "$b" '.reason == "verify"'
sleep 3
line=$(term_text "$ORCH" | grep -o "chute 결과 bounced .*pr=$b .*" | head -1)
check "반송 알림의 mode가 combo다" contains "$line" "mode=combo"
check "반송 알림의 partners가 A다" contains "$line" "partners=$a"
check "작업자 반송 줄에 상대 PR이 있다" contains "$(term_text "$AGENT")" "chute 반송: PR $b"
sc_done

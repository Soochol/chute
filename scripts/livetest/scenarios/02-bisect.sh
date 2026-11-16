#!/usr/bin/env bash
# 2. One of three PRs is broken: bisection bounces that one and merges the other two.
source "$(dirname "$0")/../lib.sh"
sc_setup bisect

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "정상 A" cases/a.txt=ok)
b=$(mk_pr b "깨진 B" cases/b.txt=fail)
c=$(mk_pr c "정상 C" cases/c.txt=ok)
for pr in $a $b $c; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
daemon_resume

wait_for 180 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $c --json state -q .state) == MERGED ]]"
wait_for 60 "[[ \$(ev_count bounced $b) -ge 1 ]]" 
check "정상 PR A가 MERGED" eq "$(pr_state "$a")" MERGED
check "정상 PR C가 MERGED" eq "$(pr_state "$c")" MERGED
check "깨진 PR B는 열려 있다" eq "$(pr_state "$b")" OPEN
check "B만 verify로 반송됐다" ev_has bounced "$b" '.reason == "verify"'
check "A, C는 반송되지 않았다" eq "$(( $(ev_count bounced "$a") + $(ev_count bounced "$c") ))" 0
check "main에 A, C의 파일이 있다" bash -c "cd $W/clone && git fetch -q origin main && git cat-file -e origin/main:cases/a.txt && git cat-file -e origin/main:cases/c.txt"
check "main에 B의 파일은 없다" bash -c "cd $W/clone && ! git cat-file -e origin/main:cases/b.txt 2>/dev/null"
sleep 3
check "작업자 터미널이 B의 반송 줄을 받았다" contains "$(term_text "$AGENT")" "chute 반송: PR $b"
check "orch 터미널이 B의 bounced 알림을 받았다" eq "$(term_prs "$ORCH" bounced)" "$b "
sc_done

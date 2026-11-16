#!/usr/bin/env bash
# 3. Two PRs edit the same line: the later one is bounced as a conflict.
source "$(dirname "$0")/../lib.sh"
sc_setup conflict

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "같은 줄 A" "app.txt=line1 from A\nline2\nline3\n")
b=$(mk_pr b "같은 줄 B" "app.txt=line1 from B\nline2\nline3\n")
for pr in $a $b; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
daemon_resume

wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
wait_for 60 "[[ \$(ev_count bounced $b) -ge 1 ]]"
check "앞의 PR A가 MERGED" eq "$(pr_state "$a")" MERGED
check "뒤의 PR B는 열려 있다" eq "$(pr_state "$b")" OPEN
check "B가 conflict로 반송됐다" ev_has bounced "$b" '.reason == "conflict"'
check "A는 반송되지 않았다" eq "$(ev_count bounced "$a")" 0
check "main의 app.txt는 A의 것이다" eq "$(git -C "$W/clone" fetch -q origin main && git -C "$W/clone" show origin/main:app.txt | head -1)" "line1 from A"
sleep 3
check "작업자 터미널이 반송 줄을 받았다" contains "$(term_text "$AGENT")" "chute 반송: PR $b"
check "보고서가 충돌한 파일 app.txt를 적었다" contains "$(find "$CHUTE_HOME" -path '*reports*' -name '*.md' -exec cat {} + 2>/dev/null)" "app.txt"
sc_done

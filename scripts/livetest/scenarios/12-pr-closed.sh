#!/usr/bin/env bash
# 12. A PR is closed while its batch verifies: it must not be merged, and the rest still lands.
source "$(dirname "$0")/../lib.sh"
sc_setup pr-closed

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "느린 A" cases/a.txt=slow:8)
b=$(mk_pr b "닫힐 B" cases/b.txt=ok)
for pr in $a $b; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
daemon_resume
wait_for 30 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.batch.phase // empty') == verifying ]]" \
  || say "batch가 verifying에 들어가지 않았다"
check "batch에 두 PR이 들어 있다" eq "$(status_json | jq -c '.batch.prs')" "[$a,$b]"
gh pr close -R "$LT_SANDBOX" "$b" >/dev/null 2>&1

wait_for 150 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
sleep 5
check "A는 MERGED" eq "$(pr_state "$a")" MERGED
check "닫은 B는 CLOSED로 남았다" eq "$(pr_state "$b")" CLOSED
check "main에 B의 파일이 없다" bash -c "cd $W/clone && git fetch -q origin main && ! git cat-file -e origin/main:cases/b.txt 2>/dev/null"
check "큐가 비었다" eq "$(status_json | jq '.queue | length')" 0
check "B의 merged 사건이 없다" eq "$(ev_count merged "$b")" 0
sc_done

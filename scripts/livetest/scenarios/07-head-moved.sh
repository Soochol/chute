#!/usr/bin/env bash
# 7. A push after submit makes the recorded HEAD stale: no merge, a head_changed bounce, no budget spent.
source "$(dirname "$0")/../lib.sh"
sc_setup head-moved

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "HEAD가 움직인다" cases/a.txt=ok)
old=$(pr_head "$a")
submit "$a" >"$W/submit.log" 2>&1 || { cat "$W/submit.log"; die "submit 실패"; }
push_more "$a" cases/a2.txt=ok
wait_for 30 "[[ \$(gh pr view -R $LT_SANDBOX $a --json headRefOid -q .headRefOid) != $old ]]"
check "push로 PR HEAD가 바뀌었다" bash -c "[[ \$(gh pr view -R $LT_SANDBOX $a --json headRefOid -q .headRefOid) != $old ]]"
daemon_resume

wait_for 90 "[[ \$(ev_count bounced $a) -ge 1 ]]"
check "옛 HEAD는 머지되지 않았다(PR 열림)" eq "$(pr_state "$a")" OPEN
check "head_changed로 반송됐다" ev_has bounced "$a" '.reason == "head_changed"'
check "main에 a.txt가 없다" bash -c "cd $W/clone && git fetch -q origin main && ! git cat-file -e origin/main:cases/a.txt 2>/dev/null"
sleep 3
check "알림이 HEAD 바뀜을 알린다" contains "$(term_text "$AGENT")" "HEAD"
check "재시도 예산을 쓰지 않았다" eq "$(status_json | jq -r --argjson n "$a" '.queue[] | select(.pr == $n) | .attempts')" 0

submit "$a" >"$W/submit2.log" 2>&1 || { cat "$W/submit2.log"; die "다시 submit 실패"; }
wait_for 90 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
check "새 HEAD로 다시 submit하면 MERGED" eq "$(pr_state "$a")" MERGED
check "main에 새 커밋의 파일도 있다" main_has cases/a2.txt
sc_done

#!/usr/bin/env bash
# 11. chute revert puts a revert commit on main; it does not open a revert PR.
source "$(dirname "$0")/../lib.sh"
sc_setup revert

daemon_up || die "데몬을 띄우지 못했다"
a=$(mk_pr a "되돌릴 PR" cases/a.txt=ok)
submit "$a" >"$W/submit.log" 2>&1 || { cat "$W/submit.log"; die "submit 실패"; }
wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
check "PR이 MERGED" eq "$(pr_state "$a")" MERGED
batch=$(batch_ids | head -1)
[[ -n $batch ]] || die "batch id를 찾지 못했다"
before=$(main_sha)
prs_before=$(gh pr list -R "$LT_SANDBOX" --state all --json number -q length)

out=$(cd "$W/clone" && "$LT_CHUTE" revert "$batch" 2>&1); rc=$?
say "revert 출력: ${out//$'\n'/ | }"
check "chute revert가 성공했다" eq "$rc" 0
after=$(main_sha)
check "main이 앞으로 나갔다" bash -c "[[ $before != $after ]]"
check "main 끝이 revert 커밋이다" contains "$(main_log | head -1)" "Revert"
check "되돌린 파일이 main에서 사라졌다" bash -c "cd $W/clone && ! git cat-file -e origin/main:cases/a.txt 2>/dev/null"
check "원래 PR 커밋은 이력에 남아 있다" contains "$(main_log)" "되돌릴 PR"
check "새 PR이 생기지 않았다" eq "$(gh pr list -R "$LT_SANDBOX" --state all --json number -q length)" "$prs_before"
check "원래 PR은 여전히 MERGED" eq "$(pr_state "$a")" MERGED
check "reverted 사건이 기록됐다" ev_has reverted "" ".batch == \"$batch\""
sleep 3
check "orch 터미널이 reverted 알림을 받았다" contains "$(term_text "$ORCH")" "chute 결과 reverted"
sc_done

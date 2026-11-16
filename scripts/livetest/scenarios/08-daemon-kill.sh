#!/usr/bin/env bash
# 8. Kill the daemon during a batch, start it again: the queue and main stay consistent.
source "$(dirname "$0")/../lib.sh"
sc_setup daemon-kill

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "느린 A" cases/a.txt=slow:8)
b=$(mk_pr b "정상 B" cases/b.txt=ok)
for pr in $a $b; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
old_pid=$(daemon_pid)
daemon_resume
wait_for 30 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.batch.phase // empty') == verifying ]]" \
  || say "batch가 verifying에 들어가지 않았다"
sleep 2
before=$(main_sha)
kill -9 "$old_pid"
sleep 1
check "죽인 뒤 main은 그대로다" eq "$(main_sha)" "$before"
check "죽인 뒤 두 PR은 아직 열려 있다" bash -c "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == OPEN && \$(gh pr view -R $LT_SANDBOX $b --json state -q .state) == OPEN ]]"

(cd "$W/clone" && "$LT_CHUTE" up >"$W/up2.log" 2>&1) || { cat "$W/up2.log"; die "다시 띄우지 못했다"; }
wait_for 30 "[[ -n \$(jq -r '.pid // empty' $CHUTE_HOME/repos/*/daemon.json) && \$(jq -r .pid $CHUTE_HOME/repos/*/daemon.json) != $old_pid ]]"
track_term "$(daemon_terminal)"
check "새 데몬이 다른 pid로 떴다" bash -c "[[ \$(jq -r .pid $CHUTE_HOME/repos/*/daemon.json) != $old_pid ]]"

wait_for 180 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $b --json state -q .state) == MERGED ]]"
check "A가 MERGED" eq "$(pr_state "$a")" MERGED
check "B가 MERGED" eq "$(pr_state "$b")" MERGED
check "큐가 비었다" eq "$(status_json | jq '.queue | length')" 0
check "merged 사건이 PR마다 하나씩이다" eq "$(events_json | jq -s '[.[]|select(.event=="merged")|.pr]|sort|join(",")' -r)" "$(printf '%s\n' "$a" "$b" | sort -n | paste -sd, -)"
check "main에 두 파일이 모두 있다" bash -c "cd $W/clone && git fetch -q origin main && git cat-file -e origin/main:cases/a.txt && git cat-file -e origin/main:cases/b.txt"
check "main이 seed 위에 합쳐 올라갔다(고아 커밋 없음)" bash -c "cd $W/clone && git merge-base --is-ancestor \$(git rev-list --max-parents=0 origin/main | tail -1) origin/main"
sc_done

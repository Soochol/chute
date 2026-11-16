#!/usr/bin/env bash
# 1. Three PRs merge as one batch and all end MERGED on GitHub.
source "$(dirname "$0")/../lib.sh"
sc_setup batch-merge

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
a=$(mk_pr a "배치 A" cases/a.txt=ok)
b=$(mk_pr b "배치 B" cases/b.txt=ok)
c=$(mk_pr c "배치 C" cases/c.txt=ok)
for pr in $a $b $c; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
daemon_resume

wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $b --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $c --json state -q .state) == MERGED ]]"
for pr in $a $b $c; do check "PR #$pr가 GitHub에서 MERGED" eq "$(pr_state "$pr")" MERGED; done
check "merged 이벤트 3개" eq "$(ev_count merged)" 3
check "batch가 하나뿐이다" eq "$(events_json | jq -s '[.[]|select(.event=="batch_finished")]|length')" 1
check "그 batch가 PR 3개를 머지했다" eq "$(events_json | jq -s '[.[]|select(.event=="batch_finished")][0].merged|length')" 3
sleep 3
check "orch 터미널이 PR $a $b $c의 merged 알림을 받았다" eq "$(term_prs "$ORCH" merged)" "$a $b $c "
sc_done

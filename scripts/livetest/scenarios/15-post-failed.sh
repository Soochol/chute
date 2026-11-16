#!/usr/bin/env bash
# 15. A step after the merge fails on main: the run finds the merge it starts at among the
# merges since the last green commit, tells that PR's workers, and a fix turns it green again.
source "$(dirname "$0")/../lib.sh"
sc_setup post-failed

enable_post_verify
base=$(main_sha)
daemon_up || die "데몬을 띄우지 못했다"
wait_for 120 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.post_verify.last_green.commit // empty') == $base ]]"
check "처음 tip이 last green이 됐다" eq "$(post_green)" "$base"

daemon_pause
a=$(mk_pr a "정상 A" post/a.txt=ok)
b=$(mk_pr b "머지 뒤에 깨뜨리는 B" post/b.txt=fail)
c=$(mk_pr c "정상 C" post/c.txt=ok)
for pr in $a $b $c; do submit "$pr" >"$W/submit-$pr.log" 2>&1 || { cat "$W/submit-$pr.log"; die "submit $pr 실패"; }; done
daemon_resume

wait_for 180 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $b --json state -q .state) == MERGED && \$(gh pr view -R $LT_SANDBOX $c --json state -q .state) == MERGED ]]"
check "batch는 빠른 단계만 돌려 세 PR 모두 MERGED" eq "$(pr_state "$a")$(pr_state "$b")$(pr_state "$c")" MERGEDMERGEDMERGED
check "머지 때는 반송이 없다" eq "$(ev_count bounced)" 0

wait_for 240 "ev_has post_failed ''"
check "post_failed 사건의 범인이 B다" ev_has post_failed "" ".culprit.pr == $b"
check "사건의 마지막 통과가 처음 tip이다" ev_has post_failed "" ".last_green == \"$base\""
check "사건의 단계가 post다" ev_has post_failed "" '.step == "post"'
check "실패한 시험 이름이 b다" ev_has post_failed "" '.tests | index("b") != null'
red=$(status_json | jq -c '.post_verify.red')
check "상태의 범인이 B다" eq "$(jq -r .culprit.pr <<<"$red")" "$b"
check "상태의 마지막 통과가 처음 tip이다" eq "$(jq -r .last_green <<<"$red")" "$base"
check "last green은 그대로다" eq "$(post_green)" "$base"
check "보고서가 있다" test -f "$(jq -r .report <<<"$red")"
sleep 3
check "작업자 터미널이 post_failed 알림을 받았다" contains "$(term_text "$AGENT")" "chute 결과 post_failed"
check "작업자가 받은 알림이 B를 가리킨다" contains "$(term_text "$AGENT")" "pr=$b"
check "orch 터미널이 post_failed 알림을 받았다" contains "$(term_text "$ORCH")" "chute 결과 post_failed"
check "알림이 chute revert를 안내한다" contains "$(term_text "$ORCH")" "chute revert"
check "A와 C는 범인으로 알리지 않는다" eq "$(term_text "$ORCH" | grep -o 'chute 결과 post_failed .*' | grep -o ' pr=[0-9]*' | sort -u | tr -d '\n')" " pr=$b"
check "작업자에게 간 post_failed 알림이 기록됐다" ev_has notified "" ".kind == \"post_failed\" and .to == \"$AGENT\" and .delivered == \"sent\""
check "submit한 터미널에 간 post_failed 알림이 기록됐다" ev_has notified "" ".kind == \"post_failed\" and .to == \"$ORCH\" and .delivered == \"sent\""
check "같은 실패는 사건 하나다" eq "$(ev_count post_failed)" 1

fix=$(mk_pr fix "B 고침" post/b.txt=__DEL__)
submit "$fix" >"$W/submit-fix.log" 2>&1 || { cat "$W/submit-fix.log"; die "submit fix 실패"; }
wait_for 180 "[[ \$(gh pr view -R $LT_SANDBOX $fix --json state -q .state) == MERGED ]]"
check "고치는 PR이 MERGED" eq "$(pr_state "$fix")" MERGED
tip=$(main_sha)
wait_for 240 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.post_verify.last_green.commit // empty') == $tip ]]"
check "last green이 고친 tip으로 옮겨졌다" eq "$(post_green)" "$tip"
check "남은 실패가 없다" eq "$(status_json | jq -r '.post_verify.red')" null
check "post_fixed 사건이 있다" ev_has post_fixed "" ".pr == $b"
sleep 3
check "작업자 터미널이 post_fixed 알림을 받았다" contains "$(term_text "$AGENT")" "chute 결과 post_fixed"
sc_done

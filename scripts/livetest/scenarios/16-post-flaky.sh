#!/usr/bin/env bash
# 16. A step after the merge that fails once and passes on the re-run is a wobble: main
# turns green, the flaky ledger has it, and nobody is told it failed.
source "$(dirname "$0")/../lib.sh"
sc_setup post-flaky

enable_post_verify
base=$(main_sha)
daemon_up || die "데몬을 띄우지 못했다"
wait_for 120 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.post_verify.last_green.commit // empty') == $base ]]"

a=$(mk_pr a "흔들리는 A" post/wobble.txt=flaky)
submit "$a" >"$W/submit-a.log" 2>&1 || { cat "$W/submit-a.log"; die "submit 실패"; }
wait_for 180 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
check "PR이 MERGED" eq "$(pr_state "$a")" MERGED
tip=$(main_sha)
wait_for 240 "[[ \$(cd $W/clone && '$LT_CHUTE' status --json | jq -r '.post_verify.last_green.commit // empty') == $tip ]]"
check "다시 돌려 통과해 tip이 last green이 됐다" eq "$(post_green)" "$tip"
check "남은 실패가 없다" eq "$(status_json | jq -r '.post_verify.red')" null
check "flaky 사건이 post- 이름으로 남았다" ev_has flaky "" '.batch | startswith("post-")'
check "flaky 사건의 시험이 wobble이다" ev_has flaky "" '.tests | index("wobble") != null'
ledger=$(chute_json flaky --json 2>&1)
check "chute flaky 장부에 wobble이 있다" contains "$ledger" wobble
check "장부의 단계가 post다" contains "$ledger" '"post"'
check "post_failed 사건이 없다" eq "$(ev_count post_failed)" 0
sleep 3
check "작업자 터미널은 post_failed를 받지 않았다" eq "$(term_text "$AGENT" | grep -c 'chute 결과 post_failed')" 0
check "orch 터미널도 post_failed를 받지 않았다" eq "$(term_text "$ORCH" | grep -c 'chute 결과 post_failed')" 0
sc_done

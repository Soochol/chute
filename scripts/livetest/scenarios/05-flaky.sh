#!/usr/bin/env bash
# 5. A flaky step passes on the re-run and lands in the flaky ledger.
source "$(dirname "$0")/../lib.sh"
sc_setup flaky

daemon_up || die "데몬을 띄우지 못했다"
a=$(mk_pr a "흔들림" cases/wobble.txt=flaky)
submit "$a" >"$W/submit.log" 2>&1 || { cat "$W/submit.log"; die "submit 실패"; }

wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
check "PR이 MERGED" eq "$(pr_state "$a")" MERGED
check "반송되지 않았다" eq "$(ev_count bounced "$a")" 0
check "flaky 사건이 기록됐다" ev_has flaky "" '.tests | index("wobble") != null'
ledger=$(chute_json flaky --json 2>&1)
check "chute flaky 장부에 wobble이 있다" contains "$ledger" wobble
check "장부의 단계가 check다" contains "$ledger" check
sc_done

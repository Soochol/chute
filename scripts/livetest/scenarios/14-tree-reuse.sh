#!/usr/bin/env bash
# 14. A tree that preflight passed is not verified again by batch; a tree that differs is.
source "$(dirname "$0")/../lib.sh"
sc_setup tree-reuse

daemon_up || die "데몬을 띄우지 못했다"

a=$(mk_pr a "재사용 A" cases/a.txt=ok)
pre=$(chute_json preflight --json 2>"$W/preflight-a.err") || { cat "$W/preflight-a.err"; die "preflight 실패: $pre"; }
check "preflight가 통과했다" eq "$(jq -r .result <<<"$pre")" pass
submit "$a" >"$W/submit-a.log" 2>&1 || { cat "$W/submit-a.log"; die "submit 실패"; }
wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $a --json state -q .state) == MERGED ]]"
check "PR A가 MERGED" eq "$(pr_state "$a")" MERGED
check "reused 사건이 preflight의 결과를 가리킨다" ev_has reused "" '.by == "preflight"'
check "reused 사건이 하나다" eq "$(ev_count reused)" 1

b=$(mk_pr b "재사용 B" cases/b.txt=ok)
pre=$(chute_json preflight --json 2>"$W/preflight-b.err") || { cat "$W/preflight-b.err"; die "preflight 실패: $pre"; }
check "두 번째 preflight가 통과했다" eq "$(jq -r .result <<<"$pre")" pass
push_main cases/m.txt=ok
submit "$b" >"$W/submit-b.log" 2>&1 || { cat "$W/submit-b.log"; die "submit 실패"; }
wait_for 120 "[[ \$(gh pr view -R $LT_SANDBOX $b --json state -q .state) == MERGED ]]"
check "PR B가 MERGED" eq "$(pr_state "$b")" MERGED
check "main이 움직여 트리가 달라졌으니 다시 쓰지 않았다" eq "$(ev_count reused)" 1
check "batch가 둘이다" eq "$(ev_count batch_finished)" 2
sc_done

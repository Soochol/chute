#!/usr/bin/env bash
# 9. The worker terminal is gone when a bounce is due: the PR stays bounced and the submitter gets the bounce line.
source "$(dirname "$0")/../lib.sh"
sc_setup unreachable

daemon_up || die "데몬을 띄우지 못했다"
daemon_pause
gone_killed=$(new_terminal worker-killed)
gone_removed=$(new_terminal worker-removed)
x=$(mk_pr x "작업자 종료" cases/x.txt=fail)
y=$(mk_pr y "작업자 삭제" cases/y.txt=fail)
SUBMIT_AGENT=$gone_killed submit "$x" >"$W/sx.log" 2>&1 || { cat "$W/sx.log"; die "submit x 실패"; }
SUBMIT_AGENT=$gone_removed submit "$y" >"$W/sy.log" 2>&1 || { cat "$W/sy.log"; die "submit y 실패"; }
lt_hc kill "$gone_killed" >/dev/null 2>&1
lt_hc kill "$gone_removed" >/dev/null 2>&1; lt_hc rm "$gone_removed" >/dev/null 2>&1
daemon_resume

wait_for 180 "[[ \$(ev_count bounced $x) -ge 1 && \$(ev_count bounced $y) -ge 1 ]]"
check "x 반송 이벤트에 닿지 않은 까닭이 있다" ev_has bounced "$x" '.unreachable != null'
check "y 반송 이벤트에 닿지 않은 까닭이 있다" ev_has bounced "$y" '.unreachable != null'
check "x 상태가 bounced" eq "$(item_state "$x")" bounced
check "y 상태가 bounced" eq "$(item_state "$y")" bounced
check "두 PR 모두 머지되지 않았다" bash -c "[[ \$(gh pr view -R $LT_SANDBOX $x --json state -q .state) == OPEN && \$(gh pr view -R $LT_SANDBOX $y --json state -q .state) == OPEN ]]"
sleep 3
check "orch 터미널이 두 PR의 반송 줄을 받았다" eq "$(term_prs "$ORCH" bounced)" "$(printf '%s\n' "$x" "$y" | sort -n | paste -sd' ' -) "
check "알림에 delivered=unreachable이 있다" contains "$(term_text "$ORCH")" "delivered=unreachable"
known=$(sort -u "$W/terms" | jq -R . | jq -s .)
extra=$(lt_hc ls --json | jq --arg w "$W" --argjson known "$known" '[.terminals[] | select((.cwd // "") | startswith($w)) | select(.address as $a | $known | index($a) | not)] | length')
check "새 에이전트 터미널이 생기지 않았다(respawn 빔)" eq "$extra" 0
sc_done

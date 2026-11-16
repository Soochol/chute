#!/usr/bin/env bash
# 6. A step that outlives its time limit is bounced as a timeout and is not left running.
source "$(dirname "$0")/../lib.sh"
sc_setup timeout

daemon_up || die "데몬을 띄우지 못했다"
a=$(mk_pr a "멈춘 단계" cases/stuck.txt=hang)
submit "$a" >"$W/submit.log" 2>&1 || { cat "$W/submit.log"; die "submit 실패"; }

wait_for 120 "[[ \$(ev_count bounced $a) -ge 1 ]]"
check "PR은 열려 있다" eq "$(pr_state "$a")" OPEN
check "timeout으로 반송됐다" ev_has bounced "$a" '.reason == "timeout"'
sleep 3
check "작업자 터미널이 반송 줄을 받았다" contains "$(term_text "$AGENT")" "chute 반송: PR $a"
check "멈춘 sleep 프로세스가 남지 않았다" bash -c "! pgrep -fx 'sleep 600' >/dev/null"
sc_done

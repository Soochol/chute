#!/usr/bin/env bash
# Run the chute live tests against the private sandbox repo, one scenario after another.
#   scripts/livetest/run.sh            all scenarios
#   scripts/livetest/run.sh 02 07      only scenarios whose file name starts with these
# Exit status: 0 all passed, 1 a scenario failed, 2 the environment is not usable.
# Environment: CHUTE_BIN (chute binary, default target/debug/chute), LIVETEST_REPO
# (default Soochol/chute-sandbox; the name must end in /chute-sandbox), LIVETEST_TIMEOUT
# (seconds per scenario, default 600).
set -uo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export LT_RUN_ID=$(date +%H%M%S)$RANDOM
source "$HERE/lib.sh"
guard_repo

for tool in git gh hc jq setsid timeout; do
  command -v "$tool" >/dev/null || die "$tool 이 PATH에 없다"
done
gh auth status >/dev/null 2>&1 || die "gh에 로그인돼 있지 않다"
gh api "repos/$LT_SANDBOX" -q .private 2>/dev/null | grep -qx true || die "$LT_SANDBOX 가 private 저장소로 보이지 않는다"
hc ls >/dev/null 2>&1 || die "hc 코어에 닿지 않는다"
if [[ ! -x $LT_CHUTE ]]; then
  if [[ -z ${CHUTE_BIN:-} ]]; then
    (cd "$LT_REPO_ROOT" && cargo build --quiet) || die "cargo build 실패"
  fi
  [[ -x $LT_CHUTE ]] || die "chute 실행 파일이 없다: $LT_CHUTE"
fi

LOG_DIR=$(mktemp -d "${TMPDIR:-/tmp}/chute-livetest-logs.XXXXXX")
export LT_CHUTE
before_terms=$(hc ls --json | jq -r '.terminals[].address' | sort)

pick=("$@")
scenarios=()
for f in "$HERE"/scenarios/[0-9][0-9]-*.sh; do
  n=$(basename "$f")
  if (( ${#pick[@]} == 0 )); then scenarios+=("$f"); continue; fi
  for p in "${pick[@]}"; do [[ $n == "$p"* ]] && scenarios+=("$f"); done
done
(( ${#scenarios[@]} > 0 )) || die "고른 시나리오가 없다"

declare -a names results secs
fail=0
for f in "${scenarios[@]}"; do
  n=$(basename "$f" .sh)
  printf '\n== %s\n' "$n"
  start=$SECONDS
  setsid timeout --foreground --kill-after=20 "${LIVETEST_TIMEOUT:-600}" bash "$f" 2>&1 | tee "$LOG_DIR/$n.log"
  rc=${PIPESTATUS[0]}
  names+=("$n"); secs+=("$((SECONDS - start))")
  case $rc in
    0) results+=("통과") ;;
    1) results+=("실패"); fail=1 ;;
    124|137|143) results+=("시간 초과"); fail=1 ;;
    *) results+=("환경 오류($rc)"); fail=1 ;;
  esac
done

leaks=0
echo; echo "== 남은 것 확인"
after_terms=$(hc ls --json | jq -r '.terminals[].address' | sort)
extra_terms=$(comm -13 <(printf '%s\n' "$before_terms") <(printf '%s\n' "$after_terms"))
if [[ -n $extra_terms ]]; then echo "  남은 터미널: $extra_terms"; leaks=1; else echo "  터미널: 없음"; fi
open_prs=$(gh pr list -R "$LT_SANDBOX" --state open --json number -q length)
(( open_prs == 0 )) && echo "  열린 PR: 없음" || { echo "  열린 PR: $open_prs개"; leaks=1; }
extra_br=$(gh api "repos/$LT_SANDBOX/branches" --paginate -q '.[].name' | grep -vx main || true)
[[ -z $extra_br ]] && echo "  브랜치: main뿐" || { echo "  남은 브랜치: $extra_br"; leaks=1; }
tmp_left=$(ls -d "${TMPDIR:-/tmp}"/chute-livetest.* 2>/dev/null | wc -l)
(( tmp_left == 0 )) && echo "  임시 폴더: 없음" || { echo "  임시 폴더 $tmp_left개"; leaks=1; }
daemons=0
for pid in $(pgrep -x chute 2>/dev/null); do
  tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -q '^CHUTE_HOME=.*chute-livetest' && daemons=$((daemons + 1))
done
(( daemons == 0 )) && echo "  시험 데몬: 없음" || { echo "  시험 데몬 $daemons개"; leaks=1; }
(( leaks == 0 )) || fail=1

echo; echo "== 결과"
printf '  %-22s %-12s %s\n' 시나리오 결과 '시간(초)'
for i in "${!names[@]}"; do printf '  %-22s %-12s %s\n' "${names[$i]}" "${results[$i]}" "${secs[$i]}"; done
echo "  로그: $LOG_DIR"
exit $fail

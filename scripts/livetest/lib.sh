#!/usr/bin/env bash

LT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
LT_REPO_ROOT=$(cd "$LT_DIR/../.." && pwd)

LT_SANDBOX=${LIVETEST_REPO:-Soochol/chute-sandbox}
LT_CHUTE=${CHUTE_BIN:-$LT_REPO_ROOT/target/debug/chute}
LT_RUN_ID=${LT_RUN_ID:-$(date +%H%M%S)$RANDOM}
LT_FAILS=0
LT_CHECKS=0

say() { printf '  %s\n' "$*"; }
die() { printf 'livetest: %s\n' "$*" >&2; exit 2; }

guard_repo() {
  [[ $LT_SANDBOX == */chute-sandbox ]] || die "시험 저장소 이름이 chute-sandbox가 아니다: $LT_SANDBOX"
}

check() {
  local desc=$1; shift
  LT_CHECKS=$((LT_CHECKS + 1))
  if "$@"; then
    printf '  ok   %s\n' "$desc"
  else
    LT_FAILS=$((LT_FAILS + 1))
    printf '  FAIL %s\n' "$desc"
  fi
}

eq() { [[ "$1" == "$2" ]] || { say "     기대 '$2', 실제 '$1'"; return 1; }; }
contains() { [[ "$1" == *"$2"* ]] || { say "     '$2'가 없다: ${1:0:300}"; return 1; }; }

wait_for() {
  local limit=$1; shift
  local end=$((SECONDS + limit))
  while (( SECONDS < end )); do
    eval "$*" >/dev/null 2>&1 && return 0
    sleep 1
  done
  return 1
}

lt_hc() { command hc "$@"; }

sc_setup() {
  guard_repo
  LT_NAME=$1
  W=$(mktemp -d "${TMPDIR:-/tmp}/chute-livetest.XXXXXX")
  export CHUTE_HOME=$W/home LIVETEST_STATE=$W/marks
  mkdir -p "$W/bin" "$LIVETEST_STATE" "$CHUTE_HOME"
  ln -s "$LT_CHUTE" "$W/bin/chute"
  # hc terminals do not inherit CHUTE_HOME, so the daemon that chute launches through
  # `hc run -- chute daemon` would read the user's real state. The shim points it at ours.
  cat > "$W/bin/hc" <<SHIM
#!/usr/bin/env bash
real=\$(PATH=\${PATH#$W/bin:} command -v hc)
args=("\$@")
if [[ \${args[0]:-} == run ]]; then
  for i in "\${!args[@]}"; do
    if [[ \${args[\$i]} == chute && \${args[\$((i+1))]:-} == daemon && \${args[\$((i-1))]:-} == -- ]]; then
      args=("\${args[@]:0:\$i}" env CHUTE_HOME="$CHUTE_HOME" LIVETEST_STATE="$LIVETEST_STATE" "$LT_CHUTE" daemon)
      break
    fi
  done
fi
exec "\$real" "\${args[@]}"
SHIM
  chmod +x "$W/bin/hc"
  export PATH=$W/bin:$PATH
  : > "$W/terms"
  trap sc_teardown EXIT
  trap 'exit 143' TERM INT HUP
  sandbox_reset
  git clone -q "https://github.com/$LT_SANDBOX.git" "$W/clone" || die "clone 실패"
  cd "$W/clone" || die "clone 폴더로 가지 못했다"
  git config user.name livetest
  git config user.email livetest@example.invalid
  (cd "$W/clone" && "$LT_CHUTE" init >"$W/init.log" 2>&1) || { cat "$W/init.log"; die "chute init 실패"; }
  AGENT=$(new_terminal agent)
  ORCH=$(new_terminal orch)
}

sandbox_reset() {
  guard_repo
  local seed=$W/seed
  rm -rf "$seed"; mkdir -p "$seed"
  cp -r "$LT_DIR/sandbox/." "$seed/"
  (
    cd "$seed" || exit 1
    git init -q -b main
    git config user.name livetest
    git config user.email livetest@example.invalid
    git add -A
    GIT_AUTHOR_DATE='2026-01-01T00:00:00Z' GIT_COMMITTER_DATE='2026-01-01T00:00:00Z' \
      git commit -q -m 'chore: seed'
    git push -q --force "https://github.com/$LT_SANDBOX.git" main:main
  ) || die "시험 저장소 main을 되돌리지 못했다"
  rm -rf "$seed"
  local pr
  for pr in $(gh pr list -R "$LT_SANDBOX" --state open --json number -q '.[].number'); do
    gh pr close -R "$LT_SANDBOX" "$pr" --delete-branch >/dev/null 2>&1 || true
  done
  local br
  for br in $(gh api "repos/$LT_SANDBOX/branches" --paginate -q '.[].name'); do
    [[ $br == main ]] && continue
    gh api -X DELETE "repos/$LT_SANDBOX/git/refs/heads/$br" >/dev/null 2>&1 || true
  done
}

sc_teardown() {
  local rc=$?
  trap - EXIT
  set +e
  cd /
  daemon_kill
  track_term "$(daemon_terminal)"
  local t
  for t in $(sort -u "$W/terms" 2>/dev/null); do
    lt_hc kill "$t" >/dev/null 2>&1
    lt_hc rm "$t" >/dev/null 2>&1
  done
  sandbox_reset
  rm -rf "$W"
  exit $rc
}

sc_done() {
  printf '  -- %s: 단언 %d개, 실패 %d개\n' "$LT_NAME" "$LT_CHECKS" "$LT_FAILS"
  if (( LT_FAILS > 0 )); then
    say "-- 사건 기록 --"
    events_json 2>&1 | jq -c 'del(.submitted_ms)' 2>/dev/null | cut -c1-260 | sed 's/^/    /'
    say "-- 상태 --"
    status_json 2>&1 | jq -c '{blocked, batch, queue: [.queue[]? | {pr, state, state_reason, attempts, after, last_failure}]}' 2>/dev/null | sed 's/^/    /'
    say "-- 터미널 orch --"; term_text "$ORCH" 2>/dev/null | cut -c1-260 | sed 's/^/    /'
    say "-- 터미널 agent --"; term_text "$AGENT" 2>/dev/null | cut -c1-260 | sed 's/^/    /'
  fi
  (( LT_FAILS == 0 ))
  exit $?
}

track_term() { [[ -n ${1:-} ]] && printf '%s\n' "$1" >> "$W/terms"; }

new_terminal() {
  local name="lt-$LT_RUN_ID-$1" out addr
  out=$(lt_hc run --bg --json --no-parent --name "$name" -- cat) || die "hc run 실패: $out"
  addr=$(jq -r .address <<<"$out")
  track_term "$addr"
  printf '%s' "$addr"
}

term_text() { lt_hc read "$1" 2>/dev/null; }

# The PR numbers a terminal was told about for one notification kind, once each. The pty
# echoes what is typed and cat prints it again, so raw line counts double.
term_prs() {
  term_text "$1" | grep -o "chute 결과 $2 .*" | grep -o ' pr=[0-9]*' | cut -d= -f2 | sort -nu | tr '\n' ' '
}

term_alive() { lt_hc get "$1" --json 2>/dev/null | jq -e '.state == "live"' >/dev/null; }

daemon_pid() { jq -r '.pid // empty' "$CHUTE_HOME"/repos/*/daemon.json 2>/dev/null | head -1; }
daemon_terminal() { jq -r '.terminal // empty' "$CHUTE_HOME"/repos/*/daemon.json 2>/dev/null | head -1; }

daemon_up() {
  (cd "$W/clone" && "$LT_CHUTE" up >"$W/up.log" 2>&1) || { cat "$W/up.log"; return 1; }
  wait_for 30 '[[ -n $(daemon_pid) ]]'
  track_term "$(daemon_terminal)"
  return 0
}

daemon_pause() { kill -STOP "$(daemon_pid)"; }
daemon_resume() { kill -CONT "$(daemon_pid)"; }

daemon_kill() {
  local pid; pid=$(daemon_pid)
  [[ -n $pid ]] || return 0
  if tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null | grep -q 'chute.* daemon'; then
    kill -CONT "$pid" 2>/dev/null
    kill -9 "$pid" 2>/dev/null
  fi
}

stage_files() {
  local kv
  for kv in "$@"; do
    local path=${kv%%=*} body=${kv#*=}
    if [[ $body == __DEL__ ]]; then
      git rm -q -f "$path"
    elif [[ $body == @* ]]; then
      mkdir -p "$(dirname "$path")"
      cp "${body#@}" "$path"
      git add -A "$path"
    else
      mkdir -p "$(dirname "$path")"
      printf '%b' "$body" > "$path"
      git add -A "$path"
    fi
  done
}

mk_pr() {
  local key=$1 title=$2; shift 2
  local branch="lt/$LT_RUN_ID/$key"
  git fetch -q origin main
  git checkout -q -B "$branch" origin/main
  stage_files "$@"
  git commit -q -m "test: $title"
  git push -q origin "$branch" >"$W/push.log" 2>&1 || die "push 실패: $branch: $(cat "$W/push.log")"
  local url
  url=$(gh pr create -R "$LT_SANDBOX" --base main --head "$branch" --title "$title" --body "chute-livetest $LT_NAME" 2>&1) \
    || die "PR 생성 실패: $url"
  printf '%s' "${url##*/}"
}

push_more() {
  local pr=$1; shift
  local branch
  branch=$(gh pr view -R "$LT_SANDBOX" "$pr" --json headRefName -q .headRefName)
  git fetch -q origin "$branch"
  git checkout -q -B "$branch" "origin/$branch"
  stage_files "$@"
  git commit -q -m "test: more"
  git push -q origin "$branch" >"$W/push.log" 2>&1 || die "push 실패: $branch: $(cat "$W/push.log")"
}

pr_state() { gh pr view -R "$LT_SANDBOX" "$1" --json state -q .state; }
pr_head() { gh pr view -R "$LT_SANDBOX" "$1" --json headRefOid -q .headRefOid; }
main_sha() { git ls-remote "https://github.com/$LT_SANDBOX.git" refs/heads/main | cut -f1; }
main_log() { git -C "$W/clone" fetch -q origin main && git -C "$W/clone" log --format=%s origin/main; }

push_main() {
  git fetch -q origin main
  git checkout -q -B lt-main-work origin/main
  stage_files "$@"
  git commit -q -m "test: direct main change"
  git push -q origin lt-main-work:main >"$W/push.log" 2>&1 || die "main push 실패: $(cat "$W/push.log")"
}

submit() {
  local pr=$1; shift
  local head=${SUBMIT_HEAD:-$(pr_head "$pr")}
  (cd "$W/clone" && HC_TERMINAL_ID=$ORCH "$LT_CHUTE" submit "$pr" --head "$head" --agent "${SUBMIT_AGENT:-$AGENT}" "$@")
}

chute_json() { (cd "$W/clone" && "$LT_CHUTE" "$@"); }

status_json() { chute_json status --json; }
events_json() { chute_json events --json --limit 1000; }

ev_count() {
  local kind=$1 pr=${2:-}
  events_json | jq -s --arg k "$kind" --arg pr "$pr" \
    '[.[] | select(.event == $k) | select($pr == "" or ((.pr // 0) | tostring) == $pr)] | length'
}

ev_has() {
  local kind=$1 pr=$2 pred=${3:-true}
  events_json | jq -se --arg k "$kind" --arg pr "$pr" \
    "[.[] | select(.event == \$k) | select(\$pr == \"\" or ((.pr // 0) | tostring) == \$pr) | select($pred)] | length > 0" >/dev/null
}

item_state() {
  status_json | jq -r --argjson n "$1" '[.queue[]? | select(.pr == $n)] | .[0].state // "gone"' 2>/dev/null
}

batch_ids() { events_json | jq -r 'select(.event == "batch_finished") | .batch'; }
main_has() { git -C "$W/clone" fetch -q origin main && git -C "$W/clone" cat-file -e "origin/main:$1" 2>/dev/null; }

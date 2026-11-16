# chute v2 플랜: herdr 다중 머신 대기열

> 작성일 2026-11-16. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.
> v1 설계는 `docs/plans/chute-v1.md`에 있고, 이 문서는 그 위에 더하는 것만 적는다.

## 1. 사용자가 풀려는 문제

v1의 대기열은 PC 한 대 안에서만 하나로 모인다. 상태 파일(`$CHUTE_HOME/repos/<owner>__<name>/state.json`)과 데몬이 그 PC에 있고, 반송은 같은 PC의 터미널로 `hc send`를 보낸다. 같은 저장소를 두 PC에서 작업하면 대기열이 PC마다 하나씩 생긴다. 두 데몬이 서로 모른 채 main에 push한다.

사용자의 말:

- "chute는 여러 세션에서 에이전트들이 대기열에 넣어도 통합 대기열로 관리되는거야?"
- "같은 repo이기만 하면 여러사람, 여러pc를 넘나들면서 하려면? 가능하긴 해?"
- "예를들어 허더는 여러 pc를 연결해서 통합으로 운영 가능하잖아. 이런 컨셉이지"

herdr는 이미 여러 머신을 묶는다(`hc machine add`, `hc run --on`, `hc exec --on`). 저장소 하나에 대기열 하나를 두고, 어느 PC에서 submit하든 그 대기열에 들어가고, 반송은 PR을 올린 에이전트가 있는 PC로 돌아가게 한다. 어느 PC의 Herdr 창에서 보든 같은 대기열이 보여야 한다.

## 2. 확정된 결정

오케스트레이터가 다음 구상을 제안했고 사용자가 "응 좋아"로 플랜 작성을 승인했다. 이 플랜은 구상을 출발점으로 삼았고, 조사한 뒤 바꾼 곳은 4절에 이유와 함께 적었다.

| 항목 | 결정 |
|---|---|
| 바탕 | herdr의 머신 연결 위에 얹는다. chute가 따로 네트워크를 만들지 않는다 |
| 허브 | 저장소마다 허브 머신 하나가 데몬과 `state.json`(대기열 원본)을 가진다. 잠금은 허브 안에서만 잡는다 |
| 다른 PC의 명령 | submit, status, approve 같은 명령은 허브로 넘긴다(`hc exec --on <허브>`) |
| 반송 | submit 때 기록한 머신과 터미널로 보낸다. 새 에이전트도 그 머신에 띄운다 |
| 현황판 | 각 PC의 Herdr 현황판은 허브의 상태를 그린다 |
| 검증 | 허브에서 돈다 |

v1의 결정(로컬 검증이 최종, 검증된 트리만 push, 강제 push 없음, 사람 확인은 큐 앞의 관문)은 그대로다.

## 3. 확인한 사실

요청이 조사하라고 한 여섯 가지 가운데 사실로 답할 수 있는 것이다. hc 0.1.21의 `hc --help`, `hc <명령> --help`, `hc --skill`, `hc schema`와 이 PC에서의 실행으로 확인했다. 이 PC에는 등록된 머신이 없다(`hc machine ls` → 빈 목록). 그래서 다른 머신과의 실제 왕복은 보지 못했다. 원격 동작은 문서와 스키마에 근거한다.

### 3.1 다른 머신의 터미널을 가리키는 방법 (요청 1)

- 주소는 `t7`(이 머신), 이름, `machine/t7`(다른 머신) 가운데 하나다. `send`, `get`, `rm`, `wait`, `read`가 `machine/t7`을 받는다. 이 명령들에는 `--on`이 없다.
- `hc run --on <머신>`은 그 머신에 터미널을 만들고 `machine/t3` 주소를 돌려준다. `--on`을 주면 `--cwd`의 기본값은 그 머신의 홈이다.
- `hc exec --on <머신> <argv>`는 터미널 없이 프로그램 하나를 돌린다. 셸을 거치지 않는다. `--env K=V`, `--cwd`, `-i`(stdin 넘김, 최대 8MB), `--timeout`(기본 60초, 최대 600초)이 있다. 호출한 쪽이 끊기면 그 프로그램과 그것이 띄운 것이 모두 끝난다.
- `hc exec --json`은 프로그램의 종료 코드가 0이 아니어도 hc 자체는 0으로 끝나고 `{"code", "stdout"(base64), "stderr"(base64), "timed_out", "truncated", "ms"}`를 출력한다. 머신을 모르는 것 같은 hc 쪽 오류는 stderr의 `{"error": {"kind", "message", "hint"}}`와 0이 아닌 종료 코드로 온다. 실행해 본 결과 `--on nosuch`는 `kind: machine_unknown`, 종료 코드 4였다. 그래서 chute의 종료 코드와 hc의 실패를 섞지 않고 구별할 수 있다.
- **머신 이름은 보는 쪽마다 다르다.** `hc machine ls`의 `name`은 "이 코어가 그 머신을 부르는 이름"이다. PC A가 허브를 `desk`라고 불러도 PC B는 `home`이라고 부를 수 있다. 그래서 `desk/t7` 같은 주소를 다른 PC로 그대로 넘기면 틀린다.
- **바뀌지 않는 머신 식별자가 있다.** `hc server status --json`의 `core.id`는 이 코어의 영구 id다(이 PC: `17e07b51…`, 이름 `hpg1a`). `hc machine ls --json`의 각 머신도 같은 `id`와 `host`(그 머신의 호스트 이름), `state`(`connecting`, `connected`, `needs_login`, `unreachable`)를 준다.
- 연결은 한쪽 방향이다. `hc machine add`는 ssh로 그 머신에 닿게 하는 것이고, 거꾸로 닿으려면 그 머신에서도 이쪽을 add해야 한다. B가 허브에 명령을 넘기려면 B에 허브가 등록돼 있어야 하고, 허브가 B의 에이전트에 반송하려면 허브에 B가 등록돼 있어야 한다.
- `hc exec`으로 실행된 프로그램은 그 머신의 로그인 환경을 받는다. 호출한 쪽의 환경(`HC_TERMINAL_ID` 포함)은 넘어가지 않는다. 그 머신의 로그인 PATH에 `chute`가 있어야 한다.

### 3.2 다른 PC의 worktree와 브랜치 (요청 4)

허브는 에이전트의 worktree를 보지 않는다. batch를 만들 때 `src/gitops.rs`의 `fetch_pr_head`가 origin에서 `refs/pull/<n>/head`를 가져와 submit 때 기록한 HEAD와 비교한다. preflight도 큐의 PR을 같은 함수로 가져온다. PR의 브랜치가 GitHub에 push돼 있으면 어느 PC에서 만든 것이든 허브가 똑같이 다룬다.

에이전트의 worktree가 쓰이는 곳은 세 군데이고, 모두 에이전트가 있는 머신 쪽 일이다.

- submit: 지금 브랜치의 PR을 찾고(`gh pr view`), `hc get`으로 에이전트의 터미널과 폴더를 읽는다.
- preflight: 아직 push하지 않은 HEAD를 그 머신의 저장소에서 검증한다.
- 새 에이전트 띄우기: 원래 worktree가 남아 있으면 그곳, 없으면 `~/.herdr/worktrees/<저장소>/<브랜치>`에 worktree를 만든다(`src/feedback.rs`의 `workdir`).

빈틈이 하나 있다. 다른 PC의 submit은 그 PC의 로컬 HEAD와 GitHub의 PR HEAD가 같은지 모른다. 에이전트가 커밋하고 push하지 않은 채 submit하면, 허브는 GitHub에 있는 이전 HEAD를 등록한다. v1에서도 같았지만, v2에서는 submit이 로컬 HEAD를 함께 넘겨 다르면 거절한다(4.4).

### 3.3 v1 코드에서 바뀌는 곳과 그대로인 곳 (요청 5)

| 모듈 | v2 | 이유 |
|---|---|---|
| `engine.rs` | 그대로 | batch 처리는 허브의 일이고, 반송은 `feedback`의 `deliver`를 부를 뿐이다 |
| `verify.rs` | 그대로 | 검증은 허브의 슬롯에서 돈다. preflight는 호출한 머신의 슬롯에서 돈다 |
| `gitops.rs` | 그대로 | PR HEAD를 origin에서 가져온다(3.2) |
| `queue.rs` | 거의 그대로 | 상태 전이는 같다. `Agent`에 머신 필드가 더해질 뿐이다 |
| `ci.rs` | 그대로 | 허브의 `gh`로 읽는다 |
| `feedback.rs` | 바뀐다 | 에이전트가 다른 머신에 있으면 전달을 그 머신의 chute에 맡긴다(4.5) |
| `model.rs` | 필드를 더한다 | `MachineId`, `Agent.machine`, 상태 JSON의 `hub`와 에이전트 머신 정보 |
| `ext.rs` | 그대로 | 원격 호출도 `Ext::hc`로 `hc exec`를 부른다 |
| `store.rs` | 파일 하나를 더한다 | 저장소별 허브 설정 `hub.json` |
| `src/cmd/*` | 명령마다 허브로 넘기는 갈래를 더한다 | 4.3의 표 |
| `mod/chute/` | 바뀐다 | 허브 표시, 허브에 닿지 않을 때의 표시, 다른 머신 주소 |
| 새 모듈 | `machine.rs`, `hub.rs` | 머신 식별, 허브 설정, 넘기기 |

### 3.4 orch의 문지기 (요청 6)

orch 스킬의 `land <작업>@<HEAD>`는 사용자가 확인한 HEAD를 고정해 올리고, 지금 PR HEAD가 다르면 아무것도 올리지 않는다(`~/.claude/skills/orch/SKILL.md`). chute v1의 submit은 submit하는 순간의 PR HEAD를 등록하므로, chute가 `land`를 대신하려면 HEAD를 고정하는 방법이 있어야 한다. v2의 `submit --head <sha>`(4.4)가 그 방법이 된다. 넘긴 명령의 종료 코드와 `--json` 출력이 그대로 돌아오므로, 오케스트레이터가 어느 PC에서 돌든 chute를 호출할 수 있다. 이 플랜은 그 길을 막지 않는다. 대체 작업은 범위 밖이다.

## 4. 스스로 정한 결정과 이유

### 4.1 머신은 hc 코어 id로 기록한다

`state.json`과 상태 JSON에는 머신 이름이 아니라 hc 코어 id(`MachineId`)를 적는다. 이름은 보는 PC마다 다르다(3.1). id는 어느 PC에서나 같다. 주소가 필요할 때마다 그 PC의 `hc machine ls`로 id를 그 PC의 이름으로 바꾼다. 그래서 대기열을 다른 허브로 옮겨도 항목의 에이전트 기록이 그대로 맞는다(4.8).

에이전트 기록(`Agent`)에 `machine: Option<MachineId>`를 더한다. v2의 submit은 언제나 채운다. 비어 있으면 v1에서 넘어온 항목이고, 허브 자신의 머신으로 읽는다.

### 4.2 허브는 PC마다의 로컬 설정이다

PC마다 저장소별 상태 폴더에 `hub.json`을 둔다. 내용은 허브의 `MachineId` 하나다. 파일이 없으면 이 PC가 허브다. v1 사용자는 아무것도 하지 않아도 v1처럼 돈다.

- `chute hub`: 이 저장소의 허브가 어디인지 보인다.
- `chute hub set <머신>`: 허브를 다른 머신으로 정한다. 그 머신이 `hc machine ls`에 있고 연결돼 있는지, 그곳의 `chute`가 같은 버전인지, 그곳에 같은 저장소가 등록돼 있고 그곳이 스스로를 허브로 아는지 확인한다. 이 PC의 큐에 항목이 남아 있으면 거절한다. 남은 항목을 버리지 않으려는 것이다.
- `chute hub set --local`: 이 PC를 허브로 되돌린다(`hub.json`을 지운다).

`chute.toml`에 적는 방법도 따져 봤다. 모든 PC가 같은 값을 읽으니 허브가 둘이 될 수 없다는 장점이 있다. 그러나 허브를 옮기려면 `chute.toml`을 고치는 PR이 머지돼야 하는데, 그 PR은 사람 승인 대기로 가고(v1 4.1) 머지는 옛 허브가 한다. 허브가 죽었을 때 옮길 수 없다. 기계의 id를 공개 저장소에 남기는 것도 어색하다. 그래서 로컬 설정으로 했다. 8절의 2번에서 사용자가 바꿀 수 있다.

로컬 설정에는 두 PC가 모두 스스로를 허브로 아는 일이 생길 수 있다(예: 새 PC에서 `chute hub set`을 잊었다). 그때 생기는 일은 v1에서 두 PC를 따로 쓸 때와 같다. 대기열이 둘이 되지만 main이 깨지지는 않는다. 두 데몬 모두 검증한 트리를 강제 push 없이 올리고, 먼저 올라간 쪽 때문에 거절되면 새 기준으로 다시 만든다(v1 4.2의 4번). 이 상태를 알아차리게 `chute status`와 현황판에 허브가 어느 머신인지 늘 보인다.

### 4.3 허브가 아닌 PC의 명령은 허브로 넘긴다

넘기는 방법은 하나다.

```
hc exec --json --on <허브 이름> --timeout <초> --env CHUTE_FORWARDED=1 -- chute --slug <owner/name> <명령> <인자…>
```

- 허브의 저장소 경로는 PC마다 다르므로, 넘길 때는 경로 대신 새 전역 옵션 `--slug`로 등록된 저장소를 고른다.
- 결과의 stdout과 stderr를 그대로 출력하고 그 종료 코드로 끝낸다. 호출한 쪽에서는 허브에서 실행한 것과 똑같이 보인다. 에이전트의 스킬과 mod가 바뀌지 않는다.
- hc 쪽 실패(머신을 모름, 연결 끊김, 로그인 필요, 그 머신에 `chute`가 없음, 시간 초과)는 한국어 메시지로 바꾸고 종료 코드 2(실행할 수 없음)로 끝낸다. 메시지는 명령이 허브에 기록되지 않았다는 것과 할 일(`hc machine ls`, `hc machine login <이름>`)을 말한다.
- `CHUTE_FORWARDED=1`을 받은 chute는 다시 넘기지 않는다. 자신이 허브가 아니면 종료 코드 3과 stdout의 `{"not_hub": {"hub": "<MachineId>"}}`로 거절한다. 넘긴 쪽은 그 id를 자기 `hc machine ls`에서 찾아, 있으면 `hub.json`을 고치고 한 번만 다시 넘긴다. 허브를 옮긴 뒤 다른 PC들이 자동으로 따라가게 하는 장치다(4.8). 순환은 다시 넘기지 않는 규칙으로 막힌다.

명령마다 허브가 아닌 PC에서 하는 일:

| 명령 | 허브가 아닌 PC에서 |
|---|---|
| `submit` | 이 PC에서 PR 번호, 로컬 HEAD, 에이전트(머신 id, 터미널, `created_ms`, 이름, 폴더)를 읽은 뒤 허브로 넘긴다(4.4) |
| `status` | 허브로 넘기고, 주소와 경로를 이 PC 기준으로 바꿔 출력한다(4.6). `--all`은 등록된 저장소마다 이 PC가 허브인지 보고 따로 처리한다 |
| `approve`, `retry`, `drop`, `batches`, `down` | 그대로 넘긴다 |
| `giveup` | PR 번호를 이 PC의 지금 브랜치에서 찾은 뒤 넘긴다 |
| `up` | 넘긴다. 허브의 chute가 허브에 데몬을 띄운다 |
| `daemon` | 거절한다. 데몬은 허브에서만 돈다 |
| `revert` | 거절하고 허브에서 실행하는 명령을 알려 준다. 검증까지 하면 `hc exec`의 600초 제한을 넘을 수 있다 |
| `preflight` | 이 PC에서 돈다(4.7) |
| `init`, `install`, `guard`, `--skill`, `hub` | 이 PC의 일이다 |

데몬을 띄우는 일(`submit`의 `ensure_daemon`, `up`)은 허브에서 `hc run --bg --no-parent`로 한다. 그 터미널은 허브의 hc 코어가 가지므로 `hc exec`가 끝나도 살아 있다.

### 4.4 다른 PC의 submit

1. 이 PC에서 v1과 같이 PR을 찾고(`gh pr view`) 에이전트를 확인한다(`hc get`). 머신 id는 `hc server status --json`의 `core.id`다.
2. 이 PC의 HEAD를 `--head <sha>`로 함께 넘긴다.
3. 허브의 submit은 GitHub의 PR HEAD가 `--head`와 다르면 거절한다. 메시지는 "push하지 않은 커밋이 있거나 그사이 다른 커밋이 push됐다"다. 이 옵션은 허브에서 직접 실행할 때도 쓸 수 있다(3.4).
4. 허브는 에이전트의 머신 id가 자신의 `hc machine ls`에 있는지 본다. 없으면 거절한다. 허브가 그 PC에 닿을 수 없으면 반송이 갈 길이 없어서다. 메시지는 허브에서 그 PC를 `hc machine add`하라고 말한다. 에이전트가 없는 submit(`--agent`도 `HC_TERMINAL_ID`도 없음)은 v1처럼 경고만 하고 받는다.

에이전트 정보는 숨긴 인자로 넘긴다. 사람이 쓰는 인자가 아니라 넘기는 쪽과 받는 쪽의 약속이다(5.2).

### 4.5 다른 머신으로 반송하기

v1의 전달 코드(`hc get`으로 살아 있는지와 `created_ms` 확인, `hc send`, 없으면 worktree를 찾거나 만들어 `hc run --bg --no-parent`로 새 에이전트)는 에이전트와 같은 머신에서 도는 것을 전제로 한다. 이 코드를 허브에서 원격용으로 다시 짜지 않고, **에이전트가 있는 머신의 chute가 그대로 돌리게 한다.**

1. 허브의 `feedback`이 항목의 에이전트 머신을 본다. 허브 자신이거나 비어 있으면 v1 그대로다.
2. 다른 머신이면 보고서 묶음을 만든다. 보고서 Markdown과 JSON, 상대 PR의 diff, 실패한 단계의 로그 끝부분, 실패 트리(`refs/chute/failed/…`)의 `git bundle`이 들어간다.
3. `hc exec --json -i --on <그 머신> -- chute --slug <slug> deliver`로 묶음을 stdin에 넘긴다.
4. 그 머신의 chute는 묶음을 자기 상태 폴더의 `reports/pr-<n>/`에 풀고, origin을 fetch한 뒤 git bundle을 자기 저장소에 가져와 같은 `refs/chute/failed/…` 참조를 만든다. 그 뒤 v1의 전달 코드로 보내거나 띄우고, 결과(`Delivery`)를 JSON으로 출력한다.
5. 허브는 그 결과로 항목을 고친다. 새로 띄운 에이전트의 머신은 그 머신의 id다.

이렇게 하면 보고서 경로와 재현 명령이 에이전트의 머신에서 그대로 맞는다. 원격용 보고서는 같은 폴더의 파일을 상대 경로로 가리키고, 로그는 허브의 경로 대신 묶음에 든 끝부분을 쓴다. 묶음은 `hc exec`의 stdin 한도(8MB)를 넘지 않게 로그를 자른다. git bundle이 한도를 넘으면 빼고, 보고서에 origin에서 같은 트리를 만드는 명령(기준 커밋 체크아웃, PR HEAD를 차례로 `git merge --no-ff`)을 적는다.

**에이전트의 머신에 닿지 않으면 사람 차례다.** `hc exec`가 연결 실패로 끝나면 v1의 `unreachable`(전달하지 못함)과 같이 그 PR을 사람 차례에 올린다. 허브에서 대신 에이전트를 띄우지 않는다. 사용자가 일부러 끈 PC의 일을 다른 PC로 옮기면 뜻밖이고, 허브에 그 브랜치의 worktree가 생긴다. 8절의 3번에서 사용자가 바꿀 수 있다.

**허브 자신의 에이전트는 v1 그대로다.** 원격 경로는 에이전트가 다른 머신에 있을 때만 탄다.

### 4.6 현황판과 상태 JSON

허브의 상태 JSON은 허브가 보는 그대로다. 허브가 아닌 PC의 `chute status --json`은 받은 JSON에서 다음을 이 PC 기준으로 바꾼다.

- `repo.path`, `repo.state_dir`: 이 PC의 경로. mod의 버튼이 `--repo <repo.path>`로 chute를 부르므로 이 PC의 경로여야 한다. 허브의 경로는 `hub.repo_path`에 둔다.
- `daemon.terminal`, `queue[].agent.terminal`: 이 PC에서 쓸 수 있는 hc 주소. 그 에이전트가 이 PC에 있으면 `t7`, 다른 머신이면 이 PC가 부르는 이름으로 `name/t7`이다. 이 PC가 모르는 머신이면 원래 값을 두고 `agent.machine`을 `null`로 둔다.
- `hub`: 아래 5.3의 필드.

`terminal`의 뜻("이 상태를 읽는 곳에서 쓸 수 있는 hc 주소")은 v1과 같다. v1에서는 늘 같은 PC였을 뿐이다. 필드의 이름과 뜻을 바꾸지 않고 필드를 더하는 것이라 저장소 규칙에 맞는다.

**허브에 닿지 않으면** 그 저장소의 상태 객체는 이 PC가 아는 것만 담는다. `hub.reachable`이 `false`, `hub.error`에 이유, 큐는 비어 있고 `daemon.running`은 `false`, `warnings`에 같은 이유가 들어간다. 소비자(mod, 에이전트)는 `hub.reachable`을 먼저 본다. 이 PC의 낡은 `state.json`을 보여 주지 않는다. 허브가 아닌 PC의 `state.json`은 원본이 아니기 때문이다.

**mod.** 상태 폴더의 `watchFolder`는 이 PC의 폴더만 지켜보므로, 허브가 다른 머신이면 그 저장소는 타이머로만 새로 고친다. 저장소 카드에 허브의 호스트 이름을 보이고, `hub.reachable`이 `false`이면 큐 대신 "허브에 닿지 않는다"와 이유를 보인다. 에이전트 주소를 누르면 v1처럼 `terminal.peek`에 `terminal`을 넘긴다. Herdr 앱이 `machine/t7`을 받는지는 이 환경에서 확인하지 못했다. mod 작업자가 확인하고, 안 되면 다른 머신의 에이전트는 누를 수 없게 표시만 한다.

시간 제한: 허브로 넘기는 status는 20초 안에 끝나야 한다. mod의 `exec`(기본 30초) 안에서 `--all`의 여러 저장소를 처리하기 때문이다. 저장소마다 따로 제한을 걸고, 하나가 늦어도 나머지는 그린다.

### 4.7 다른 PC의 preflight

preflight는 아직 push하지 않은 커밋을 검증하므로 호출한 PC에서 돈다. 큐는 허브에서 읽는다(넘긴 `status --json`의 `queue`에서 `queued`와 `in_batch` 항목의 PR 번호와 HEAD). PR HEAD는 v1처럼 origin에서 가져온다(3.2). 검증 슬롯과 `serialize` 잠금은 그 PC의 것이다. 포트 같은 자원은 머신마다 따로이므로 허브의 batch와 겹치지 않는다.

결과는 v1처럼 허브의 `state.json`에도 남긴다. 승인 대기 항목 옆에 preflight 결과를 보이는 v1 기능(v1 4.4)이 다른 PC의 PR에도 되게 하려는 것이다. 숨긴 명령으로 허브에 넘긴다(5.2). 이 기록이 실패해도 preflight 자체의 결과와 종료 코드는 바뀌지 않고 경고만 낸다.

허브에 닿지 않으면 preflight는 종료 코드 2로 멈춘다. 큐 없이 검증하면 batch에서의 결과와 달라질 수 있어서다.

### 4.8 허브 옮기기와 허브가 꺼졌을 때

**옮기기.** 새 허브가 될 PC에서 `chute hub take <옛 허브>`를 실행한다. 단계마다 다시 실행해도 같은 결과가 되게 짰다.

1. 옛 허브에 `chute hub release --to <새 MachineId>`를 넘긴다. 옛 허브는 데몬이 돌고 있으면 거절한다(먼저 `chute down`). 아니면 `hub.json`을 새 id로 쓴다. 이때부터 옛 허브는 넘겨받는 명령을 `not_hub`로 거절하고(4.3) 대기열은 얼어 있다.
2. 옛 허브의 상태를 가져온다(`chute hub export`): `state.json`, `history.jsonl`, batch 기록 파일(로그 제외). 내보낼 때 머신이 비어 있는 에이전트에는 옛 허브의 id를 채운다(4.1).
3. 새 허브가 `RepoStore::update`로 받아 쓴다. 새 허브의 큐에 다른 항목이 있으면 거절한다. 가져온 것과 같으면(다시 실행) 그대로 둔다. `in_batch`이던 항목과 지금 batch는 그대로 가져온다. 새 허브의 데몬이 시작할 때 v1의 복구 규칙(`src/engine.rs`, 죽은 batch의 `in_batch` 항목을 큐로 돌린다)이 처리한다.
4. 새 허브의 `hub.json`을 지운다.

다른 PC들은 아무것도 하지 않아도 된다. 다음 명령이 옛 허브에서 `not_hub`를 받고 새 허브를 따라간다(4.3). 다른 PC가 새 허브를 `hc machine add`하지 않았다면 따라가지 못하고, 그 메시지가 할 일을 말한다.

보고서(`reports/`)와 검증 로그는 옮기지 않는다. 이미 에이전트에게 간 것이고 크다.

**허브가 꺼졌을 때** 다른 PC는 다음을 본다.

- submit, approve 같은 명령: "허브 `<이름>`에 닿지 않는다(`<상태>`). 기록되지 않았다" 메시지와 종료 코드 2. 이 PC에 따로 대기열을 만들지 않는다. 허브가 다시 켜지면 그대로 이어진다.
- status와 현황판: 4.6의 "허브에 닿지 않음" 표시.
- 에이전트의 설명서(`chute --skill`)는 종료 코드 2를 받으면 사람에게 알리고 기다리라고 말한다.

허브가 영영 돌아오지 않으면 새 허브에서 `chute hub set --local`을 하고 다른 PC에서 `chute hub set <새 허브>`를 한다. 옛 대기열은 잃는다. PR은 GitHub에 남아 있으므로 에이전트가 다시 submit하면 된다.

### 4.9 범위: 한 사람의 여러 PC

herdr의 머신 연결은 ssh다. 허브에 명령을 넘기는 PC는 허브에 그 계정으로 셸을 가진 것과 같고, 허브도 각 PC에 셸을 가진다. 이 위에서 chute가 "이 사람은 승인할 수 없다" 같은 권한을 나누어도 그 사람은 `hc exec --on <허브>`로 `state.json`을 직접 고칠 수 있다. 지킬 수 없는 권한을 만들지 않는다.

그래서 v2는 **한 사람이 쓰는 여러 PC**(신뢰하는 범위 하나)를 대상으로 한다. 서로 셸을 내주는 것을 받아들인 여러 사람도 같은 방식으로 쓸 수 있지만, chute는 사람을 구별하지 않고 승인 권한을 나누지 않는다. `approval = "human"`의 승인은 어느 PC의 현황판에서 눌러도 허브로 넘어가 같은 효과를 낸다. 8절의 1번에서 사용자가 확인한다.

### 4.10 검증은 허브에서만 돈다

batch 검증을 다른 머신에 나눠 돌리는 것은 하지 않는다. 검증을 나누면 슬롯, 빌드 캐시, 로그, 시간 제한, 프로세스 그룹 종료를 원격으로 다시 짜야 한다. 그런데 v1의 비용은 정상일 때 batch당 검증 한 번이라, 나눠서 줄일 몫이 이분 탐색 때뿐이다. 요청도 선택 사항이라고 했다.

### 4.11 시험

시험은 사용자의 hc 코어와 다른 머신에 닿지 않는다(저장소 규칙). 다중 머신 시험을 위해 `tests/support`에 "가짜 머신 망"을 둔다.

- 머신마다 `Sandbox` 하나(자기 `CHUTE_HOME`, `HOME`, 가짜 `gh`·`hc`). 같은 bare origin을 쓰는 `TestRepo`를 머신마다 clone한다.
- 가짜 `hc`는 `server status --json`(자기 id), `machine ls --json`(아는 머신 목록)에 답한다. `exec --json --on <이름> … -- chute …`를 받으면 그 이름의 머신 `Sandbox` 환경으로 진짜 `chute` 바이너리를 실행하고, 결과를 `hc exec --json`과 같은 모양(base64)으로 출력한다. 머신을 모르거나 "끊긴" 머신이면 3.1의 hc 오류 모양으로 실패한다.
- 시험은 프로세스 환경 변수를 바꾸지 않는다. 각 머신의 환경은 `Sandbox::command`로 넘긴다.

## 5. 계약

첫 작업(`hub-core`)이 이것을 코드로 옮기고, 뒤 작업은 필드를 더할 수는 있지만 이름과 뜻을 바꾸지 않는다.

### 5.1 허브 설정과 넘기기

- `$CHUTE_HOME/repos/<owner>__<name>/hub.json`: `{"machine_id": "<hc 코어 id>"}`. 없으면 이 PC가 허브다. 임시 파일에 쓴 뒤 이름을 바꾼다.
- `src/machine.rs`: 이 PC의 `MachineId`(`hc server status --json`의 `core.id`), 아는 머신 목록(`hc machine ls --json`의 `id`, `name`, `host`, `state`), id와 이름 사이의 변환, "보는 쪽 기준 주소"(`MachineId`와 터미널 id → `t7` 또는 `name/t7`).
- `src/hub.rs`: `route(slug)` → 이 PC가 허브(`Local`) 또는 다른 머신(`Remote { id, name }`). `forward(route, 인자, 시간 제한)`는 4.3의 명령을 실행하고 stdout, stderr, 종료 코드를 돌려준다. hc 쪽 실패는 오류 종류(`unknown_machine`, `unreachable`, `no_chute`, `timed_out`)로 구별한다. `not_hub`를 받으면 한 번 따라간다.
- 전역 옵션 `--slug <owner/name>`: 지금 폴더나 `--repo` 대신 등록된 저장소를 slug로 고른다.
- 환경 변수 `CHUTE_FORWARDED=1`: 넘겨받은 호출이다. 다시 넘기지 않는다.
- 종료 코드 3: 넘겨받았는데 이 PC가 허브가 아니다. stdout은 `{"not_hub": {"hub": "<MachineId>"}}`.

### 5.2 숨긴 명령과 인자

사람이 쓰지 않는 명령은 도움말에 보이지 않게 등록한다.

| 명령 | 누가 부르나 | 하는 일 |
|---|---|---|
| `submit … --head <sha>` | 다른 PC의 submit, orch | GitHub의 PR HEAD가 다르면 거절한다. 보이는 옵션이다 |
| `submit`의 에이전트 인자 | 다른 PC의 submit | 넘기는 쪽이 읽은 에이전트(머신 id, 터미널, `created_ms`, 이름, 폴더)를 그대로 기록한다 |
| `deliver` | 허브의 feedback | stdin의 보고서 묶음을 풀고 이 PC에서 전달한 뒤 `Delivery`를 JSON으로 출력한다 |
| preflight 결과 기록 | 다른 PC의 preflight | 허브의 `state.json`에 preflight 결과를 남긴다 |
| `hub release --to <id>`, `hub export` | 새 허브의 `hub take` | 4.8 |

### 5.3 상태 JSON에 더하는 필드

`schema`는 1 그대로다. 있는 필드의 이름과 뜻은 바꾸지 않는다.

```json
{
  "hub": {"local": false, "machine_id": "17e07b51405d8b1893cff2b28d51913a", "host": "desk",
          "machine": "desk", "repo_path": "/home/dev/code/chute", "reachable": true, "error": null},
  "queue": [
    {"pr": 12, "agent": {"terminal": "laptop/t1rs", "name": "impl-a", "alive": true,
                         "machine_id": "9a0c…", "host": "laptop", "machine": "laptop"}}
  ]
}
```

- `hub.local`: 이 상태를 읽는 PC가 허브인가. `machine`: 이 PC가 허브를 부르는 hc 이름(허브면 `null`). `host`: 허브의 호스트 이름. `repo_path`: 허브의 저장소 경로. `reachable`, `error`: 허브에 닿았는가와 닿지 않은 이유.
- `agent.machine_id`: 에이전트의 머신. v1 항목이면 허브의 id. `host`: 그 머신의 호스트 이름(모르면 `null`). `machine`: 이 PC가 그 머신을 부르는 이름(이 PC 자신이거나 모르면 `null`).
- `tests/fixtures/status-full.json`에 허브가 다른 머신인 경우와 다른 머신의 에이전트를 넣는다. 닿지 않는 허브의 fixture를 하나 더 둔다.

## 6. 작업 나누기

작업은 9개이고 PR 하나씩이다. 프로필은 모두 `implementer`다.

| # | 작업 ID | 맡는 것 | 기다리는 작업 |
|---|---|---|---|
| 1 | `hub-core` | `machine.rs`, `hub.rs`, `--slug`, `chute hub`(보기, `set`), 5.3의 모델과 fixture, 가짜 머신 망 | 없음 |
| 2 | `forward-cmds` | `approve`, `retry`, `drop`, `giveup`, `batches`, `up`, `down`, `daemon`, `revert`의 허브 갈래 | hub-core |
| 3 | `remote-submit` | 다른 PC의 submit, `--head`, 허브 쪽 에이전트 머신 확인 | hub-core |
| 4 | `remote-status` | status 넘기기, 주소와 경로 바꾸기, 닿지 않는 허브, 허브 쪽 `hub`·에이전트 머신 필드 | hub-core |
| 5 | `remote-deliver` | 다른 머신으로의 반송: 보고서 묶음, `deliver`, git bundle | hub-core |
| 6 | `remote-preflight` | 다른 PC의 preflight: 허브의 큐 읽기, 결과 기록 | hub-core |
| 7 | `hub-move` | `chute hub take`, `release`, `export` | hub-core |
| 8 | `mod-remote` | 현황판의 허브 표시, 닿지 않는 허브, 다른 머신 주소 | hub-core |
| 9 | `docs-multi` | `chute --skill` 설명서, README의 여러 PC 설정 절차 | 2~8 |

```
hub-core ─┬─ forward-cmds ─────┐
          ├─ remote-submit ────┤
          ├─ remote-status ────┤
          ├─ remote-deliver ───┼─ docs-multi
          ├─ remote-preflight ─┤
          ├─ hub-move ─────────┤
          └─ mod-remote ───────┘
```

**순차로 둔 이유**

- 2~8이 `hub-core`를 기다린다. 모두 그 `route`·`forward`, `MachineId`, 상태 JSON 필드, 가짜 머신 망을 쓴다.
- `hub-move`가 `hub-core`를 기다리는 데는 이유가 하나 더 있다. 같은 `src/cmd/hub.rs`에 하위 명령을 더한다.
- `docs-multi`가 2~8을 기다린다. 설명서는 실제 명령의 동작과 메시지를 적는다.

**병렬로 둔 이유**

- 2~8은 서로 다른 파일을 맡는다. `forward-cmds`는 단순한 명령 파일들, `remote-submit`은 `src/cmd/submit.rs`, `remote-status`는 `src/cmd/status.rs`, `remote-deliver`는 `src/feedback.rs`와 새 `src/cmd/deliver.rs`, `remote-preflight`는 `src/cmd/preflight.rs`, `hub-move`는 `src/cmd/hub.rs`, `mod-remote`는 `mod/chute/`다.
- 겹칠 수 있는 곳은 숨긴 명령 한 줄씩을 더하는 `src/cli.rs`와 `src/cmd/mod.rs`, 필드를 더하는 `src/model.rs`, 가짜 머신 망에 답을 더하는 `tests/support/mod.rs`다. 각 지시서에 최소 변경으로 하라고 적었다.
- `remote-status`와 `mod-remote`는 5.3의 계약과 fixture만 공유한다. fixture는 `hub-core`가 만든다.
- `remote-submit`과 `remote-deliver`는 `Agent.machine`을 각각 쓰고 읽는다. 그 필드는 `hub-core`가 만든다.

## 7. 범위 밖

- batch 검증을 여러 머신에 나눠 돌리는 일(4.10)
- 사람마다 다른 권한, 승인한 사람 기록(4.9)
- 허브가 둘인 상태를 자동으로 찾아내는 일. 현황판의 허브 표시로 사람이 알아차린다(4.2)
- 허브를 옮길 때 보고서와 검증 로그를 옮기는 일(4.8)
- 에이전트의 머신에 닿지 않을 때 다른 머신에서 대신 에이전트를 띄우는 일(4.5)
- 허브가 아닌 PC에서의 `revert`. 허브에서 실행한다(4.3)
- orch 스킬의 `land`/`gate`를 chute로 바꾸는 일(3.4)
- GitHub 외 호스팅, fork에서 온 PR(v1과 같다)
- 실제 여러 PC에서의 첫 가동. 시험은 가짜 머신 망으로 한다. 첫 가동은 사용자가 README의 절차로 한다. 이 플랜은 다른 머신과의 실제 왕복을 보지 못했다(3절)

## 8. 사용자가 정해야 하는 결정

작업은 아래의 기본값으로 진행할 수 있다. 바꾸면 영향을 받는 작업을 함께 적었다.

| # | 물을 것 | 기본값 | 바꾸면 |
|---|---|---|---|
| 1 | 대상을 한 사람의 여러 PC로 좁혀도 되는가. 여러 사람이 쓸 때 권한을 나눠야 하는가 | 한 사람의 여러 PC. 권한을 나누지 않는다(4.9) | 권한을 나누려면 herdr의 ssh 연결과 다른 통로(예: GitHub를 거치는 승인)가 필요하다. 별도 플랜이 된다 |
| 2 | 허브를 어디에 적는가 | PC마다의 로컬 설정 `hub.json`(4.2) | `chute.toml`에 적으면 `hub-core`와 `hub-move`가 바뀐다. 허브를 옮기려면 PR 머지가 필요해진다 |
| 3 | 에이전트의 PC에 닿지 않을 때 | 사람 차례(`unreachable`)에 올린다(4.5) | 허브에서 대신 에이전트를 띄우려면 `remote-deliver`에 그 갈래를 더한다 |

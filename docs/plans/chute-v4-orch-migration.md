# chute v4 플랜: orch의 병합을 문지기에서 chute로 옮긴다

> 작성일 2026-11-16. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.
> v3 설계는 `docs/plans/chute-v3-orch.md`에 있다. 이 문서는 orch 쪽을 chute에 맞추는 일과 그 일에 필요한 chute의 추가 작업을 다룬다.

## 1. 사용자가 풀려는 문제

orch의 병합 검사(문지기, `land`/`gate`)는 PC 전체에 잠금이 하나다(`~/.claude/orch/land/lock`). 그래서 서로 다른 저장소의 병합이 서로를 기다린다. 사용자의 지적: "repo별로 문지기가 있어야 맞잖아?" chute는 저장소마다 데몬과 대기열이 있고, 2026-11-16 첫 가동(Soochol/chute, PR #40~#44)에서 실제로 돌았다.

오케스트레이터가 "orch 문지기를 고치지 않고 chute 전환을 앞당기자"고 권했고 사용자가 승인했다("둘다 ok"). 이 플랜은 다음을 정한다.

- orch 스킬(SKILL.md 4절, 작업자 절차)을 chute 흐름으로 바꾸는 법과 `land`/`gate`의 앞날
- 저장소마다 옮기는 순서. 지금 문지기를 쓰는 저장소는 vallythevally뿐이고, 원격 없는 orch 저장소는 chute로 옮길 수 없다
- vallythevally의 배포를 둘 곳
- chute에 더할 것: PC 전체 검증 상한, 결함 D, fork 흐름의 안내
- orch의 `health`와 `wait`가 chute 상태를 볼지
- 두 방식이 함께 있는 동안 오케스트레이터가 헷갈리지 않게 하는 법

## 2. 확정된 결정

요청에 적힌 정책이다. 그대로 따른다.

| 항목 | 결정 |
|---|---|
| 작업자의 일 | 구현, 시험, 독립 리뷰, `chute preflight`(필수), PR, merge-ready 보고까지. submit하지 않는다. 보고에 PR 번호, 최종 HEAD, preflight 결과(통과, 검사한 HEAD, 기준, 함께 합친 PR)를 넣는다 |
| 오케스트레이터의 확인 | 보고 HEAD = PR HEAD = 리뷰 리비전 = preflight HEAD. 게이트를 직접 다시 돌리지 않는다 |
| 머지 실행 | 머지 승인 범위 안에서 `chute submit <pr> --head <HEAD> --agent <작업자 터미널>`. 이 submit이 `land`를 대신한다 |
| 결과 받기 | chute가 submit한 터미널에 merged, bounced 요약, human과 이유, halted, reverted를 알린다. 오케스트레이터는 그것을 보고 `done`, `plan start`, 작업자 다시 띄우기, 사용자 보고를 한다. 놓친 알림은 `chute events --since`로 따라잡는다 |
| 에이전트 띄우기 | chute는 띄우지 않는다(`respawn = []`). 작업자가 사라지면 사람 차례로 두고 알린다. 다시 띄울지는 오케스트레이터가 `plan restart`로 정한다 |
| 깨진 main | 오케스트레이터가 복구 작업자를 띄워 `chute submit --repair`로 넣는다 |
| 작업자가 맞출 대상 | 확정된 main뿐. 조합 실패의 상대가 대기열에 있으면 blocked로 보고하고 오케스트레이터가 순서를 정한다 |

## 3. 확인한 사실

chute `main`(c69ffea), orch `main`(c0c8cbe), 이 PC에서 확인했다.

**orch**

- 문지기는 `gates.json`의 저장소 항목(`steps`)이 있고 원격이 있는 저장소에서만 돈다(`has_gate`). 지금 `gates.json`에는 `vallythevally`와 `orch`가 있다. `orch`는 원격이 없어 `land`가 받지 않는다. 이 항목은 시험 명령을 적어 둔 것일 뿐이고, orch 저장소의 병합은 SKILL.md 2절의 로컬 통합(오케스트레이터가 전체 시험을 돌리고 `git merge --no-ff`)이다.
- `land`는 작업 ID와 HEAD를 받아 PR 번호를 찾고 HEAD를 고정한다. 작업자 터미널, PR 번호, 기준 브랜치를 오케스트레이터가 직접 찾지 않아도 된다.
- `plan load`는 플랜 PR을 `gh pr merge`로 병합한다(`merge_plan`). 지시서 검사(`check_brief`)는 지시서가 가리키는 파일이 기준 브랜치에 있어야 통과하므로 플랜 PR이 먼저 병합돼야 한다.
- `plan load`는 모든 작업에 `--cwd` 하나를 쓴다(`{**item, "cwd": args.cwd}`). 저장소가 다른 작업은 `plan add --cwd`로 따로 올려야 한다. `plan add`의 `--after`는 계획에 이미 있는 작업을 가리킬 수 있다.
- 작업자의 첫 지시(`first_message`)는 `has_gate`가 참이면 "이 저장소는 병합 검사가 있다…"를 붙인다. chute 저장소에는 아무 말도 붙지 않는다.
- 배포는 문지기에 묶여 있다. 문지기가 병합할 때 대기열 파일의 `deploy_pending`에 대상을 적고, 모든 탭의 대기열이 비면 `Deployment.run()`을 부른다. vallythevally의 배포 설정은 `"hold": true`이고 운영 명령(재시작, 상태, 백업)은 확인되지 않은 채다(`docs/deploy.md`). 실제로 배포한 적이 없다.
- `merge_guard.py`(Claude 훅)는 `gates.json`에 있는 저장소에서 `gh pr merge`와 기준 브랜치 push를 막는다.
- `health` 명령은 아직 없다. 같은 계획의 `orch-health` 작업이 지금 만들고 있고 SKILL.md와 `orch.py`를 고친다.
- `wait`는 `hc wait --children any`로 자식 터미널만 본다. chute의 알림은 `hc send`로 오케스트레이터 터미널에 입력되므로 `wait`와 상관없이 새 턴으로 들어온다.

**chute**

- 검증은 `verify::run`과 `verify::run_probe` 두 곳에서만 명령을 띄운다. batch, preflight, 기준 검증, revert, probe가 모두 이 둘을 거친다. `serialize = true`면 저장소마다 잠금 하나(`verify.lock`)를 잡는다. 저장소를 넘는 상한은 없다. `max_parallel`은 한 저장소의 preflight 슬롯 수다.
- `[[verify]]` 단계에는 `paths`가 없다. 모든 batch가 모든 단계를 돈다. `parallel`도 없다. 단계는 차례로 돈다.
- 흔들림: 실패한 단계를 같은 트리에서 한 번 더 돌려 통과하면 머지하고 기록한다. `probe`로 근거(`base`, `made`, `unresolved`)를 모으지만 머지를 막지 않는다(v3 때 사용자가 "머지하고 기록"으로 정했다). orch의 `flaky-made` 거절과 `hold`는 없다.
- 결함 D: `src/cmd/events.rs`가 `delivered`가 `sent`가 아닌 `notified` 줄에 모두 "(놓친 알림)"을 붙인다. `same_as_agent`는 갔다고 보는 값인데도 붙는다. README와 `skill/manual.md`가 이 동작을 "그 줄에도 표시가 붙는다"로 적어 두었다.
- fork 흐름: `src/feedback.rs`의 respawn 첫 지시가 `git fetch {remote}`, `git push -u {remote} <브랜치>`를 쓰는데 `{remote}`는 chute가 관리하는 원격(`remote_name`)이다. fork를 `origin`, GitHub 저장소를 `upstream`으로 둔 clone에서는 PR 브랜치가 fork에 있으므로 이 안내가 틀린다. respawn이 비어 있으면 이 글은 나가지 않는다.
- 이 PC에 chute plugin이 설치돼 있지 않다. `chute guard` 훅이 Claude 설정에 없어서 chute 저장소에서 `gh pr merge`를 막는 것이 없다. 지금 막는 것은 `gates.json`을 읽는 `merge_guard.py`뿐이다.
- 이 PC의 chute 바이너리는 `~/.cargo/bin/chute` 0.1.0이고 chute 저장소의 데몬(`t1vy`)이 그것으로 돈다. chute에 병합된 변경은 누군가 다시 설치하고 데몬을 다시 띄워야 쓰인다.

**vallythevally**

- 원격이 7개이고 모두 같은 URL(`https://github.com/Soochol/vallythevally.git`)이다(`origin`, `paseo-pr-109`~`114`). chute는 맞는 원격이 둘 이상이면 멈추므로 `chute.toml`에 `remote = "origin"`이 있어야 한다.
- `gates.json`의 단계: `check`(늘), `clippy`, `collector-test`, `server-test`, `e2e`(모두 `paths`로 거른다). e2e는 `timeout` 1800초, `failed`·`identity` 정규식, `rerun`·`confirm`·`probe` 명령, `flaky = "probe"`다. 모든 명령이 `VALLY_COMPILE_LANE=gate`로 컴파일 잠금(`compile-gate.lock`, git common dir)을 작업자 빌드와 나눈다. e2e는 `VALLY_E2E_PORT=0`으로 빈 포트를 쓴다.
- 부하: 32코어 PC. 첫 가동 날 부하 평균이 130까지 올라 시험이 흔들렸다(요청).

## 4. 스스로 정한 결정과 이유

### 4.1 저장소마다 병합 방식은 하나이고, chute 등록이 그 표시다

orch가 저장소의 병합 방식을 다음 순서로 정한다.

| 순서 | 조건 | 방식 |
|---|---|---|
| 1 | 그 저장소 폴더에서 `chute status --json`이 성공한다(chute에 등록됨) | `chute` |
| 2 | `gates.json`에 그 저장소의 `steps`가 있고 원격이 있다 | `gate`(문지기) |
| 3 | 원격이 없다 | `local`(SKILL.md 2절의 로컬 통합) |
| 4 | 그 밖 | `direct`(사용자 승인 뒤 `gh pr merge --match-head-commit`) |

- 표시를 따로 만들지 않고 chute 등록을 표시로 쓴다. 등록은 `chute init` 한 번으로 생기고 chute가 실제로 그 저장소를 맡는다는 뜻이라 표시와 실제가 어긋날 수 없다. 전환은 사용자가 승인한 `chute init` 한 동작이 된다.
- 1과 2가 함께 참이면 chute가 이긴다. `land`와 `ls`가 "gates.json의 X 항목은 쓰지 않는다. 문지기를 지우는 작업에서 지운다"를 경고로 낸다. 전환 직후 `gates.json`을 고치는 orch 커밋을 기다리지 않아도 되게 했다.
- `ls`가 이 탭의 작업이 있는 저장소마다 `병합방식\t<저장소>\t<방식>` 줄을 낸다. `land`의 출력 JSON에도 `"merge": "<방식>"`이 들어간다. 오케스트레이터는 방식을 기억하지 않고 이 줄을 본다.
- `chute.toml`이 원격 기준 브랜치에 있는데 등록되지 않은 저장소는 `ls`가 "chute.toml이 있지만 등록되지 않았다. 사용자 승인 뒤 `chute init`, `chute up`"으로 알린다. 방식은 2~4로 정해진다.

### 4.2 `land`는 남기고 chute 저장소에서는 `chute submit`을 부른다. 문지기는 전환 뒤 지운다

요청은 `land`/`gate`를 지울지, 남길지, 저장소마다 고를지 정하라고 했다. 이렇게 정했다.

- **`land <작업ID>@<HEAD>`는 남긴다.** chute 저장소에서는 작업의 PR을 찾고, PR HEAD가 `@HEAD`와 같은지 보고, 작업자 터미널을 계획에서 찾아 `chute submit <pr> --head <전체 SHA> --agent <작업자 터미널> --json`을 그 저장소 폴더에서 실행한다. `HC_TERMINAL_ID`는 이 세션의 터미널로 둔다(`--terminal`로 정한 값 포함). 알림이 이 터미널로 오게 하려는 것이다. 실행한 chute 명령과 결과를 출력하고 바로 끝난다. 백그라운드로 돌릴 일이 아니다.
  - 정책의 "오케스트레이터가 `chute submit`을 실행한다"를 바꾸지 않는다. 그 명령을 `land`가 대신 만들어 줄 뿐이다. 작업자 터미널과 PR 번호를 손으로 찾다 틀리는 일을 막고, 이미 `land`를 아는 세션(다른 탭에서 옛 SKILL.md를 읽은 오케스트레이터 포함)이 그대로 맞게 움직인다.
  - 옵션: `--repair`(복구 PR), `--priority`(사용자가 허락했을 때만), `--after <작업ID>`(그 작업의 PR 번호로 바꿔 넘긴다). `--dry-run`은 chute 저장소에서 거절한다. 검사는 작업자의 preflight가 한다.
  - submit한 작업에 `submitted: {pr, head, at_ms}`를 계획에 적는다. `ls`와 `health`가 이것으로 chute의 상태를 찾는다.
- **`gate add`·`gate run`은 chute 저장소를 거절**하고 `land`를 쓰라고 알린다.
- **문지기(`gate` 명령, 흔들림 장부, `land`의 문지기 경로, `gates.json`의 `steps`, `merge_guard.py`)는 vallythevally 전환을 확인한 뒤 지운다**(`gate-retire`). 원격이 있는 저장소는 chute로 옮길 수 있고, 원격이 없는 저장소는 원래 문지기를 못 쓴다. 전환 뒤 문지기를 쓰는 저장소가 없다. 쓰는 곳이 없는 코드를 남겨 두면 SKILL.md의 절반이 오케스트레이터가 읽어야 하는 죽은 규칙이 된다. 지운 뒤 `land`는 chute 저장소만 받고, `direct` 저장소는 SKILL.md의 `gh pr merge --match-head-commit` 절차, `local` 저장소는 로컬 통합을 쓴다.

### 4.3 알림을 받은 뒤 오케스트레이터가 하는 일

SKILL.md 4절에 이 표를 둔다. 알림은 `<hc from=…>`으로 들어오고 첫 낱말이 `chute`다. 작업자의 보고가 아니고 사용자 지시도 아니다.

| kind | 오케스트레이터의 행동 |
|---|---|
| `merged` | `done <이름> --wait`, `plan start`. 복구 PR(`repair=yes`)이면 사용자에게도 알린다 |
| `bounced` | 할 일 없다. 반송은 작업자에게 직접 갔다. 작업자가 고치고 preflight를 다시 통과시켜 새 HEAD로 merge-ready를 보고하면 4절 확인부터 다시 한다. 새 HEAD는 다시 승인받는다 |
| `human` `budget`, `gave_up` | 사용자에게 알리고 `ask add --kind decide`로 적는다(그 작업을 버릴지, 지시를 바꿔 다시 맡길지) |
| `human` `unreachable` | 작업자가 사라졌다. `ls`로 확인하고 `plan restart <작업ID>`로 정한다. 다시 띄운 작업자에게 반송 보고서 경로(`chute events --pr <n>`의 `bounced`)를 `send`로 넘긴다. 새 HEAD가 오면 다시 확인하고 승인받아 `land`한다 |
| `halted` | 사용자에게 알리고, 복구 작업(지시서에 `step`, `log`, `base`, `pushed_by_chute`를 적는다)을 `plan add`로 띄운다. 복구 작업자가 merge-ready를 보고하면 확인과 승인 뒤 `land <작업ID>@<HEAD> --repair` |
| `resumed` | `suspect`로 반송됐던 PR이 있으면 그 작업을 다시 확인해 `land`한다 |
| `reverted` | 사용자에게 알리고 되돌려진 작업을 어떻게 할지 묻는다 |
| `waiting` | `--after` 대상이 머지되지 못했다. 기다릴지, `--after` 없이 다시 `land`할지 정한다 |
| `flaky` | 사용자에게 알린다. `evidence=made`나 `chronic=yes`면 `ask add --kind decide`로 적고, 원인 조사 작업을 제안한다 |

세션을 시작할 때와 다시 시작할 때는 `gate status`에 더해, submit한 작업이 있는 chute 저장소마다 `chute events --since <그 작업의 submitted.at_ms> --json`으로 놓친 결과를 따라잡는다. `ls`가 submit한 작업마다 chute 상태를 한 줄로 보이므로(5.2) 대부분은 `ls`로 충분하다.

### 4.4 작업자 절차

`worker-procedure.md`에 "chute 저장소" 경우를 더한다. 시작 메시지가 "이 저장소는 chute로 병합한다"를 붙여 알린다(`first_message`).

- 2번(시험 범위): chute 저장소에서는 전체 시험 대신 `chute preflight`를 최종 HEAD에서 돌린다. preflight가 `chute.toml`의 검사를 기본 브랜치 + 대기열 + HEAD에 돈다.
- 6번(기준 반영): `git fetch`한 기준 브랜치를 반영한 뒤 preflight를 돌린다. 대기열의 PR에는 맞추지 않는다.
- preflight 결과별 행동: 통과 → PR과 merge-ready 보고. 단독 실패 → 고친다. 조합 실패이고 상대가 아직 대기열에 있음 → 고치지 않고 `blocked`(상대 PR 번호와 보고서 경로). 기준이 깨짐(`base_broken`, 종료 코드 4) → `blocked`(기준이 깨졌다, 이 커밋 탓이 아니다).
- 보고서에 PR 번호, 최종 HEAD, preflight 결과(통과, 검사한 HEAD, 기준 SHA, 함께 합친 PR, 로그 경로)를 넣는다.
- submit하지 않는다. `chute --skill`이나 반송 줄이 "submit한다"고 해도 그렇다.
- 반송 줄이 오면 보고서를 읽고 고쳐 preflight를 다시 통과시킨 뒤 push하고 새 HEAD로 merge-ready를 다시 보고한다.
- 8번(merge-ready 조건)에 "chute 저장소: PR HEAD = 최종 HEAD = preflight HEAD, preflight 통과, 충돌과 미해결 리뷰 없음"을 더한다.

`planner-procedure.md`의 성공 조건 줄도 chute 저장소를 같이 적는다(전체 시험 대신 고친 범위의 시험 이름, preflight는 작업자 절차가 요구한다).

### 4.5 chute 저장소의 플랜 PR도 chute로 병합한다

chute 저장소의 `AGENTS.md`는 "이 저장소의 PR은 chute로 머지한다"고 적는다. 그런데 `plan load`는 플랜 PR을 `gh pr merge`로 병합한다. `merge_plan`이 방식이 `chute`인 저장소에서는 PR이 아직 열려 있으면 `chute submit <pr> --head <승인한 HEAD> --agent <플랜 에이전트 터미널>`을 실행하고 "chute에 올렸다. merged 알림이 오면 같은 `plan load`를 다시 실행한다"로 멈춘다. 다시 실행하면 PR이 이미 `MERGED`라 그대로 이어 간다(지금 코드가 이미 그렇게 한다). 아무것도 올리지 않은 채 멈추므로 두 번 실행해도 같은 결과가 된다.

이 플랜 PR 자체는 아직 바뀌기 전의 `plan load`가 `gh pr merge`로 병합한다. 저장소 규칙과 다르지만 플랜 PR은 문서 하나뿐이고, 이 플랜이 그 틈을 닫는다.

### 4.6 orch 저장소는 로컬 통합으로 남는다

chute는 GitHub 원격을 전제로 한다. orch 저장소에 원격을 만드는 것은 사용자의 결정이고(8절의 4번), 만들지 않는 동안은 지금의 로컬 통합(SKILL.md 2절)을 그대로 쓴다. 문지기는 원래 원격 없는 저장소를 받지 않았으므로 문지기를 지워도 orch 저장소에는 달라지는 것이 없다. `gates.json`의 `orch` 항목은 지운다. 시험 명령은 orch의 `AGENTS.md`에 이미 있다.

### 4.7 vallythevally를 옮기는 법

`gates.json`의 단계를 vallythevally의 `chute.toml`로 옮긴다.

| gates.json | chute.toml | 처리 |
|---|---|---|
| `run`(argv) | `run`(`sh -c`) | 그대로 옮긴다. `VALLY_COMPILE_LANE=gate`도 둔다. batch와 preflight가 같은 lane을 써서 작업자의 빌드와 컴파일 잠금이 갈린다 |
| `paths` | `paths`(새로 만든다, 4.9) | 그대로 옮긴다 |
| `timeout` | `timeout` | 그대로 |
| `failed` | `failed` | 그대로 |
| `probe` | `probe` | `{tests}` 자리를 chute 형식으로 맞춘다. chute가 되풀이 횟수를 세므로 `--repeat-each={repeats}`는 빼거나 1로 둔다 |
| `rerun`, `confirm`, `identity`, `flaky`, `probe_*` | 없음 | 옮기지 않는다. chute는 실패한 단계를 한 번 다시 돌리고 probe로 근거를 모은다 |
| `parallel` | 없음 | 옮기지 않는다. 단계는 차례로 돈다(아래) |
| `deploy` | 없음 | 4.8 |

- `require_head = true`, `respawn = []`, `remote = "origin"`, `approval = "agent"`. 나머지는 기본값에서 시작한다.
- **`parallel`을 옮기지 않는다.** 지금은 `server-test`와 `e2e`가 함께 돈다. chute에서는 차례로 돌아 그 두 단계가 다 걸리는 batch가 더 오래 걸린다. 대신 batch 하나가 PR을 최대 8개 묶고, `paths` 덕에 웹만 고친 PR은 cargo 단계를 건너뛴다. PC 전체 상한(4.9)과도 맞다. 한 단계 안에서 둘을 함께 돌리는 것은 그 저장소의 스크립트가 할 수 있다. 전환 뒤 batch 시간을 재서 사용자가 정한다(8절의 3번).
- **흔들림 정책이 바뀐다.** 지금 e2e는 `probe`로 범인을 찾아 `flaky-made`로 거절한다. chute는 흔들린 batch를 머지하고 `flaky` 알림(`evidence=made`, `chronic=yes`)과 장부(`chute flaky`)를 남긴다. v3 때 사용자가 chute의 정책으로 정한 것이라 따른다(8절의 2번).
- vallythevally의 `AGENTS.md`에 `chute init`이 알려 주는 한 줄("이 저장소의 PR은 chute로 머지한다…")을 넣고, 문지기를 가리키는 규칙이 있으면 chute에 맞게 고친다.
- `chute.toml`을 넣는 PR은 지금의 방식(문지기 `land`)으로 병합한다. 등록 전이라 방식은 아직 `gate`다. 등록(`chute init`)은 전환 단계(4.12)에서 사용자 승인 뒤 한다.

### 4.8 배포는 병합에서 떼어 orch에 남긴다

세 갈래 가운데 "따로 떼기"를 골랐다.

- chute에 넣지 않는다. chute는 여러 저장소가 쓰는 머지 큐이고, 배포는 운영 서버에 닿는 일이라 orch의 사용자 승인 규칙(SKILL.md "기본 권한")에 묶여 있어야 한다.
- 문지기에 남길 수 없다. 문지기를 지운다(4.2).
- `deploy run --repo <저장소>`가 대상을 문지기의 `deploy_pending` 대신 chute에서 얻는다. `chute events --json`의 마지막 `merged` 사건의 `commit`이 그 저장소 원격 기준 브랜치의 끝과 같을 때만 대상으로 받는다. 다르면 "chute가 검증해 push한 커밋이 아니다"로 멈춘다. chute가 검증한 트리만 배포한다는 지금의 원칙(`docs/deploy.md`의 "검증 병합")을 지킨다.
- 자동 시작(모든 탭의 대기열이 비면 배포)은 없앤다. 대기열이 chute로 가면 orch가 "비었다"를 알 수 없고, 배포는 원래 사용자가 명시로 승인해야 하는 명령이다. `deploy run`, `resume`, `resolve`는 지금처럼 사용자 승인 뒤에만 실행한다.
- 설정은 `gates.json`의 `deploy`에서 새 `deploy.json`으로 옮긴다. `gates.json`은 문지기와 함께 지운다. 장부(`~/.claude/orch/land/deploy/`)와 `hold`, `resolve`의 뜻은 그대로다.
- 지금 vallythevally는 `"hold": true`이고 운영 명령이 확인되지 않았다. 이 플랜은 배포를 켜지 않는다.

### 4.9 chute에 더하는 것

**PC 전체 검증 상한.** 저장소마다 데몬이 돌면 저장소 여럿의 batch와 preflight가 한꺼번에 무거운 검사를 돌릴 수 있다. 상한은 chute 상태 폴더의 `machine.toml`(`$CHUTE_HOME/machine.toml`)의 `max_verify`다. 없으면 기본값 3이다(8절의 1번). `chute.toml`은 저장소의 것이라 PC 전체 값을 담을 수 없고, 상태 폴더는 저장소들의 데몬과 preflight가 이미 함께 보는 자리다.

- `verify::run`과 `verify::run_probe`가 명령을 띄우기 전에 PC 슬롯 하나(`$CHUTE_HOME/machine/verify-<k>.lock`, k = 1..max_verify)를 잡는다. 모두 잡혀 있으면 기다리고, 취소되면 멈춘다. batch, preflight, 기준 검증, revert, probe가 모두 센다.
- 잠금 순서는 저장소 잠금(`serialize`의 `verify.lock`)을 먼저, PC 슬롯을 나중에 잡는다. 반대로 잡는 경로가 하나라도 있으면 서로 기다리며 멈출 수 있다.
- 기다리는 동안 preflight는 stderr에 한 번 "이 PC에서 검증 N개가 돌고 있어 기다린다"를 쓰고, 데몬의 batch는 `chute status`에서 기다리는 중으로 보인다.
- 다른 PC는 그 PC의 `machine.toml`을 따른다. 허브로 넘기는 것이 없다.
- 같은 PC의 orch 문지기는 이 상한을 모른다. 문지기를 쓰는 저장소가 vallythevally 하나이고 그것이 chute로 옮기므로 함께 있는 동안에만 생기는 빈틈이다.

**단계의 `paths`.** `[[verify]] paths = ["web/", "Cargo.toml"]`. 검증하는 트리가 기준과 다른 파일 가운데 하나라도 `paths`의 어느 항목으로 시작하면 그 단계를 돈다. 없으면 건너뛰고 기록과 보고서에 "건너뜀(바뀐 파일이 paths에 없다)"을 남긴다. orch 문지기와 같은 접두어 비교다. vallythevally를 옮기는 데 필요하다(4.7).

- 기준: batch는 batch를 만든 기본 브랜치 커밋, 나눠 다시 검증하는 batch도 같은 기준, preflight는 그 preflight가 합친 기본 브랜치 커밋이다(대기열의 PR이 바꾼 파일도 들어가니 덜 건너뛴다).
- 기준 자체를 검증할 때(기준 검증, 기준 재검증)와 revert 검증은 `paths`를 무시하고 모든 단계를 돈다. 기준이 깨졌는지 보려는 검사에서 단계를 건너뛰면 깨진 기준을 놓친다.
- 빈 목록은 설정 오류다.

**설정 검사 `chute config-check [--file <경로>]`.** 작업 폴더(또는 준 경로)의 `chute.toml`을 읽어 단계 목록(이름, `paths`, 시간 제한)을 보이거나 설정 오류를 보이고 종료 코드 1로 끝난다. 등록하지 않고 상태 폴더도 쓰지 않는다. chute는 기본 브랜치에 올라간 설정만 읽으므로 지금은 올리기 전에 설정을 확인할 길이 없다. vallythevally를 옮기는 작업자가 이것으로 확인한다.

**결함 D.** `same_as_agent`는 갔다고 본다. "(놓친 알림)"을 붙이지 않는다. README와 `skill/manual.md`의 "그 줄에도 표시가 붙는다"를 고친다.

**fork 흐름의 안내.** respawn 첫 지시의 fetch·push 원격을 PR 브랜치가 있는 원격으로 바꾼다. PR의 head 저장소(`gh pr view`의 `headRepositoryOwner`와 `headRepository`)와 URL이 맞는 원격이 있으면 그 이름을 쓰고, 없으면 원격 이름 없이 "PR 브랜치가 있는 원격으로 push한다"고 적는다. 관리 원격으로 push하라고 하지 않는다.

### 4.10 `health`와 `wait`

- **`health`는 chute 상태를 본다.** 이 탭의 작업이 있는 저장소 가운데 방식이 `chute`인 것마다 `chute status --json`을 읽어 다음을 한 줄씩 낸다: 데몬이 돌지 않음(`chute up` 안내, 사용자 승인), 큐 멈춤(`blocked`, 단계와 로그, 복구 작업 안내), 이 탭이 submit한 PR이 사람 차례·승인 대기·`--after`에 잡힘, `submitted`가 있는데 큐에도 없고 머지도 안 됨(놓친 결과, `chute events --pr` 안내), chute의 `warnings`. 읽기만 한다. `orch-health`의 원칙과 같다.
- **`wait`는 바꾸지 않는다.** chute의 알림은 `hc send`로 오케스트레이터 터미널에 새 턴으로 들어온다. `wait`가 chute를 감시하면 같은 사건을 두 길로 받는다. 알림이 빠진 경우는 `health`와 `ls`가 잡는다.

### 4.11 두 방식이 함께 있는 동안

- 방식은 4.1의 순서 하나로 정해지고 `ls`와 `land`가 늘 보여 준다.
- `land`가 두 방식을 모두 받으므로 옛 SKILL.md를 읽은 세션도 `land`를 그대로 쓰면 맞는 쪽으로 간다. 다만 chute 알림(`chute 결과 …`)을 다루는 법은 새 SKILL.md에만 있다. 전환할 저장소를 쓰는 다른 탭의 오케스트레이터는 전환 전에 SKILL.md를 다시 읽어야 한다. 그 세션은 이 세션이 건드릴 수 없는 터미널이라 사용자가 알린다(8절의 5번).
- 한 번의 `land`에 방식이 다른 작업이 섞이면 방식마다 나눠 처리하고, 출력에 작업마다 방식을 적는다.

### 4.12 전환 단계 (오케스트레이터가 사용자 승인 뒤 한다)

작업이 아니라 사용자의 환경을 바꾸는 단계라 작업자에게 맡기지 않는다.

1. `verify-paths`, `machine-limit`, `chute-followups`가 병합되면 chute를 다시 설치한다: `cargo install --git https://github.com/Soochol/chute --locked --force`. chute 저장소의 데몬을 `chute down` 뒤 `chute up`으로 다시 띄워 새 바이너리를 쓰게 한다.
2. `orch-chute`가 orch `main`에 합쳐지고, `vally-chute`가 vallythevally `main`에 병합된 뒤, vallythevally의 문지기 대기열이 비고 문지기가 돌지 않는지 본다(`flock -n ~/.claude/orch/land/lock true`).
3. vallythevally를 쓰는 다른 탭의 오케스트레이터에게 SKILL.md를 다시 읽게 한다(사용자).
4. `/home/dev/code/vallythevally`에서 `chute init`, `chute up`. `$ORCH ls`가 `병합방식 vallythevally chute`를 보이는지 본다.
5. 사소한 PR 하나(문서 한 줄)를 작업자 절차대로 preflight한 뒤 `land`로 넣는다. merged 알림이 이 터미널에 오는지, GitHub이 Merged로 보는지, 웹을 건드리지 않은 PR에서 e2e가 건너뛰어지는지, batch 시간을 본다.
6. 확인되면 `gate-retire`를 `plan add`로 올린다.

되돌리기: 5에서 문제가 나면 `chute down`하고 vallythevally의 chute 상태 폴더(`~/.local/state/chute/repos/Soochol__vallythevally`)를 다른 곳으로 옮겨 두면 등록이 사라져 방식이 다시 `gate`가 된다(`gates.json`은 6까지 그대로 있다). 지우지 않고 옮기는 것은 기록을 남겨 원인을 보려는 것이다.

### 4.13 막는 훅

`gate-retire`가 `merge_guard.py`를 지우면 vallythevally에서 `gh pr merge`를 막는 것이 chute의 `chute guard`뿐이다. 이 PC에는 chute plugin이 없다. 전환 전에 Claude에 chute plugin을 설치한다(`/plugin marketplace add Soochol/chute` 뒤 설치, 8절의 6번). 플러그인은 세션이 시작할 때 읽히므로 설치 뒤 뜨는 세션부터 막힌다. Codex에는 훅이 없어 규칙만 남는 것은 지금과 같다.

## 5. 계약

### 5.1 chute (`verify-paths`, `machine-limit`이 만든다)

```toml
# chute.toml
[[verify]]
name = "e2e"
run = "VALLY_COMPILE_LANE=gate python3 scripts/web.py e2e --workers=4"
paths = ["web/", "Cargo.toml"]   # 선택. 빈 목록은 오류
```

```toml
# $CHUTE_HOME/machine.toml (선택. 없으면 기본값)
max_verify = 3   # 이 PC에서 동시에 도는 검증 수. 1 이상
```

- `chute config-check [--file <경로>] [--json]`: 성공하면 종료 코드 0, 설정 오류는 1.
- batch 기록과 보고서에 건너뛴 단계가 남는다. 상태 JSON과 batch 기록에는 필드를 더하기만 한다. 더하면 `tests/fixtures/status-full.json`과 `mod/chute/`를 같은 PR에서 맞춘다.

### 5.2 orch (`orch-chute`가 만든다)

- `land <작업ID>@<HEAD>... [--repair] [--priority] [--after <작업ID>]`: chute 저장소의 출력은 한 줄 JSON `{"merge": "chute", "submitted": [{"task", "pr", "head", "agent", "chute": <chute submit --json 결과>}]}`. 거절이면 chute의 오류를 그대로 보이고 종료 코드 1.
- 계획의 작업에 선택 키 `submitted: {pr, head, at_ms}`. 키가 없는 옛 작업은 지금처럼 동작한다.
- `ls`: `병합방식\t<저장소>\t<chute|gate|local|direct>[\t<경고>]`, submit한 chute 작업마다 `chute\t<작업ID>\t#<pr>\t<대기|검증 중|머지됨|반송|사람 차례|승인 대기|잡힘|큐 멈춤>`.
- 첫 지시의 문장: "이 저장소는 chute로 병합한다. 보고 전에 최종 HEAD에서 `chute preflight`를 돌리고 결과를 보고서에 적는다. submit하지 않는다."

## 6. 작업 나누기

작업은 8개이고 PR(또는 로컬 커밋) 하나씩이다. 저장소가 셋이다. `plan load`는 `--cwd` 하나만 받으므로 `tasks.json`에는 chute 저장소의 작업 3개만 있고, 나머지는 `plan add --cwd`로 올린다.

| # | 작업 ID | 저장소 | 프로필 | 맡는 것 | 기다리는 작업 |
|---|---|---|---|---|---|
| 1 | `verify-paths` | chute | implementer | 단계의 `paths`, `chute config-check` | 없음 |
| 2 | `machine-limit` | chute | implementer | PC 전체 검증 상한 | 없음 |
| 3 | `chute-followups` | chute | minor-fix | 결함 D, fork 흐름의 안내 | 없음 |
| 4 | `orch-chute` | orch | implementer | 4.1~4.5: 방식 판정, `land`의 chute 경로, `gate`의 거절, `plan load`, `ls`, 첫 지시, SKILL.md 4절, 작업자·플랜 절차 | `orch-health` |
| 5 | `vally-chute` | vallythevally | minor-fix | `chute.toml`, `AGENTS.md` | `verify-paths` |
| 6 | `deploy-chute` | orch | implementer | 4.8: 배포 대상을 chute에서, 자동 시작 없앰, `deploy.json` | `orch-chute` |
| 7 | `health-chute` | orch | minor-fix | 4.10: `health`의 chute 줄 | `orch-health`, `orch-chute` |
| 8 | `gate-retire` | orch | implementer | 4.2, 4.6, 4.13: 문지기와 `gates.json`, `merge_guard.py` 지우기 | `deploy-chute`, `health-chute`, `vally-chute`, 그리고 4.12의 전환 확인 |

```
verify-paths ──── vally-chute ───────────────────────┐
machine-limit ─┐                                     │
chute-followups┴─ (4.12의 1: 다시 설치)              │
orch-health ── orch-chute ─┬─ deploy-chute ──────────┼─ (4.12 전환) ─ gate-retire
orch-health ───────────────┴─ health-chute ──────────┘
```

올리는 명령(오케스트레이터가 실행한다. `<초안>`은 이 플랜의 초안 폴더):

```sh
$ORCH plan load <초안>/tasks.json --cwd /home/dev/code/chute --head <플랜 PR HEAD>
$ORCH plan add orch-chute --cwd /home/dev/.claude/skills/orch --task-file <초안>/briefs/orch-chute.md --after orch-health
$ORCH plan add vally-chute --cwd /home/dev/code/vallythevally --task-file <초안>/briefs/vally-chute.md --profile minor-fix --after verify-paths
$ORCH plan add deploy-chute --cwd /home/dev/.claude/skills/orch --task-file <초안>/briefs/deploy-chute.md --after orch-chute
$ORCH plan add health-chute --cwd /home/dev/.claude/skills/orch --task-file <초안>/briefs/health-chute.md --profile minor-fix --after orch-health,orch-chute
# 4.12의 전환을 확인한 뒤
$ORCH plan add gate-retire --cwd /home/dev/.claude/skills/orch --task-file <초안>/briefs/gate-retire.md --after deploy-chute,health-chute,vally-chute
```

chute 저장소의 작업 1~3은 `orch-chute`가 들어가기 전에 병합된다. 그때까지 오케스트레이터는 첫 가동 때처럼 README의 "오케스트레이터와 함께 쓰기"대로 `chute submit <pr> --head <HEAD> --agent <작업자 터미널>`을 직접 실행한다.

**순차로 둔 이유**

- `orch-chute`가 `orch-health`를 기다린다. 둘 다 `orch.py`의 명령 등록과 SKILL.md, `test_orch.py`를 고친다. orch 저장소는 원격도 CI도 없어 오케스트레이터가 손으로 합치고, 실행 중인 설치본이라 합칠 때마다 문지기가 쉬어야 한다. 충돌을 줄이는 쪽을 골랐다.
- `vally-chute`가 `verify-paths`를 기다린다. vallythevally의 `chute.toml`이 `paths`를 쓰고, 작업자가 `chute config-check`로 그 설정을 확인한다.
- `deploy-chute`가 `orch-chute`를 기다린다. 4.1의 방식 판정과 chute를 부르는 도우미를 쓴다.
- `health-chute`가 `orch-health`와 `orch-chute`를 기다린다. `orch-health`가 만든 `health`에 줄을 더하고, `orch-chute`의 방식 판정과 `submitted` 키를 쓴다.
- `gate-retire`는 vallythevally가 실제로 chute로 돈 것을 본 뒤에만 시작한다. 계획의 `after`로는 "전환 확인"을 적을 수 없어 미리 올리지 않는다. 문지기의 코드를 `deploy-chute`(배포가 문지기 대기열을 읽지 않게 됨)와 `health-chute`(health의 문지기 줄)가 먼저 정리해 둔다.

**병렬로 둔 이유와 겹치는 곳**

- `verify-paths`와 `machine-limit`은 둘 다 `src/verify.rs`의 `run`·`run_probe`와 README를 고친다. 맡는 줄이 다르다: `verify-paths`는 단계 고르기(단계 반복문)와 `Options`에 기준을 넘기는 필드, `machine-limit`은 `serialize` 잠금 바로 뒤의 PC 슬롯 잡기다. 두 지시서에 그 파일은 최소 변경으로 하라고 적었다. `config.rs`는 `verify-paths`만 고친다. `machine-limit`은 새 모듈에 `machine.toml`을 둔다.
- `chute-followups`는 `src/cmd/events.rs`, `src/feedback.rs`의 respawn 첫 지시, README·`skill/manual.md`의 한두 문장만 고친다. README는 셋이 모두 고치므로 모두 최소 변경이다.
- `deploy-chute`와 `health-chute`는 `orch.py`의 다른 함수(배포, health)와 SKILL.md의 다른 절(배포, health 단락)을 고친다. 최소 변경으로 하라고 적었다.
- `vally-chute`는 다른 저장소라 겹치는 것이 없다. 다른 탭의 오케스트레이터가 vallythevally에서 일하고 있으니 그 저장소에서는 `chute.toml`과 `AGENTS.md`만 고친다.

## 7. 범위 밖

- vallythevally의 배포를 켜는 일. 운영 명령을 확인하고 `hold`를 푸는 것은 사용자가 따로 정한다(`docs/deploy.md`의 마지막 단락).
- orch 저장소에 GitHub 원격을 만드는 일(8절의 4번).
- 문지기를 쓰지 않는 다른 저장소(hoga-ops 등)를 chute로 옮기는 일. 4.12와 같은 절차로 따로 한다.
- chute의 `parallel` 단계, 흔들림의 범인 찾기와 머지 보류.
- 사용자의 Claude 설정(`~/.claude/settings.json`)에서 `merge_guard.py` 훅 줄을 지우는 일. 파일이 없으면 그 훅은 아무것도 하지 않는다(`[ -r "$g" ] || exit 0`). 지울지는 사용자가 정한다.
- 알림을 다시 보내는 장치(v3과 같다).

## 8. 사용자가 정해야 하는 결정

작업은 아래 기본값으로 진행할 수 있다.

| # | 물을 것 | 기본값 | 바꾸면 |
|---|---|---|---|
| 1 | PC 전체 검증 상한 `max_verify`의 기본값 | 3(32코어 PC에서 batch와 preflight를 함께 돌릴 만큼, 첫 가동 날의 부하 130보다 한참 아래) | `machine-limit`의 상수 하나 |
| 2 | vallythevally에서 흔들린 batch를 머지할지 | 머지하고 `flaky` 알림과 장부를 남긴다(chute의 정책, v3 결정) | 막으려면 chute에 머지 보류를 새로 만들어야 한다. 이 플랜 밖 |
| 3 | vallythevally의 `server-test`와 `e2e`를 함께 돌릴지 | 차례로 돈다. 전환 뒤 batch 시간을 재서 다시 정한다 | vallythevally 스크립트에 두 단계를 함께 도는 명령을 만들고 `chute.toml`에서 한 단계로 묶는다(새 작업) |
| 4 | orch 저장소에 원격을 만들지 | 만들지 않는다. 로컬 통합으로 남긴다 | GitHub 저장소를 만들면 4.12와 같은 절차로 chute에 옮길 수 있다 |
| 5 | 전환 시점과 다른 탭 오케스트레이터에게 알리는 일 | 4.12의 2가 맞을 때 사용자가 그 탭에 SKILL.md를 다시 읽게 하고 전환한다 | 그 탭의 작업이 끝날 때까지 미룬다 |
| 6 | Claude에 chute plugin을 설치할지 | `gate-retire` 전에 설치한다(4.13) | 설치하지 않으면 `gate-retire`가 `merge_guard.py`를 chute 저장소도 막게 고쳐 남긴다(그 지시서에 적었다) |
| 7 | 문지기를 지울지 | 전환을 확인한 뒤 지운다(4.2) | 남기면 `gate-retire`를 올리지 않는다. SKILL.md에 문지기 규칙이 그대로 남는다 |
| 8 | 배포의 자동 시작을 없앨지 | 없앤다. `deploy run`은 사용자 승인 뒤 오케스트레이터가 실행한다(4.8) | chute의 merged 알림을 받으면 orch가 배포를 시작하게 할 수 있다. 사용자 승인 규칙과 맞지 않아 권하지 않는다 |
| 9 | 반송을 고친 새 HEAD를 다시 승인받을지 | 지금 규칙대로 새 HEAD마다 다시 승인받는다 | 처음 승인을 같은 작업의 고친 HEAD까지 넓히면 SKILL.md 4절의 한 문장이 바뀐다 |

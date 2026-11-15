# chute v1 플랜

> 작성일 2026-11-15. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

chute는 herdr 에이전트들이 main에 합류하는 관문이다. 에이전트는 PR을 올리기 전에 chute에 묻고, chute는 통과한 PR을 묶어 머지하고, 막힌 PR은 이유와 함께 만든 에이전트에게 돌려보낸다. 사람은 Herdr 창에서 흐름을 본다.

## 1. 사용자가 풀려는 문제

herdr에서 여러 에이전트가 PR을 쏟아 내는데 머지가 그 속도를 따라가지 못한다. 막히는 곳은 세 군데다.

- CI 대기: PR마다 원격 CI가 끝나기를 기다린다.
- 충돌: 앞 PR이 머지되면 뒤 PR이 충돌하고, 누군가 그것을 알아채 에이전트에게 다시 맞추라고 해야 한다.
- 사람 확인: 실패 원인을 읽고 해당 에이전트에게 전달하는 일을 사람이 한다.

chute는 검증을 로컬에서 묶음 단위로 한 번에 돌리고, 실패와 충돌을 원인과 함께 만든 에이전트에게 자동으로 돌려보내고, 사람에게는 사람만 풀 수 있는 것만 남긴다.

## 2. 확정된 결정 (사용자와 합의)

| 항목 | 결정 |
|---|---|
| 구성 | Rust 엔진(CLI + 데몬) + Herdr mod + 스킬. 이름과 명령어는 `chute` |
| herdr 연계 | 필수. herdr-core CLI `hc`로 에이전트에게 보내고 새 에이전트를 띄운다 |
| 신뢰 기본값 | 에이전트 신뢰(자동 머지). 사람 확인은 옵션이고, 켜면 Herdr mod의 승인 버튼으로 받는다 |
| 검증 | 로컬 검증이 최종 판정이다. GitHub CI를 기다리지 않는다 |
| 머지 방식 | 검증된 batch를 main에 직접 push한다. PR마다 merge commit을 남겨 GitHub이 PR을 Merged로 인식하게 한다 |
| 대상 | GitHub 전용, 1인 개발자, 로컬 PC |

합의된 동작은 열 가지다. preflight, submit, batch, 로컬 검증, 이분 탐색, 반송, 재시도 예산과 flaky 판별, 현황판(mod), 스킬, batch 되돌리기.

## 3. 확인한 사실

요청이 확인하라고 한 세 가지와, 설계에 영향을 준 저장소 상태다.

**Herdr mod가 데이터를 읽고 입력을 받는 방법** (`~/.config/herdr-app/AGENTS.md`, herdr-app 0.1.15)

- mod는 `~/.config/herdr-app/mods/<이름>/main.js`에 있는 JavaScript다. HTML이나 Node API는 없다.
- 읽기: `exec(cmd, args, {cwd, timeout})`로 프로그램을 실행해 출력을 받는다(기본 30초, 최대 10분). `readTextFile(path)`로 파일을 읽는다. `watchFolder(dir)`는 폴더가 바뀔 때 깨어난다. 주기 실행은 `cx.timer.every`다.
- 입력: 요소에 `.on_click`을 달고 그 안에서 `exec`나 `run(앱 명령)`을 부른다. 승인 버튼은 `exec("chute", ["approve", …])`로 만들 수 있다.
- 검사: `herdr-app check <mod 폴더>`가 mod를 한 번 불러 그려 보고 오류를 낸다. 창에 설치하지 않고 돌릴 수 있다. 실제 화면은 `herdr-app shot`으로 본다.
- Herdr가 처음 보는 mod 폴더는 `mod.json`의 `place`대로 지금 레이아웃에 한 번 붙는다.

**`hc` 터미널 주소가 재사용되는가** (hc 0.1.21)

- 문서에 재사용 여부를 밝힌 문장은 없다.
- 관찰: `hc ls --all`의 주소가 `t1jk, t1q0, t1qz, t1r1, t1r8, t1rp, t1rq, t1rs`로 만든 순서대로 커지고, 끝난 터미널도 `hc rm` 전까지 주소를 가진다. 재사용은 보지 못했다.
- 보장이 없으므로 주소와 함께 `hc get`의 `created_ms`를 기록한다. 반송할 때 둘이 모두 맞고 `state`가 `live`일 때만 같은 에이전트로 본다.
- Codex는 공유 app-server 때문에 이전 터미널의 `HC_TERMINAL_ID`를 넘겨받을 수 있다(orch 스킬 문서). 그래서 submit은 `--agent <주소>`로 주소를 직접 받을 수도 있어야 한다.

**`hc --skill`에서 chute가 따라야 하는 것**

- `hc send`는 대상이 `blocked`면 거절한다(종료 코드 9). `--force`로 권한 대화상자를 넘기지 않는다.
- 새 에이전트는 `hc run --bg`로 띄우고 첫 일을 `run`의 인자로 준다. 결과의 `first_message.sent`가 false면 일이 전달되지 않은 것이다.
- 부모 없이 살아남아야 하는 터미널은 `--no-parent`로 띄운다. 오류는 stderr에 `{error: {kind, message, hint}}`로 온다.

**저장소**

- `Soochol/chute`는 공개 저장소이고 `main`에 보호 규칙이 없다. merge commit이 허용돼 있다. CI는 없다.
- 저장소에는 초기화 커밋 하나만 있다. `AGENTS.md`도 없다.
- 로컬 도구: cargo 1.96.0, git 2.53.0, gh, hc, herdr-app, node.

## 4. 스스로 정한 결정과 이유

### 4.1 구조

**Rust crate 하나, 바이너리 하나, 동기 코드.** 한 번에 검증 하나를 돌리고 나머지는 외부 명령(`git`, `gh`, `hc`) 실행이다. 비동기 런타임은 쓰지 않는다.

**CLI와 데몬은 파일로만 이야기한다.** 상태는 저장소별 상태 폴더의 JSON 파일이고, 고칠 때는 잠금(flock)을 잡고 임시 파일에 쓴 뒤 이름을 바꾼다. 소켓이나 RPC는 없다. 이유는 세 가지다. 데몬이 죽어도 큐가 남는다. mod가 폴더 변화를 감지해 새로 고칠 수 있다. 시험이 쉽다. 깨진 상태 파일은 빈 큐로 읽지 않고 오류로 멈춘다.

**별도 clone을 두지 않고 사용자의 저장소 객체 저장소를 쓴다.** 에이전트는 같은 저장소의 worktree에서 일한다. preflight는 아직 push하지 않은 커밋을 검증해야 하는데, 객체 저장소가 같으면 fetch 없이 그 커밋을 쓸 수 있다. 별도 clone에서 일하는 에이전트의 preflight는 v1에서 지원하지 않는다(submit은 PR 번호만 쓰므로 된다).

**검증 worktree는 슬롯으로 두고 재사용한다.** 요청은 "임시 worktree"라고 했지만, 매번 새 폴더를 만들면 빌드 캐시(`target/`, `node_modules/`)가 없어 검증마다 처음부터 빌드한다. 슬롯은 상태 폴더 아래 고정 경로의 worktree이고, 쓸 때마다 원하는 커밋으로 맞추고 추적되지 않는 파일만 지운다(git이 무시하는 캐시는 남긴다). 격리 수준은 임시 worktree와 같다. batch용 슬롯 하나와 preflight용 슬롯 여러 개를 둔다.

**저장소 하나에 데몬 하나.** `chute up`이 `hc run --bg --no-parent`로 데몬을 hc 터미널에 띄운다. Herdr에서 보이고 읽을 수 있다. 이미 떠 있으면 아무 일도 하지 않는다. `chute submit`은 데몬이 없으면 띄운다. 에이전트가 submit만 해도 큐가 흐르게 하기 위해서다.

**외부 명령은 한 모듈에서만 부른다.** `git`, `gh`, `hc` 실행은 경계 모듈 하나를 거치고, 실행 파일 경로를 환경 변수로 바꿀 수 있다. 시험은 진짜 git(임시 저장소와 bare origin)과 가짜 `gh`·`hc` 스크립트로 한다. 시험이 사용자의 hc 코어, Herdr 창, GitHub에 닿지 않게 하는 장치다.

**설정은 대상 저장소의 `chute.toml`이고, 기준 브랜치의 것을 읽는다.** batch나 PR 안의 `chute.toml`이 아니라 `origin/main`의 것으로 검증한다. PR이 검증 명령을 `true`로 바꿔 스스로 통과하는 것을 막는다. 같은 이유로 **`chute.toml`을 고치는 PR은 신뢰 모드에서도 사람 승인 대기로 보낸다.** 검증 규칙을 바꾸는 변경은 검증이 판정할 수 없다.

### 4.2 머지와 검증

**불변식: main에 push하는 커밋은 검증 명령이 통과한 바로 그 트리다.** 아래 규칙은 모두 여기서 나온다.

1. 데몬이 큐에서 PR을 올린 순서대로 최대 `max_batch`개 가져온다.
2. `origin/main`을 fetch해 기준으로 삼는다. batch 슬롯을 기준에 맞추고 PR의 등록된 HEAD를 `git merge --no-ff`로 차례로 합친다. 커밋 메시지는 GitHub의 형식(`Merge pull request #N from …`)을 따른다.
3. 합치다 충돌한 PR은 그 batch에서 빼고 나머지로 계속한다. batch가 끝난 뒤 그때의 main에 다시 합쳐 본다. 여전히 충돌하면 반송하고, 깨끗하면 큐에 남긴다. 함께 묶였던 PR이 결국 머지되지 않았다면 충돌이 아니기 때문이다.
4. 합친 트리에 검증 단계를 순서대로 돌린다. 통과하면 `git push origin <batch HEAD>:main`으로 올린다. 강제 push는 하지 않는다. main이 그사이 움직여 거절되면 새 기준으로 다시 만든다(3회까지).
5. 단계가 실패하면 **같은 트리에서 그 단계만 한 번 더** 돌린다. 통과하면 flaky로 기록하고 남은 단계를 이어 간다. 또 실패하면 진짜 실패다.
6. 진짜 실패면 batch를 반으로 나눈다. 앞 절반을 만들고 검증해 통과하면 push하고, 뒤 절반을 새 main 위에 다시 만들어 검증한다. PR 하나가 남아 실패하면 그 PR이 범인이다.
7. 범인을 반송하기 전에 **단독 실패인지 조합 실패인지** 가린다. 범인을 검증한 기준이 batch를 시작할 때의 main과 같으면 단독이다. 다르면(같은 batch의 앞 PR이 먼저 머지됐으면) 시작 때의 main에 범인만 얹어 한 번 더 검증한다. 실패하면 단독, 통과하면 조합이고 상대는 그사이 머지된 PR들이다.
8. **기준이 깨진 경우를 가린다.** 단독 실패로 반송하기 전에, 그 기준 커밋이 검증을 통과한 적이 없으면 기준만 검증한다. 기준도 실패하면 아무도 반송하지 않고 큐를 멈춰 사람 차례에 올린다(`base_broken`). main이 누군가의 직접 push나 환경 문제로 깨졌을 때 모든 에이전트에게 틀린 반송이 나가는 것을 막는다. chute가 push한 커밋은 검증을 통과한 것이므로 다시 검증하지 않는다.
9. 시간 제한을 넘긴 단계는 실패와 같게 다루되 5번의 재실행은 하지 않는다(제한 시간만큼 또 기다리게 된다). 보고서에 `timeout`이라고 적는다. 검증 명령은 자기 프로세스 그룹으로 띄우고, 넘기면 그룹 전체를 끝낸다.

비용은 정상일 때 batch당 검증 한 번이다. PR n개 가운데 하나가 깨져 있으면 대략 2·log₂(n) + 4번이다.

**등록된 HEAD만 머지한다.** submit할 때 PR의 HEAD SHA를 기록한다. batch를 만들 때 PR의 HEAD가 그 값과 다르면 건너뛰고 에이전트에게 "HEAD가 바뀌었다. 준비되면 다시 submit하라"고 알린다. 예산은 깎지 않는다. 작업 도중의 커밋을 머지하지 않고, 검증한 것과 머지한 것이 같게 하기 위해서다.

**기본 브랜치를 향한 열린 PR만 받는다.** draft이거나 다른 브랜치를 향한 PR은 submit이 거절한다.

**push 뒤 확인.** push가 끝나면 `gh`로 PR 상태가 MERGED인지 본다. 아니면 현황판에 경고를 남긴다. "merge commit을 직접 push하면 GitHub이 Merged로 인식한다"는 가정은 가짜 `gh`로는 시험할 수 없어서, 실제 첫 가동에서 확인된다.

### 4.3 반송

**보고서는 파일 두 개다.** 에이전트가 읽는 Markdown과 같은 내용의 JSON이다. 담는 것은 다음과 같다.

- 검증 실패: 실패한 단계, 실패한 시험 이름(단계에 `failed` 정규식이 있을 때. 없으면 로그의 끝부분), 로그 경로, 단독인지 조합인지, 조합이면 상대 PR의 번호·제목·바뀐 파일·diff 파일 경로, 재현 명령.
- 충돌: 충돌한 파일, 그 파일을 고친 main 쪽 커밋(SHA, 제목), 해결 방법(main을 합치고 다시 submit).
- 재현: 실패한 트리의 커밋을 `refs/chute/failed/…` 참조로 남겨 둔다. 에이전트는 그 커밋을 체크아웃해 단계 명령을 그대로 실행하면 된다. PR이 머지되거나 큐에서 빠지면 참조를 지운다.

**전달.** 기록한 주소의 터미널이 살아 있고 `created_ms`가 같으면 `hc send`로 요약 한 줄과 보고서 경로를 보낸다. 없으면 `chute.toml`의 `respawn` 명령을 `hc run --bg --no-parent`로 띄우고 첫 일로 같은 내용을 준다. 작업 폴더는 원래 에이전트의 worktree가 남아 있으면 그곳이고, 없으면 `~/.herdr/worktrees/<저장소>/<브랜치>`에 PR 브랜치의 worktree를 만든다(Herdr의 worktree 규칙과 같은 자리). 새 에이전트에는 `herdr.space`·`herdr.tab` 라벨을 붙이지 않는다. 붙이면 사용자의 탭에 pane이 끼어든다. 현황판에서 주소를 눌러 들여다볼 수 있게 한다.

**전달하지 못하면 사람 차례다.** 대상이 `blocked`이거나, `respawn`이 비어 있거나, 띄운 에이전트에 첫 일이 전달되지 않았으면 그 PR을 사람 차례에 올린다(`unreachable`).

**머지 성공은 에이전트에게 보내지 않는다.** `hc send`는 에이전트의 턴을 깨워 비용이 드는데, 머지된 뒤에는 할 일이 없다. `chute status`로 확인할 수 있다.

**재시도 예산.** PR마다 연속 반송 횟수를 센다. 반송을 `budget`번(기본 3) 보낸 뒤 또 실패하면 반송하지 않고 사람 차례에 올린다(`budget`). 머지되거나 사람이 `chute retry`를 하면 0으로 돌아간다.

**flaky.** 4.2의 5번으로 판정한다. 그 batch는 통과로 보고 머지하며, 단계·시험 이름·트리를 기록해 현황판에 보인다. 에이전트에게는 보내지 않는다. 8절의 1번 결정이 이 처리를 바꿀 수 있다.

### 4.4 사람 확인 옵션

`approval = "human"`이면 submit한 PR은 `awaiting_approval`에 머물고, 사람이 mod의 승인 버튼(`chute approve <pr>`)을 눌러야 큐에 들어간다. **승인은 검증 뒤가 아니라 큐에 들어가기 전의 관문이다.** 검증을 마친 batch를 승인이 날 때까지 붙잡아 두면 그동안 main이 움직이지 못하고 뒤의 PR이 모두 막힌다. 실패할 PR을 사람이 미리 읽는 낭비를 줄이려고, 승인 대기 항목 옆에 그 HEAD의 preflight 결과를 보인다. 승인은 HEAD에 묶인다. 새 커밋으로 다시 submit하면 다시 승인받는다.

### 4.5 preflight

에이전트의 worktree에서 `chute preflight`를 실행하면 `origin/main` + 지금 큐의 PR들 + 자신의 HEAD를 합쳐 검증하고 결과를 그 자리에서 돌려준다. 실패나 충돌이면 반송과 같은 형식의 보고서 경로를 출력한다. 큐의 PR끼리 충돌하면 batch와 같은 규칙으로 건너뛴다. 커밋되지 않은 변경이 있으면 거절한다.

preflight는 호출한 프로세스에서 돌고, 기본으로 batch 검증과 동시에 돈다. 슬롯이 달라 파일은 겹치지 않는다. 포트나 DB처럼 겹치는 자원을 쓰는 저장소는 `serialize = true`로 검증을 한 번에 하나만 돌게 한다. 검증 명령에는 실행마다 다른 `CHUTE_RUN_ID`를 환경 변수로 준다.

submit은 preflight 통과를 강제하지 않는다. 최종 판정은 batch 검증이기 때문이다.

### 4.6 되돌리기

`chute revert <batch>`는 그 batch의 merge commit들을 역순으로 `git revert -m 1` 해 지금의 main 위에 쌓고, 검증한 뒤 push한다(`--no-verify`로 검증을 건너뛸 수 있다). 강제 push로 main을 되감지 않는다. 그 뒤에 머지된 변경과 충돌하면 아무것도 push하지 않고 멈춘다.

되돌린 PR의 커밋은 main의 이력에 남아 있어서, 같은 브랜치를 다시 머지해도 변경이 돌아오지 않는다. revert는 되돌린 PR 목록과 다시 올리는 방법(브랜치에서 revert 커밋을 되돌린 뒤 고친다)을 출력한다. 되돌리기는 사람이 내린 결정이므로 에이전트에게 자동으로 알리지 않는다.

### 4.7 현황판과 스킬

**mod는 `chute status --json --all`의 출력만 그린다.** 상태 파일을 직접 읽지 않는다. 상태 폴더의 변화(`watchFolder`)와 느린 타이머는 새로 고칠 때를 알려 줄 뿐이다. 버튼은 `chute approve`, `chute retry`, `chute drop`을 실행한다. 되돌리기는 CLI에만 둔다. 화면 순서는 사람 차례, 검증 중 batch, 대기, 반송, 오늘의 숫자다.

**상태 JSON이 엔진과 mod 사이의 계약이다.** 모든 상태를 담은 fixture 파일을 저장소에 두고, Rust 쪽은 fixture가 상태 타입으로 읽히고 다시 같은 JSON으로 써지는지 시험하고, mod 쪽은 같은 fixture로 표시 항목을 만든다. 필드 이름이 바뀌면 시험이 깨진다.

**스킬 파일은 얇다.** `chute install`이 `~/.claude/skills/chute/SKILL.md`와 `~/.codex/skills/chute/SKILL.md`에 "`chute --skill`을 실행해 따르라"는 안내만 쓴다. 설명서 본문과 mod 파일은 바이너리에 들어 있다. `chute install`은 mod도 `~/.config/herdr-app/mods/chute/`에 복사한다.

**템플릿과 `AGENTS.md`는 고치지 않고 넣을 한 줄을 출력한다.** herdr 템플릿 폴더(`~/.config/herdr-core/templates/`)는 지금 비어 있다. 사용자의 지침 파일을 chute가 몰래 고치지 않는다. `chute install`과 `chute init`이 넣을 문장을 출력하고 README가 어디에 넣는지 안내한다.

**설명서와 메시지는 한국어로 쓴다.** 사용자의 스킬과 지침이 한국어다. 코드와 식별자는 영어다.

### 4.8 저장소 운영

**GitHub Actions CI를 두지 않는다.** "로컬 검증이 최종"이 이 프로젝트의 전제다. 저장소 루트에 chute 자신의 `chute.toml`을 두어 검증 명령(`cargo fmt --check`, `cargo clippy`, `cargo test`)을 한곳에 적는다. 작업자는 CI 대신 최종 HEAD의 로컬 전체 시험 결과를 근거로 보고한다.

## 5. 계약

작업들이 병렬로 진행되므로 서로 맞물리는 부분을 여기서 정한다. 첫 작업(`scaffold`)이 이것을 코드로 옮기고, 뒤 작업은 필드를 더할 수는 있지만 이름과 뜻을 바꾸지 않는다.

### 5.1 명령

| 명령 | 누가 | 하는 일 |
|---|---|---|
| `chute init` | 사람 | 대상 저장소에 `chute.toml` 초안을 쓰고 상태 폴더에 등록한다. `AGENTS.md`에 넣을 한 줄을 출력한다 |
| `chute preflight [--json]` | 에이전트 | main + 큐 + 자신의 HEAD를 검증한다. 종료 코드 0 통과, 1 검증 실패나 충돌, 2 실행할 수 없음 |
| `chute submit [<pr>] [--agent <주소>] [--json]` | 에이전트 | 지금 브랜치의 PR(또는 번호)을 큐에 올린다. HEAD와 에이전트를 기록한다 |
| `chute giveup [<pr>] --reason <글>` | 에이전트 | 못 풀겠다고 선언한다. 사람 차례로 간다 |
| `chute status [--json] [--all]` | 모두 | 큐, batch, 사람 차례, 오늘의 숫자. `--all`은 등록된 모든 저장소 |
| `chute approve <pr>` | 사람(mod) | 승인 대기 PR을 큐에 넣는다 |
| `chute retry <pr>` | 사람(mod) | 사람 차례의 PR을 예산을 0으로 하고 큐에 다시 넣는다 |
| `chute drop <pr>` | 사람(mod) | PR을 큐에서 뺀다 |
| `chute up` / `chute down [--now]` | 사람 | 데몬을 hc 터미널로 띄운다 / 지금 batch 뒤(또는 즉시) 멈춘다 |
| `chute daemon` | `up`이 실행 | 데몬을 전경에서 돈다 |
| `chute batches` | 사람 | 최근 batch 목록 |
| `chute revert <batch> [--no-verify]` | 사람 | batch를 되돌린다 |
| `chute install` | 사람 | mod와 스킬 파일을 설치한다 |
| `chute --skill` | 에이전트 | 설명서를 출력한다 |

저장소 밖에서 실행할 때는 `--repo <경로>`로 대상을 고른다(mod의 버튼이 쓴다).

### 5.2 설정 `chute.toml`

```toml
approval = "agent"      # "agent" 또는 "human"
budget = 3              # 연속 반송 한도
max_batch = 8           # 한 batch의 PR 수 상한
serialize = false       # true면 검증을 한 번에 하나만 돈다
respawn = ["claude"]    # 에이전트가 없을 때 띄울 명령. 빈 배열이면 띄우지 않는다

[[verify]]
name = "test"
run = "cargo test"      # 슬롯 worktree에서 sh -c로 실행한다
timeout = 1800          # 초. 기본 3600
failed = 'test (\S+) \.\.\. FAILED'   # 선택. 실패한 시험 이름을 뽑는 정규식
```

### 5.3 상태 폴더

`$CHUTE_HOME`(없으면 `$XDG_STATE_HOME/chute`, 그것도 없으면 `~/.local/state/chute`) 아래에 저장소마다 상태 폴더 하나와 슬롯 폴더 하나를 둔다. 폴더 이름은 원격의 `owner/name`에서 만든다.

```
repos/<owner>__<name>/      상태 폴더
  repo.json       저장소 경로, 원격, 기본 브랜치
  state.json      큐 항목, 지금 batch, 멈춤 사유, 검증을 통과한 기준 커밋, preflight 결과
  lock            state.json을 고칠 때 잡는 잠금
  daemon.lock     데몬이 사는 동안 쥐는 잠금
  daemon.json     pid, hc 주소, 지금 하는 일
  history.jsonl   머지, 반송, flaky, 되돌리기 기록 (한 줄에 하나)
  batches/<id>/   batch 기록과 검증 로그
  reports/pr-<n>/ 반송 보고서 (<순번>.md, <순번>.json, 상대 PR의 diff)
slots/<owner>__<name>/      검증 worktree (batch, preflight-<k>, revert)
```

슬롯을 상태 폴더 밖에 둔 것은 mod 때문이다. mod는 상태 폴더를 `watchFolder`로 지켜보는데, Linux에서 이 함수는 하위 폴더가 4096개를 넘으면 거절한다. worktree가 그 안에 있으면 넘는다.

### 5.4 큐 항목의 상태

| 상태 | 뜻 | 다음 |
|---|---|---|
| `awaiting_approval` | 사람 승인을 기다린다 | `approve` → `queued` |
| `queued` | batch를 기다린다 | 데몬이 가져가면 `in_batch` |
| `in_batch` | 지금 batch에 들어 있다 | 머지되면 큐에서 빠진다. 실패하면 `bounced`나 `needs_human`. 충돌이 풀리면 `queued` |
| `bounced` | 에이전트에게 돌려보냈다. 사유는 `conflict`, `verify`, `timeout`, `head_changed` | 다시 submit하면 `queued`(또는 `awaiting_approval`) |
| `needs_human` | 사람 차례. 사유는 `budget`, `unreachable`, `gave_up` | `retry` → `queued`, `drop` → 빠진다 |

큐 전체가 멈춘 상태는 항목이 아니라 저장소에 붙는다(`blocked: base_broken`).

### 5.5 `chute status --json`

```json
{
  "schema": 1,
  "repo": {"slug": "Soochol/chute", "path": "/home/dev/code/chute", "default_branch": "main",
           "state_dir": "/home/dev/.local/state/chute/repos/Soochol__chute"},
  "approval": "agent",
  "daemon": {"running": true, "terminal": "t1zz", "pid": 4242},
  "blocked": null,
  "batch": {"id": "b20261115-153000", "base": "abc1234", "prs": [12, 13], "phase": "verifying",
            "step": "test", "round": 1, "started_ms": 0},
  "queue": [
    {"pr": 12, "title": "…", "branch": "…", "head": "def5678", "state": "in_batch",
     "state_reason": null, "agent": {"terminal": "t1rs", "name": "impl-a", "alive": true},
     "attempts": 1, "budget": 3, "submitted_ms": 0, "touches_config": false,
     "preflight": {"result": "pass", "head": "def5678"},
     "last_failure": {"kind": "verify", "mode": "combo", "summary": "…", "report": "/…/reports/pr-12/1.md"}}
  ],
  "today": {"merged": 7, "bounced": 2, "flaky": 1, "batches": 3, "median_lead_s": 412, "verify_s": 1260},
  "recent": [{"batch": "b20261115-150000", "merged": [9, 10], "bounced": [11], "seconds": 388, "finished_ms": 0}],
  "warnings": []
}
```

`--all`은 이 객체의 배열이다. `phase`는 `building`, `verifying`, `bisecting`, `pushing` 가운데 하나다. `median_lead_s`는 오늘 머지된 PR의 submit부터 머지까지 걸린 시간의 중앙값이다.

### 5.6 소스 배치

| 경로 | 내용 | 맡는 작업 |
|---|---|---|
| `src/main.rs`, `src/cli.rs` | 진입점과 하위 명령 목록 | scaffold |
| `src/cmd/<명령>.rs` | 명령마다 인자 정의와 실행 | 명령을 맡은 작업 |
| `src/model.rs` | 큐 항목, 실패, batch 기록, 상태 JSON 타입 | scaffold |
| `src/config.rs`, `src/store.rs`, `src/ext.rs` | 설정, 상태 폴더와 잠금, 외부 명령 경계 | scaffold |
| `src/verify.rs` | 검증 실행기 | verify |
| `src/gitops.rs` | 슬롯, 합치기, 충돌 정보, push | gitops |
| `src/queue.rs` | 큐 상태 전이 | queue |
| `src/feedback.rs` | 보고서와 전달 | feedback |
| `src/engine.rs` | batch 처리와 데몬 | engine |
| `mod/chute/` | Herdr mod | mod |
| `skill/` | 설명서와 스킬 파일 | skill-install |
| `tests/fixtures/` | 상태 JSON fixture | scaffold |

`scaffold`가 모든 하위 명령을 `src/cli.rs`에 등록하고 `src/cmd/`에 "아직 구현되지 않았다"는 오류를 내는 파일을 만든다. 뒤 작업은 자기 명령 파일과 자기 모듈만 채우므로 서로 같은 파일을 고치지 않는다.

## 6. 작업 나누기

작업은 10개이고 PR 하나씩이다. 프로필은 모두 `implementer`다.

| # | 작업 ID | 맡는 것 | 기다리는 작업 |
|---|---|---|---|
| 1 | `scaffold` | crate, 명령 뼈대, 설정, 상태 저장, 도메인 타입, 외부 명령 경계, 시험 도구, `init`, `status`, `AGENTS.md` | 없음 |
| 2 | `verify` | 검증 실행기(단계, 시간 제한, 프로세스 그룹, 실패 시험 추출, 직렬 잠금) | scaffold |
| 3 | `gitops` | 슬롯 worktree, PR 합치기, 충돌 정보, push, 실패 트리 참조 | scaffold |
| 4 | `queue` | `submit`, `approve`, `retry`, `drop`, `giveup`과 상태 전이, 에이전트 식별 | scaffold |
| 5 | `feedback` | 실패 보고서와 `hc` 전달(send, 새 에이전트) | scaffold |
| 6 | `mod` | Herdr 현황판 mod | scaffold |
| 7 | `engine` | batch 처리(검증, flaky, 이분 탐색, 기준 확인, 예산), 데몬, `up`·`down` | verify, gitops, queue, feedback |
| 8 | `preflight` | `chute preflight` | verify, gitops, queue, feedback |
| 9 | `revert` | `chute revert`, `chute batches` | engine |
| 10 | `skill-install` | `chute --skill` 설명서, `chute install`, README | mod, engine, preflight |

```
scaffold ─┬─ verify ───┐
          ├─ gitops ───┼─┬─ engine ────┬─ revert
          ├─ queue ────┤ └─ preflight ─┼─ skill-install
          ├─ feedback ─┘               │
          └─ mod ──────────────────────┘
```

**순차로 둔 이유**

- 2~6이 `scaffold`를 기다린다: 모두 그 타입, 상태 저장, 외부 명령 경계, 시험 도구를 쓴다.
- `engine`이 2~5를 기다린다: 검증 실행기, 합치기, 큐 전이, 반송을 호출해 하나의 흐름으로 엮는다.
- `preflight`가 2~5를 기다린다: 합치기와 검증 실행기를 호출하고, 큐를 읽고, 보고서 형식을 쓴다.
- `revert`가 `engine`을 기다린다: engine이 쓰는 batch 기록을 읽고, 검증 뒤 push하는 경로를 다시 쓴다.
- `skill-install`이 `mod`, `engine`, `preflight`를 기다린다: 설명서는 실제 명령의 동작을 적고, `install`은 mod 파일을 바이너리에 넣는다.

**병렬로 둔 이유**

- 2~6은 서로 다른 파일을 맡는다. `src/model.rs`와 `Cargo.toml`에 몇 줄 더하는 일이 겹칠 수 있어 지시서에 최소 변경으로 하라고 적었다. `Cargo.lock` 충돌을 줄이려고 `scaffold`가 뒤 작업이 쓸 의존성을 미리 넣는다.
- `engine`과 `preflight`는 다른 파일을 맡고, 둘 다 2~5의 함수를 부르기만 한다.
- `revert`와 `skill-install`은 다른 파일을 맡는다. 설명서의 revert 설명은 이 문서의 4.6을 따른다.

## 7. 범위 밖

요청에서 뒤로 미룬 것:

- 충돌 예보(main이 전진할 때 작업 중인 에이전트에게 알림)
- 수리 대신 재생성 판단
- 반복 실패 유형 집계와 에이전트 지침 제안
- 위험도 기반 자동/사람 확인 세분화. v1에는 `approval`의 켜고 끔과 `chute.toml` 변경 PR의 승인 대기만 있다

이 플랜이 뺀 것:

- 에이전트의 우회(`gh pr merge`, `git push origin main`)를 훅으로 막는 일. v1은 스킬의 규칙으로만 금지한다
- orch 스킬의 병합 대기열(`land`, `gate`)을 chute로 바꾸거나 잇는 일
- 머지된 브랜치와 worktree 정리
- 별도 clone에서의 preflight, fork에서 온 PR, 기본 브랜치가 아닌 곳을 향한 PR
- 흔들리는 시험의 근거 관리(기준에서 재현, 만성 표시). v1은 기록과 표시만 한다
- GitHub 외 호스팅, 여러 기기, 원격 기기의 에이전트
- 실제 GitHub 저장소에서의 첫 가동. 시험은 가짜 `gh`·`hc`와 로컬 bare origin으로 한다. 첫 가동은 사용자가 README의 절차로 한다

## 8. 사용자가 정해야 하는 결정

1. **flaky로 판정된 batch를 머지할 것인가.** 추천은 머지하고 기록하는 것이다(지금 플랜). 대안은 머지하지 않고 사람 차례에 올리는 것이다. 추천안은 흔들리는 시험을 새로 만든 PR도 머지될 수 있다. 대안은 흔들림이 생길 때마다 큐가 사람을 기다린다.
2. **에이전트가 없을 때 띄울 명령(`respawn`)과 그 권한.** 추천 기본값은 `["claude"]`이고 권한 모드는 사용자의 Claude 설정을 따른다. 무인 실행이라 권한 대화상자에서 멈출 수 있고, 그때는 사람 차례에 올라온다. orch 작업자처럼 `--permission-mode bypassPermissions`를 붙일지는 사용자가 `chute.toml`에서 정한다.
3. **mod 작업자가 사용자의 Herdr 창에 mod를 설치해 화면을 확인해도 되는가.** 지금 지시서는 `herdr-app check`(창을 건드리지 않음)까지만 허용한다. 허용하면 작업자가 `~/.config/herdr-app/mods/chute/`에 설치하고 `herdr-app shot`으로 확인한다. 이때 사용자의 창에 패널이 하나 붙는다. 허용하지 않으면 화면 확인은 사용자가 `chute install` 뒤에 한다.
4. **GitHub Actions CI를 둘 것인가.** 추천은 두지 않는 것이다(4.8). 두려면 `scaffold`에 fmt·clippy·test 워크플로를 더한다.

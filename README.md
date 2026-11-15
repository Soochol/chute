# chute

chute는 herdr 에이전트들의 PR을 묶어 로컬에서 검증하고 기본 브랜치에 머지하는 머지 큐다. GitHub 저장소, 한 사람, 로컬 PC를 전제로 한다. 설계는 [`docs/plans/chute-v1.md`](docs/plans/chute-v1.md)에 있다.

## 푸는 문제

여러 에이전트가 PR을 쏟아 내는데 머지가 그 속도를 따라가지 못한다. 막히는 곳은 세 군데다.

- PR마다 원격 CI가 끝나기를 기다린다.
- 앞 PR이 머지되면 뒤 PR이 충돌하고, 누군가 알아채서 에이전트에게 다시 맞추라고 해야 한다.
- 실패 원인을 읽고 해당 에이전트에게 전달하는 일을 사람이 한다.

chute는 검증을 로컬에서 묶음(batch) 단위로 한 번에 돌린다. 실패와 충돌은 원인과 함께 만든 에이전트에게 자동으로 돌려보낸다. 사람에게는 사람만 풀 수 있는 것만 남긴다.

## 동작

1. 에이전트가 PR을 올리기 전에 `chute preflight`로 기본 브랜치 + 큐 + 자기 HEAD를 합쳐 검증한다.
2. PR을 올린 뒤 `chute submit`으로 큐에 넣는다. 그 순간의 PR HEAD가 기록된다.
3. 데몬이 큐에서 PR을 올린 순서대로 최대 `max_batch`개 가져와 기본 브랜치 위에 합치고 `chute.toml`의 검증 단계를 돌린다.
4. 통과하면 검증한 바로 그 트리를 기본 브랜치에 push한다. 실패하면 batch를 반으로 나눠 범인을 찾는다. 범인은 보고서와 함께 에이전트에게 돌아간다. 에이전트가 없으면 새로 띄운다.
5. 머지에 성공하면 에이전트에게 알리지 않는다. `chute status`나 현황판으로 본다.

## 설치

```sh
cargo install --path .
```

개발할 때는 `cargo build --release`로 빌드하고 `cargo test`로 시험한다. 실행 파일은 `target/release/chute`다.

필요한 것은 다음과 같다.

- `git`
- `gh`(로그인한 상태)
- `hc`(herdr-core CLI). 에이전트에게 메시지를 보내고 새 에이전트와 데몬 터미널을 띄우는 데 쓴다.
- Herdr. 현황판 mod를 보려면 필요하다.

## 처음 쓰기

1. 대상 저장소에서 `chute init`을 실행한다. `chute.toml` 초안을 쓰고 저장소를 상태 폴더에 등록한다.
2. `chute.toml`의 `[[verify]]`를 이 저장소의 검사 명령으로 고치고 커밋해 기본 브랜치에 올린다. chute는 `origin/<기본 브랜치>`에 올라간 `chute.toml`만 읽는다.
3. `chute install`을 실행한다. 스킬 파일(`~/.claude/skills/chute/SKILL.md`, `~/.codex/skills/chute/SKILL.md`)과 현황판 mod(`~/.config/herdr-app/mods/chute/`)를 쓴다. `~/.claude`나 `~/.codex`가 없으면 그쪽은 건너뛰고, Herdr 홈이 없으면 mod를 건너뛴다. `HERDR_APP_HOME`이 있으면 그 아래에 mod를 쓴다. 다시 실행해도 같은 결과이고, 무엇을 썼고 무엇이 그대로인지 출력한다.
4. 대상 저장소의 `AGENTS.md`(또는 에이전트 지침 파일)에 한 줄을 넣는다. `chute init`과 `chute install`이 출력하는 문장이다. chute는 지침 파일과 herdr 템플릿을 고치지 않는다.
   ```
   - 이 저장소의 PR은 chute로 머지한다. PR을 올리기 전에 `chute --skill`을 실행해 나온 절차를 따르고, `gh pr merge`나 기본 브랜치로의 직접 push는 하지 않는다.
   ```
5. `chute up`으로 데몬을 띄운다. 데몬은 `hc` 터미널에서 돌아 Herdr에서 보인다. 이미 떠 있으면 아무 일도 하지 않는다. `chute submit`도 데몬이 없으면 띄운다.
6. 에이전트가 `chute preflight`와 `chute submit`을 쓴다. 에이전트용 설명서는 `chute --skill`이 출력한다.

현황판이 `chute install` 뒤에 오른쪽에 자동으로 붙지 않을 수 있다. Herdr는 처음 보는 mod 폴더만 `mod.json`의 위치대로 한 번 붙이기 때문이다(이 기기에서는 추정이다). 붙지 않으면 직접 연다.

```sh
herdr-app do tab.new '{"mod":"chute"}'
```

`chute`가 Herdr의 `PATH`에 없으면 현황판이 실행 불가를 알린다. 위의 `cargo install --path .`로 `PATH`에 있는 곳에 설치한다.

## `chute.toml`

대상 저장소의 루트에 둔다. 기본 브랜치의 것을 읽는다. PR이나 batch 안의 `chute.toml`은 읽지 않는다.

```toml
approval = "agent"
budget = 3
max_batch = 8
serialize = false
require_ci = false
respawn = ["claude", "--dangerously-skip-permissions"]

[[verify]]
name = "test"
run = "cargo test"
timeout = 3600
failed = 'test (\S+) \.\.\. FAILED'
```

| 키 | 기본값 | 뜻 |
|---|---|---|
| `approval` | `"agent"` | `"agent"`면 submit한 PR이 바로 큐에 들어가 자동으로 머지된다. `"human"`이면 `승인 대기`에 머물다가 사람이 승인해야 큐에 들어간다. 승인은 검증 뒤가 아니라 큐에 들어가기 전의 관문이다. 승인은 HEAD에 묶인다. 새 커밋으로 다시 submit하면 다시 승인받는다 |
| `budget` | `3` | PR마다 연속 반송 한도. 반송을 이만큼 보낸 뒤 또 실패하면 사람 차례(`예산 소진`)로 올린다. 머지되거나 `chute retry`를 하면 0으로 돌아간다 |
| `max_batch` | `8` | 한 batch의 PR 수 상한. 1 이상 |
| `serialize` | `false` | `true`면 검증을 한 번에 하나만 돈다. preflight와 batch 검증은 기본으로 동시에 돈다. 포트나 DB처럼 겹치는 자원을 쓰는 저장소가 켠다 |
| `require_ci` | `false` | `true`면 PR마다 등록된 HEAD의 GitHub check가 통과한 PR만 batch에 넣는다. 아래에 설명한다 |
| `respawn` | `["claude", "--dangerously-skip-permissions"]` | 반송을 받을 에이전트가 없을 때 띄우는 명령 |
| `verify` (`[[verify]]`) | 없음. 하나 이상 필요 | 검증 단계. 적은 순서대로 돌고 하나라도 실패하면 그 batch는 실패다 |
| `verify.name` | 필수 | 단계 이름. 겹치거나 `/`를 담거나 `.`으로 시작하면 안 된다 |
| `verify.run` | 필수 | 검증 worktree에서 `sh -c`로 실행하는 명령 |
| `verify.timeout` | `3600` | 초. 넘기면 그 단계는 실패(`시간 초과`)다. 시간 초과는 다시 돌려 보지 않는다 |
| `verify.failed` | 없음 | 선택. 실패한 시험 이름을 뽑는 정규식. 캡처 그룹이 있으면 그 값이, 없으면 일치한 글이 이름이다. 없으면 보고서에 로그의 끝부분이 들어간다 |

**`respawn`의 기본값 `["claude", "--dangerously-skip-permissions"]`는 새 에이전트가 권한 확인 없이 명령을 실행하게 한다.** 무인으로 뜨는 에이전트가 권한 대화상자에서 멈추지 않게 하려는 선택이다. 다른 명령으로 바꾸려면 배열을 바꾼다. 실행 파일과 인자를 나눠 적는다(`respawn = ["claude"]`면 권한 확인을 건너뛰지 않는다). 빈 배열(`respawn = []`)이면 에이전트를 띄우지 않고 그 PR을 사람 차례(`전달 실패`)로 올린다. 새 에이전트는 PR 브랜치의 worktree에서 돈다. 원래 에이전트의 worktree가 남아 있으면 그곳이고, 없으면 `~/.herdr/worktrees/<저장소>/<브랜치>`에 만든다. 사용자의 chute 체크아웃이 그 PR 브랜치를 쥐고 있으면 새 에이전트가 그 폴더에서 돈다. 기본값이면 그곳에서도 권한 확인 없이 실행된다.

**`require_ci`.** 켜면 batch에 넣기 전에 PR마다 등록된 HEAD의 GitHub check를 `gh`로 읽는다. 모두 통과했으면 batch에 넣는다(건너뛴 check와 중립은 통과로 본다). check가 도는 중이거나 아직 하나도 없으면 PR을 `대기(CI 기다림)`으로 두고 이번 batch에서 뺀다. 다음 batch 때 다시 보고, 큐에 CI를 기다리는 PR만 남았으면 30초쯤마다 다시 본다. check가 submit부터(반송 뒤 다시 submit했으면 마지막 반송부터) 10분 넘게 하나도 없으면 현황판에 경고를 한 번 남긴다. CI가 없는 저장소에서 옵션만 켠 경우를 알리기 위해서다. check가 생기거나 PR이 큐에서 빠지거나 옵션을 끄면 경고가 사라진다. 실패했거나 취소됐으면 실패한 check의 이름과 링크를 담아 반송하고 재시도 예산을 한 번 쓴다. 보는 것은 PR 자신의 HEAD에 대한 check뿐이다. 합친 트리는 여전히 로컬 검증이 판정한다. **로컬 검증이 최종이라는 원칙은 그대로이고 이 옵션은 그 앞의 추가 관문이다.** `chute preflight`는 이 옵션과 무관하다.

**`CHUTE_HOME`.** 상태는 `$CHUTE_HOME`(없으면 `$XDG_STATE_HOME/chute`, 그것도 없으면 `~/.local/state/chute`) 아래에 저장소마다 폴더를 두고 쌓인다. `CHUTE_HOME`을 바꿔 쓴다면 주의한다. `chute up`이 `hc`로 띄운 데몬 터미널이 `CHUTE_HOME`을 물려받지 못하면 다른 상태 폴더를 보고 에이전트의 submit을 못 본다. 데몬이 도는 터미널과 에이전트의 셸이 같은 `CHUTE_HOME`을 갖는지 확인한다. `chute status`가 보이는 큐가 에이전트가 본 것과 다르면 이것을 의심한다.

## 사람이 하는 일

### 현황판 읽기

`chute status`(또는 Herdr의 chute 현황판)가 보인다. 현황판은 `chute status --json --all`의 출력만 그린다. 위에서 아래로 다음 순서다.

1. 멈춤과 경고: 기준 커밋이 깨져 큐가 멈췄는지, 데몬이 꺼졌는지, 설정이나 기록의 문제가 있는지.
2. 사람 차례: 사람이 풀어야 하는 PR과 이유(`예산 소진`, `전달 실패`, `포기`).
3. 검증 중인 batch: 기준, PR 목록, 단계, 회차.
4. 대기: 승인 대기, 대기, `대기(CI 기다림)`.
5. 반송: 에이전트에게 돌려보낸 PR과 이유.
6. 오늘의 숫자: 머지, 반송, flaky, batch 수, 머지까지 걸린 시간의 중앙값.

흔들린 시험(flaky)은 에이전트에게 보내지 않고 기록만 한다. 한 단계가 실패한 뒤 같은 트리에서 그 단계만 다시 돌려 통과하면 flaky로 보고 그 batch를 통과로 머지한다. 흔들리는 시험을 새로 만든 PR도 머지될 수 있다.

### 승인, 다시 시도, 빼기

현황판의 버튼이 같은 명령을 실행한다. 터미널에서도 쓸 수 있다.

- `chute approve <pr> [--head <SHA>]`: 승인 대기 PR을 큐에 넣는다. 현황판의 버튼은 사람이 본 HEAD를 `--head`로 넘긴다. 그 사이 PR HEAD가 바뀌었으면 승인하지 않는다.
- `chute retry <pr>`: 사람 차례의 PR을 반송 횟수 0으로 큐에 다시 넣는다.
- `chute drop <pr>`: PR을 큐에서 뺀다.

### batch 보기와 되돌리기

- `chute batches`: 최근 batch 목록을 본다.
- `chute revert <batch> [--no-verify]`: 그 batch의 merge commit들을 역순으로 `git revert -m 1`로 지금의 기본 브랜치 위에 쌓고, 검증한 뒤 push한다. `--no-verify`는 검증을 건너뛴다. 강제 push로 기본 브랜치를 되감지 않는다. 그 뒤에 머지된 변경과 충돌하면 아무것도 push하지 않고 멈춘다. 되돌리기는 사람이 내린 결정이라 에이전트에게 자동으로 알리지 않는다. 되돌린 PR의 커밋은 기본 브랜치의 이력에 남아 있어서 같은 브랜치를 다시 머지해도 변경이 돌아오지 않는다. `chute revert`는 되돌린 PR 목록과 다시 올리는 방법(브랜치에서 revert 커밋을 되돌린 뒤 고친다)을 출력한다.

### 데몬 멈추기

- `chute down`: 지금 도는 batch가 끝난 뒤 데몬이 멈춘다.
- `chute down --now`: 돌던 검증을 취소하고 바로 멈춘다.
- 다시 시작하려면 `chute up`.

`chute daemon`은 `chute up`이 데몬 터미널에서 실행하는 명령이다. 직접 쓸 일은 거의 없다.

## 동작 규칙

- **검증한 트리만 push한다.** 기본 브랜치에 push하는 커밋은 검증 명령이 통과한 바로 그 트리다. 강제 push는 하지 않는다. 그사이 기본 브랜치가 움직여 push가 거절되면 새 기준으로 다시 만든다(3회까지).
- **등록된 HEAD만 머지한다.** submit할 때 PR의 HEAD를 기록하고, batch를 만들 때 PR의 HEAD가 다르면 건너뛰고 에이전트에게 "HEAD가 바뀌었다. 준비되면 다시 submit하라"고 알린다. 예산은 깎지 않는다.
- **`chute.toml`을 고치는 PR은 승인 대기로 간다.** `approval = "agent"`여도 그렇다. 검증 규칙을 바꾸는 변경은 검증이 판정할 수 없다. PR이 검증 명령을 `true`로 바꿔 스스로 통과하는 것을 막는다.
- **기준이 깨지면 큐가 멈춘다.** 단독 실패로 반송하기 전에, 그 기준 커밋이 검증을 통과한 적이 없으면 기준만 검증한다. 기준도 실패하면 아무도 반송하지 않고 큐를 멈춘다(`큐 멈춤`). 기본 브랜치를 직접 push로 깨뜨렸거나 환경이 깨졌을 때 모든 에이전트가 틀린 반송을 받는 것을 막는다.
- 기본 브랜치를 향한 열린 PR만 받는다. draft나 다른 브랜치를 향한 PR은 submit이 거절한다.
- push가 끝나면 `gh`로 PR이 Merged인지 본다. 아니면 현황판에 경고를 남긴다.

## 첫 가동 점검

chute는 검증한 batch를 기본 브랜치에 직접 push하고 PR마다 merge commit을 남겨 GitHub이 PR을 Merged로 표시하길 기대한다. **이 가정은 자동 시험으로 확인되지 않았다.** 시험은 가짜 `gh`와 가짜 `hc`, 로컬 bare 원격으로 도는데, 가짜는 GitHub의 판정을 흉내 낼 뿐이다. 실제 저장소에서 처음 쓸 때 다음 절차로 확인한다.

1. 실제 저장소에서 `chute init`, 설정 커밋과 push, `chute up`까지 마친다.
2. 내용이 사소한 PR을 하나 올린다(문서 한 줄 고치기면 충분하다).
3. 그 브랜치에서 `chute preflight`, 이어서 `chute submit`을 실행한다.
4. `chute status`로 batch가 도는 것을 보고, 끝나기를 기다린다.
5. GitHub의 그 PR 페이지가 **Merged**(보라색)로 표시되는지 본다. `gh pr view <번호> --json state`가 `MERGED`를 내는지로도 확인한다. 기본 브랜치의 이력에 `Merge pull request #N from …` 커밋이 있어야 한다.
6. Merged로 표시되지 않으면 `chute status`의 경고에 "GitHub이 아직 머지된 PR로 보지 않는다"가 나온다. 이때는 이 방식이 이 저장소에서 통하지 않는 것이니 더 쓰지 말고 알린다.

## 한계

- 검증 명령은 `sh -c`로 자기 프로세스 그룹에서 실행된다. 시간 초과나 취소 때 그 그룹 전체를 끝낸다. 명령이 `setsid`나 GNU `timeout`처럼 그룹을 떠나는 프로세스를 만들면 시간 초과 뒤에도 남을 수 있다. 데몬은 시작할 때와 batch가 끝날 때 batch 슬롯 안에서 도는 검증 프로세스를 찾아 정리하지만, 작업 폴더를 슬롯 밖으로 옮긴 프로세스는 정리하지 못한다.
- 실패 재현 명령(보고서의 `재현`)은 `timeout` 명령(GNU 또는 BusyBox)이 있는 환경을 전제로 한다.
- 전달 직후 상태를 쓰기 전에 데몬이 죽으면 에이전트가 같은 반송을 두 번 받을 수 있다. 유실보다 중복을 골랐다.
- 데몬이 강제 종료되면 batch 기록과 이력에 좁은 틈이 생길 수 있다. 큐와 기본 브랜치는 어긋나지 않지만 반송 이벤트가 하나 빠질 수 있다.
- 별도 clone에서 일하는 에이전트의 `chute preflight`는 지원하지 않는다. `preflight`는 아직 push하지 않은 커밋을 검증하므로 사용자의 저장소와 객체 저장소를 공유하는 worktree에서 돌려야 한다(submit은 PR 번호만 쓰므로 어디서든 된다).

## 하지 않는 것

v1이 일부러 뺀 것이다.

- 충돌 예보(기본 브랜치가 전진할 때 작업 중인 에이전트에게 알림)
- 수리 대신 재생성 판단
- 반복 실패 유형 집계와 에이전트 지침 제안
- 위험도에 따른 자동/사람 확인 세분화. `approval`의 켜고 끔과 `chute.toml` 변경 PR의 승인 대기만 있다
- 에이전트의 우회(`gh pr merge`, 기본 브랜치로의 직접 push)를 훅으로 막는 일. 스킬의 규칙으로만 금지한다
- orch 스킬의 병합 대기열을 chute로 바꾸거나 잇는 일
- 머지된 브랜치와 worktree 정리
- 별도 clone에서의 preflight, fork에서 온 PR, 기본 브랜치가 아닌 곳을 향한 PR
- 흔들리는 시험의 근거 관리(기준에서 재현, 만성 표시). 기록과 표시만 한다
- GitHub 외 호스팅, 여러 기기, 원격 기기의 에이전트

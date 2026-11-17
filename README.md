# chute

chute는 herdr 에이전트들의 PR을 묶어 로컬에서 검증하고 기본 브랜치에 머지하는 머지 큐다. GitHub 저장소와 한 사람을 전제로 하고, 검증은 로컬 PC에서 돈다. 같은 저장소를 여러 PC에서 작업하는 방법은 "여러 PC에서 쓰기"에 있다. 설계는 [`docs/plans/chute-v1.md`](docs/plans/chute-v1.md)에 있다.

## 푸는 문제

여러 에이전트가 PR을 쏟아 내는데 머지가 그 속도를 따라가지 못한다. 막히는 곳은 세 군데다.

- PR마다 원격 CI가 끝나기를 기다린다.
- 앞 PR이 머지되면 뒤 PR이 충돌하고, 누군가 알아채서 에이전트에게 다시 맞추라고 해야 한다.
- 실패 원인을 읽고 해당 에이전트에게 전달하는 일을 사람이 한다.

chute는 검증을 로컬에서 묶음(batch) 단위로 한 번에 돌린다. 실패와 충돌은 원인과 함께 만든 에이전트에게 자동으로 돌려보낸다. 사람에게는 사람만 풀 수 있는 것만 남긴다.

## 동작

1. 에이전트가 PR을 올리기 전에 `chute preflight`로 기본 브랜치 + 큐 + 자기 HEAD를 합쳐 검증한다. 합칠 PR이 큐에 없으면 batch와 같은 검사를 한 번 더 하는 것뿐이라 기본으로 검사하지 않고 생략한다(`preflight` 키, 아래 표). 생략하면 종료 코드 0이고 `--json`의 `result`는 `skipped`이며 `head`와 `base`는 그대로 남는다. 단독 실패가 나오면 기준 커밋만 따로 검증해 본다(큐가 이미 검증한 기준은 건너뛴다). 기준도 실패하면 "기준이 깨졌다. 이 커밋 탓이 아니다"로 보고하고 종료 코드 4로 끝난다. 이때 `--json`의 `result`는 `base_broken`이다.
2. PR을 올린 뒤 `chute submit`으로 큐에 넣는다. 그 순간의 PR HEAD가 기록된다.
3. 데몬이 큐에서 PR을 (복구 PR, `--priority` PR, 나머지 순으로 각자 올린 순서대로) 최대 `max_batch`개 가져와 기본 브랜치 위에 합치고 `chute.toml`의 검증 단계를 돌린다.
4. 통과하면 검증한 바로 그 트리를 기본 브랜치에 push한다. 실패하면 batch를 반으로 나눠 범인을 찾는다. 범인은 보고서와 함께 에이전트에게 돌아간다. 에이전트가 없으면 기본 설정에서는 띄우지 않고 반송 줄을 submit한 터미널에 대신 보낸다. `respawn`에 명령을 넣었으면 그 명령으로 새로 띄운다.
5. 머지에 성공하면 submit한 터미널에 결과 한 줄을 `hc send`로 보낸다. 작업자가 스스로 submit했으면 작업자가, 오케스트레이터가 submit했으면 오케스트레이터가 받는다. 받을 터미널이 없으면 기록만 남고 `chute events`로 따라잡는다. `chute status`나 현황판으로도 본다.

batch가 기다리기에는 너무 느린 단계(예: e2e)는 `[[post_verify]]`에 둔다. 그 단계는 머지가 끝난 뒤 기본 브랜치의 tip에서 큐와 따로 돈다. 아래 "머지 뒤 검사"를 본다.

orch 같은 오케스트레이터 아래에서 쓰는 법은 "오케스트레이터와 함께 쓰기"에 있다.

## 설치

Claude Code에서는 plugin으로 설치한다. plugin이 chute 스킬과 막는 훅을 준다.

```
/plugin marketplace add Soochol/chute
/plugin install chute@chute
```

plugin은 실행 파일을 빌드하지 않는다. `chute` 실행 파일은 따로 설치한다.

```sh
cargo install --git https://github.com/Soochol/chute
```

저장소를 받아 개발할 때는 `cargo install --path .`로 설치하고, `cargo build --release`로 빌드하고 `cargo test`로 시험한다. 빌드한 실행 파일은 `target/release/chute`다.

필요한 것은 다음과 같다.

- `git`
- `gh`(로그인한 상태)
- `hc`(herdr-core CLI). 에이전트에게 메시지를 보내고 새 에이전트와 데몬 터미널을 띄우는 데 쓴다.
- Herdr. 현황판 mod를 보려면 필요하다.

### plugin이 하는 일

- 스킬은 `skills/chute/SKILL.md`다. 에이전트에게 `chute --skill`의 설명서를 읽으라고 알린다. 이 파일이 스킬의 유일한 원본이고 `chute install`도 이 파일을 쓴다.
- 훅은 Bash 호출 전에 `chute guard`를 실행한다. 대상 저장소의 루트에 `chute.toml`이 있을 때만 다음을 막고, `chute preflight`와 `chute submit`을 쓰라고 안내한다. `chute.toml`이 없는 저장소에서는 아무것도 막지 않는다.
  - `gh pr merge`와 PR 머지 API(`gh api .../pulls/<번호>/merge`)
  - chute가 관리하는 저장소의 기본 브랜치로 가는 `git push`. `git push origin main`, `git push origin HEAD:main`, `git push --all`, 기본 브랜치를 체크아웃한 상태의 맨 `git push` 같은 형태다. 맨 `git push`는 `push.default`와 upstream이 정하는 브랜치를 본다. 기본 브랜치는 push 대상 원격의 `HEAD`(`refs/remotes/<원격>/HEAD`)가 가리키는 브랜치이고, 그것이 없으면 `main`과 `master`다. 그 원격에 `HEAD`가 없는 clone에서 기본 브랜치가 다른 이름이면 `git remote set-head <원격> -a`로 만든다.
  - push 대상 원격은 git이 고르는 순서로 정한다. 명령에 적힌 원격(`--repo`도 같다), 브랜치의 `branch.<이름>.pushRemote`, `remote.pushDefault`, 브랜치의 upstream 원격, `origin` 순이다. 그 원격이 chute가 관리하는 저장소가 아니면(fork, 미러, 백업) 막지 않는다. 원격 URL의 `owner/name`이 chute가 관리하는 저장소와 같으면 관리하는 저장소로 본다. 어느 원격으로 가는지 알아내지 못하면 막고 이유를 보인다. 안내 문구의 `require_head`는 push 대상 원격의 기본 브랜치에 있는 `chute.toml`을 따른다.
  - PR 브랜치로의 push는 막지 않는다.
- chute 데몬이 직접 하는 push는 Claude Code의 도구 호출이 아니므로 훅이 보지 않는다. 데몬이 머지하는 데는 영향이 없다.
- 훅은 실수를 막는 장치이지 보안 경계가 아니다. 셸 별칭, 변수, 스크립트 안의 명령은 따라가지 않는다. 중괄호 확장, 함수 정의, 파이프나 `&` 뒤의 `cd`, 작은따옴표 안의 `$(...)`, 큰따옴표 안의 백틱도 따라가지 못한다.
- `chute` 실행 파일이 `PATH`에 없으면 훅은 호출을 막지 않고 지나간다. `chute.toml`이 있는 저장소에서만 "실행 파일이 없어 막지 못한다"는 안내를 사용자에게 보인다. 훅이 입력을 읽거나 판정하지 못할 때도 호출을 막지 않고 이유만 보인다.

### `chute install`이 하는 일

`chute install`은 plugin이 주지 않는 것만 쓴다. Codex 스킬(`~/.codex/skills/chute/SKILL.md`)과 Herdr 현황판 mod(`~/.config/herdr-app/mods/chute/`)다. Claude Code 스킬은 쓰지 않는다. plugin의 스킬과 같은 스킬이 두 번 뜨는 것을 막기 위해서다. 이전 버전의 `chute install`이 쓴 `~/.claude/skills/chute/SKILL.md`가 남아 있으면 plugin과 겹치니 직접 지운다. `chute install`이 그 파일을 알려 준다.

### Herdr 탭 막대 버튼과 패널 레이아웃(선택)

`chute install --layout`은 위의 파일에 더해 Herdr 탭 막대에 `● chute` 버튼을 달고 오른쪽 패널에 chute 현황판을 넣은 레이아웃을 설치한다. 옵션 없는 `chute install`은 이 파일을 쓰지 않는다.

- 그 PC의 Herdr 기본 파일(`layouts/main.json`, `mods/tabs/main.js`)을 **읽어** 복제본을 만든다. 기본 파일은 고치지 않는다. 쓰는 파일은 `layouts/chute.json`과 `mods/chute-tabs/`(`main.js`, `chute.js`, `mod.json`)다. 버튼 동작과 점 색은 `mod/chute-tabs/chute.js`가 정한다.
- 쓴 뒤 `herdr-app do layout.switch '{"name":"chute"}'`로 chute 레이아웃으로 바꾼다. Herdr가 떠 있지 않거나 명령이 실패하면 설치는 성공으로 두고 바꾸는 명령을 알려 준다. `ctrl-shift-l`로도 고른다.
- 이 파일들은 chute가 만드니 고치지 않는다. 다음 `chute install --layout`이 덮어쓴다. 같은 이름의 다른 파일(chute의 것이 아닌 `layouts/chute.json`이나 `mods/chute-tabs/main.js`)이 있으면 덮어쓰지 않고 알린다.
- 복제본은 Herdr가 따라 고쳐 주지 않는다. Herdr를 업데이트한 뒤에는 `chute install --layout`을 다시 실행한다. 옵션 없는 `chute install`은 지금 기본 파일로 다시 만들어 본 결과와 설치된 파일을 견주고, 다르면 경고하고 파일은 쓰지 않는다. Herdr가 기본 탭 막대를 바꿔 바꿔 끼울 줄을 찾지 못하면 설치를 거절하고 이유를 알린다.
- 단축키는 `keys.json`을 고치지 않고 안내만 한다. 쓰려면 `keys.json`에 다음 구역을 직접 더한다.
  ```json
  "layout:chute": { "ctrl-shift-g": "layout.toggle:panel", "ctrl-shift-u": "layout.toggle:chute" }
  ```
- 되돌리려면 `chute uninstall --layout`을 실행한다. chute 레이아웃이 보이면 `main`으로 바꾼 뒤 chute가 쓴 파일(`layouts/chute.json`, `mods/chute-tabs/`와 Herdr가 그 폴더에 넣은 `gpui-kit.d.ts`·`herdr.d.ts`·`jsconfig.json`)만 지운다. 다른 파일이 남은 폴더는 두고 알린다. 다시 실행해도 같은 결과다.
- Herdr가 mod용 탭 막대 버튼 API를 주면 이 복제본을 지우고 갈아탄다.

## 처음 쓰기

1. 대상 저장소에서 `chute init`을 실행한다. `chute.toml` 초안을 쓰고 저장소를 상태 폴더에 등록한다.
2. `chute.toml`의 `[[verify]]`를 이 저장소의 검사 명령으로 고치고 커밋해 기본 브랜치에 올린다. chute는 `<원격>/<기본 브랜치>`(원격이 하나뿐이면 보통 `origin/<기본 브랜치>`)에 올라간 `chute.toml`만 읽는다. git 원격이 여러 개인 저장소는 아래 "원격이 여러 개일 때"를 본다.
3. `chute install`을 실행한다. Codex 스킬 파일(`~/.codex/skills/chute/SKILL.md`)과 현황판 mod(`~/.config/herdr-app/mods/chute/`)를 쓴다. `~/.codex`가 없으면 스킬을 건너뛰고, Herdr 홈이 없으면 mod를 건너뛴다. `HERDR_APP_HOME`이 있으면 그 아래에 mod를 쓴다. 다시 실행해도 같은 결과이고, 무엇을 썼고 무엇이 그대로인지 출력한다. Claude Code의 스킬과 훅은 위의 plugin이 준다.
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

## 오케스트레이터와 함께 쓰기

orch 같은 오케스트레이터가 작업자 에이전트들을 부리면 머지를 오케스트레이터가 실행한다. 설계는 [`docs/plans/chute-v3-orch.md`](docs/plans/chute-v3-orch.md)에 있다.

- **작업자**는 구현, 시험, 리뷰, `chute preflight`(필수), PR 생성까지 하고 PR 번호와 HEAD를 보고한다. submit하지 않는다. 확정된 main(`origin/main`)에만 맞추고, 큐에 있는 다른 PR의 diff에 맞추지 않는다. preflight의 조합 실패 상대가 아직 큐에 있으면 고치지 않고 blocked로 보고한다. 반송을 고친 뒤에도 submit하지 않고 새 HEAD를 다시 보고한다.
- **오케스트레이터**는 확인한 HEAD로 `chute submit <pr> --head <HEAD> --agent <작업자 터미널>`을 실행한다. 이 submit이 머지 실행이다. 반송은 `--agent`의 작업자에게 가고, 결과는 submit한 터미널에 온다.
- `require_head = true`를 켜면 `--head` 없는 submit을 거절한다. 허브가 아닌 PC에서 넘긴 submit도 거절한다. `chute --skill`은 지금 폴더가 등록된 저장소이고 기본 브랜치의 설정이 `require_head = true`이면 설명서 맨 앞에 "이 저장소에서는 submit하지 말고 PR과 HEAD를 보고한다" 줄을 붙인다. 등록되지 않았거나 설정을 읽지 못하면 설명서만 낸다.
- 작업자가 없을 때 chute는 새 에이전트를 띄우지 않고 반송 줄을 submit한 터미널에 대신 보낸다(`delivered=unreachable`). PR은 `bounced`에 그대로 있다. 반송 한도를 넘거나 `chute giveup`을 하면 PR을 큐에서 빼고 submit한 터미널에 알린다. 다음은 오케스트레이터가 정한다.

오케스트레이터가 쓰는 명령과 옵션은 다음과 같다. 에이전트용 설명서(`chute --skill`)의 같은 이름 절이 더 자세하다.

| 명령 | 하는 일 |
|---|---|
| `chute submit --priority` | 급한 PR로 표시해 batch에 먼저 넣는다. 사용자가 허락한 범위에서만 쓴다. chute는 누가 쓰는지 막지 않는다 |
| `chute submit --after <PR>` | 그 PR이 머지된 뒤에 batch에 넣는다. 그 PR이 반송, 큐에서 빠짐이 되면 잡혀 있고 `waiting` 알림이 간다 |
| `chute submit --repair` | 멈춘 큐를 푸는 복구 PR로 넣는다. 아래 "복구 PR" |
| `chute events [--since <시각>] [--pr <n>] [--limit <n>] [--json]` | history의 사건을 시간 순으로 보인다. 놓친 알림을 따라잡는다. 허브가 다른 PC면 허브의 것을 보인다. `--since`는 epoch 밀리초, RFC 3339 시각, 상대 시간(`30m`, `2h`, `1d`)이다. 같은 시각을 포함하고 `--limit`의 기본값은 500이다 |
| `chute flaky [--days <n>] [--json]` | 흔들린 시험의 장부. 시험마다 횟수, 마지막 시각, 근거, 만성 여부. 허브로 넘긴다 |
| `chute gc [--dry-run] [--json]` | 오래된 파일을 정리한다. 이 PC만 다룬다. 아래 "정리" |

### 알림

머지 결과는 알림 한 줄로 submit한 터미널에 간다. 형식은 `chute 결과 <kind> slug=<owner/name> at=<epoch ms> <key>=<value> … <한국어 한 문장>`이다. 글자, 숫자, 공백, `.,:/_-+=@%`만 쓰고 SHA는 12자다. `at`은 그 사건의 history 시각이라 `chute events --since <at - 1000>`처럼 조금 앞선 값으로 같은 사건을 찾는다(`flaky` 알림의 `at`은 사건과 몇 밀리초 어긋날 수 있다).

| kind | 받는 쪽 | 뜻 |
|---|---|---|
| `merged` | submit한 터미널 | PR이 머지됐다 |
| `bounced` | submit한 터미널(작업자와 같으면 보내지 않는다. 작업자에게 닿지 않아 반송 줄을 대신 보낼 때는 같아도 보낸다) | 반송 요약. 작업자는 반송 줄과 보고서를 따로 받는다. `delivered=unreachable`이면 작업자에게 닿지 않아 이 줄이 반송 줄을 대신한다 |
| `removed` | submit한 터미널(작업자와 같아도 보낸다) | PR을 큐에서 뺐다(`budget`, `gave_up`). 다시 넣으려면 다시 submit한다 |
| `halted` | 그때 큐에 있던 모든 항목의 submit한 터미널 | 큐가 멈췄다. 단계, 로그, 깨진 커밋을 chute가 push했는지가 든다 |
| `resumed` | 멈춤이 풀릴 때 큐에 있던 항목과 복구 PR의 submit한 터미널 | 멈춤이 풀렸다 |
| `reverted` | 되돌린 PR을 submit한 터미널 | 사람이 batch를 되돌렸다 |
| `waiting` | `--after`로 잡힌 PR을 submit한 터미널 | 기다리는 PR이 머지되지 못했다 |
| `flaky` | batch의 PR을 submit한 터미널 | 흔들림이 만들어졌거나 만성이다 |
| `post_failed` | 처음 실패한 머지의 PR을 submit한 터미널과 그 작업자(같은 터미널이면 한 번) | 머지 뒤 검사가 기본 브랜치에서 실패했다. 아래 "머지 뒤 검사" |
| `post_fixed` | `post_failed`를 받은 터미널 | 실패했던 머지 뒤 검사가 다시 통과했다 |

알림은 사건이 기록된 뒤에 한 번만 보낸다. 데몬이 알림 도중 죽으면 한 번 빠질 수 있고, 같은 알림이 두 번 가는 것보다 낫다고 보았다. 빠진 알림은 `chute events`에서 `notified`의 `delivered`가 `sent`가 아닌 줄(사람용 출력에서는 `(놓친 알림)`)로 찾는다. `same_as_agent`는 작업자와 같은 터미널이라 반송 줄만 보낸 것이므로 갔다고 보고 표시를 붙이지 않는다. 알림을 못 보내도 큐 상태는 바뀌지 않는다.

### 복구 PR

기본 브랜치가 검증을 통과하지 못하면 큐가 멈춘다(`큐 멈춤`). 사람이 직접 push해서 풀지 않는다. 오케스트레이터가 에이전트에게 복구 PR을 만들게 하고 `chute submit <pr> --head <HEAD> --repair`로 넣는다.

- 큐가 멈춰 있을 때(또는 `post_verify_hold`가 머지 뒤 검사 실패로 PR을 잡아 두는 동안)만, 한 번에 하나만 받는다. `--after`와 함께 쓸 수 없다. `--head`와 승인 규칙은 일반 submit과 같다. 복구 PR의 `chute.toml`도 일반 PR과 같아서, 사람 모드이거나 PR의 `chute.toml`을 읽지 못하거나 지울 때만 사람 승인을 기다린다.
- 데몬은 멈춘 채로도 복구 PR 하나만 지금 main 위에서 검증한다. 다른 PR과 묶지 않는다. 통과하면 push하고 멈춤을 풀고 `merged`와 `resumed`를 보낸다.
- 실패하거나 충돌하면 보통 반송처럼 작업자에게 보고서가 가고 예산을 한 번 쓴다. 큐는 계속 멈춰 있다. 고친 HEAD로 다시 `--repair` submit을 하면 같은 항목이 들어간다.
- 멈춘 사이 누가 main에 직접 push해 기준이 바뀌면 멈춤이 풀리고 복구 PR은 일반 PR처럼 batch를 고를 때 맨 앞에 선다.
- 멈춤의 이유는 `chute status --json`의 `blocked`(`step`, `log`, `pushed_by_chute`, `rechecked`)와 `halted` 알림에 있다.
- chute가 push한 main이라도 같은 기준 위에서 서로 다른 PR 둘이 연달아 단독 실패하면 두 번째를 반송하기 전에 그 기준을 다시 검증한다. 실패하면 반송하지 않고 멈추고 `halted`에 `rechecked=yes`, 이미 반송된 첫 PR은 `suspect`로 적는다. 환경이 망가졌을 때 모든 PR이 틀린 반송을 받는 것을 막는다.

### 정리

데몬이 시작할 때와 한 시간마다, 그리고 데몬이 없는 PC를 위해 `chute preflight`와 `chute deliver`의 끝(한 시간에 한 번)에 같은 정리를 한다. `chute gc`로 직접 할 수도 있고 `--dry-run`은 지울 것과 크기만 보인다. 보관 기간은 `[retention]`이다.

지우는 것:

- batch 검증 로그와 preflight 로그: `logs_days`(기본 7일)보다 오래된 것
- 반송 보고서(그 PR이 큐에 없을 때)와 preflight 보고서: `reports_days`(기본 30일)보다 오래된 것
- batch 실패 참조(`refs/chute/failed/pr<n>-<batch>`): 허브에서는 그 PR이 큐에서 빠지면(나이는 보지 않는다), 다른 PC에서는 batch id의 시각이 `reports_days`보다 오래되면
- preflight 실패 참조: `reports_days`보다 오래된 것
- `max_parallel`을 넘는 번호의 preflight 슬롯 가운데 아무도 쓰지 않는 것

남기는 것:

- 기록: `history.jsonl`, `state.json`, `batch.json`, `revert.json`
- 지금 도는 batch의 로그와 큐 멈춤이 가리키는 로그(`blocked.log`)
- **respawn이 만든 worktree.** 에이전트가 커밋하지 않은 작업이 남아 있을 수 있고 Herdr 작업 공간이라 지우지 않는다.

다른 PC에 남은 것은 그 PC의 chute가 같은 규칙으로 지운다. `chute gc`는 이 PC의 상태 폴더만 다룬다. 정리가 실패해도 데몬과 preflight는 멈추지 않고 상태에 `gc` 경고만 남는다.

`respawn`의 기본값이 `[]`가 되기 전에 chute가 띄운 worktree가 `~/.herdr/worktrees/<저장소>/<브랜치>`에 남아 있을 수 있다. 손으로 지우는 법은 다음과 같다.

1. 대상 저장소에서 `git worktree list`로 목록을 본다. 지울 폴더가 이미 머지됐거나 버려도 되는 PR의 브랜치인지 확인한다.
2. 그 폴더에서 `git status`로 커밋하지 않은 작업이 없는지 본다. 있으면 먼저 살리거나 버릴지 정한다.
3. 대상 저장소에서 `git worktree remove <폴더>`를 실행한다. 커밋하지 않은 변경이 있으면 git이 거절한다. 그 변경을 버려도 확실할 때만 `--force`를 붙인다.
4. 폴더가 이미 지워져 목록에만 남았으면 `git worktree prune`을 실행한다.

### 샌드박스는 아직 없다

검증 명령을 샌드박스(`bwrap` 같은)에서 돌리는 선택지는 설계만 있고 구현하지 않았다. 검증 명령은 지금 사용자 권한으로, 네트워크와 홈 폴더에 닿는 채로 돈다. 설계는 `docs/plans/chute-v3-orch.md`의 4.14에 있다.

## 여러 PC에서 쓰기

같은 저장소를 여러 PC에서 작업해도 대기열은 하나다. 저장소마다 PC 한 대가 **허브**가 되어 데몬과 대기열(`state.json`)과 검증을 맡는다. 다른 PC에서 `chute submit`, `chute status`, `chute approve` 같은 명령을 실행하면 chute가 `hc exec --on <허브>`로 허브에 넘기고 결과를 그대로 보인다. 에이전트가 쓰는 명령과 설명서는 허브가 어디든 같다. 머신 사이의 연결은 herdr(`hc machine`)가 맡는다. chute가 따로 네트워크를 만들지 않는다. 설계는 [`docs/plans/chute-v2-multi-machine.md`](docs/plans/chute-v2-multi-machine.md)에 있다.

PC가 하나면 이 절은 필요 없다. `chute hub set`을 하지 않은 PC는 스스로 허브이고 v1처럼 돈다.

### 허브 고르기

허브는 저장소마다 하나다. 다음을 만족하는 PC를 고른다.

- 거의 항상 켜져 있다. 허브가 꺼지면 다른 PC의 submit이 멈춘다(아래 "허브가 꺼졌을 때").
- 검증 명령을 돌릴 만큼 빠르다. batch 검증은 허브에서만 돈다. 검증을 여러 PC에 나누지 않는다.
- `git`, `gh`(로그인한 상태), `hc`가 있고, 저장소를 받아 두었고, 기본 브랜치에 push할 권한이 있다.

### 설정

1. **`hc machine add`를 두 방향으로 한다.** 허브에서 각 PC를 더하고, 각 PC에서 허브를 더한다.
   ```sh
   # 허브에서: 각 PC마다
   hc machine add <PC의 ssh 주소>
   # 각 PC에서
   hc machine add <허브의 ssh 주소>
   ```
   허브는 반송을 PC에 있는 에이전트에게 보내려고 PC에 닿아야 하고, PC는 명령을 허브로 넘기려고 허브에 닿아야 한다. 한쪽만 더하면 명령은 가도 반송이 못 돌아오거나, 반송은 가도 submit이 못 간다. `hc machine ls`로 상태가 연결됨인지 본다. 로그인이 필요하다고 나오면 `hc machine login <이름>`을 실행한다. 머신 이름은 보는 PC마다 다를 수 있다. 같은 머신도 PC마다 다르게 부를 수 있으므로 아래 명령에는 그 PC의 `hc machine ls`에 나온 이름을 쓴다.
2. **각 PC에서 `chute`를 로그인 환경의 `PATH`에 둔다.** chute가 `hc exec`로 상대 PC에서 `chute`를 실행하므로 그 PC의 로그인 셸에서 `chute`가 찾아져야 한다(`cargo install`이 `~/.cargo/bin`에 두면 그곳이 로그인 셸의 `PATH`에 있는지 본다). 찾지 못하면 "그 머신의 로그인 환경 PATH에 chute가 있는지 확인한다"가 나온다. 모든 PC의 chute는 같은 버전이어야 한다. `chute hub set`이 `chute --version`을 견주고 다르면 거절한다.
3. **각 PC에서 `chute init`을 실행한다.** 그 PC의 저장소 clone에서 한다. 기본 브랜치에 `chute.toml`이 이미 있으면 초안을 쓰지 않고 이 PC의 상태 폴더에 저장소를 등록만 한다. 다른 PC의 `chute hub set <허브>`가 허브에도 저장소가 등록돼 있는지 확인하므로 허브에서도 해 둔다.
4. **허브가 아닌 PC에서 `chute hub set <허브>`를 실행한다.** `<허브>`는 그 PC의 `hc machine ls`에 나온 허브의 이름이다. 허브가 연결돼 있는지, 같은 버전의 `chute`가 있는지, 저장소가 등록돼 있고 스스로를 허브로 아는지 확인한 뒤 이 PC가 허브를 따르게 한다. 이 PC의 큐에 항목이 남아 있으면 거절한다. 허브에서는 아무것도 하지 않는다.
5. **`chute hub`로 확인한다.** 각 PC에서 실행한다. 출력은 다음 가운데 하나다.
   ```
   허브: 이 PC (desk-host)
   허브: desk (연결됨)
   허브: desk
   닿지 않는다: 머신 `desk`의 상태가 unreachable이다
   ```
   허브에서는 첫 줄, 다른 PC에서는 둘째 줄이 나와야 한다. 셋째와 넷째 줄은 허브에 닿지 않을 때다. `--json`은 같은 내용을 JSON으로 낸다. 허브 설정은 PC마다 따로 저장되는 로컬 설정이고, 저장소의 `chute.toml`에는 적지 않는다.

그 뒤로 에이전트는 어느 PC에서나 `chute preflight`와 `chute submit`을 쓴다. 허브가 다른 PC일 때의 차이는 `chute --skill`의 설명서에 있다. 요약하면 다음과 같다.

- submit은 이 PC의 로컬 HEAD를 허브로 함께 넘기고, 허브는 GitHub의 PR HEAD와 다르면 거절한다(종료 코드 1). push한 뒤에 submit해야 한다. 같은 확인을 허브에서 직접 하려면 `chute submit --head <SHA>`를 쓴다.
- 반송은 submit한 에이전트가 있는 PC로 간다. 보고서는 그 PC의 상태 폴더에 풀리므로 반송 한 줄의 경로는 에이전트의 PC에서 그대로 맞다. 에이전트의 PC에 닿지 않으면 반송 줄을 submit한 터미널에 대신 보내고(`delivered=unreachable`) PR은 `bounced`에 둔다. 허브가 대신 에이전트를 띄우지 않는다.
- `chute preflight`는 호출한 PC에서 돈다. 큐만 허브에서 읽는다. 허브에 닿지 않으면 종료 코드 2로 멈춘다.
- 현황판은 어느 PC에서든 허브의 상태를 그린다. 허브가 다른 머신이면 저장소 카드에 허브의 호스트 이름이 나오고, 허브에 닿지 않으면 "Hub unreachable"과 이유가 나온다.

### 허브 옮기기

옛 허브를 멈추고 새 허브가 될 PC에서 옛 허브의 대기열을 받는다. 옛 허브가 켜져 있고 연결돼 있어야 한다.

```sh
# 옛 허브에서
chute down
# 새 허브가 될 PC에서 (옛 허브의 hc 이름)
chute hub take <옛 허브>
# 새 허브에서
chute up
```

`chute hub take`는 옛 허브에 허브를 넘기라고 하고(옛 허브는 데몬이 돌고 있으면 거절하니 먼저 `chute down`) 큐 항목, 이력, batch 기록을 가져온다. 다시 실행해도 같은 결과가 된다. 보고서와 검증 로그는 옮기지 않는다. 옛 허브에 남아 있다. 이 PC의 큐에 다른 항목이 있으면 거절한다.

다른 PC는 아무것도 하지 않아도 된다. 옛 허브가 "나는 허브가 아니다"라고 답하면 다음 명령이 새 허브를 따라간다. 단 그 PC가 새 허브를 `hc machine add`했어야 한다. 하지 않았다면 "허브 머신(…)을 hc가 모른다" 메시지가 나오고, 그 PC에서 새 허브를 더하면 된다.

### 허브가 꺼졌을 때

다른 PC에서 submit이나 approve 같은 명령은 종료 코드 2로 끝나고 "허브 `<이름>`에 닿지 않는다(…). 명령은 기록되지 않았다"를 낸다. 이 PC에 따로 대기열을 만들지 않으므로 허브가 다시 켜지면 그대로 이어진다. `chute status`와 현황판은 허브에 닿지 않는다고 보이고 큐를 비워 보인다. 에이전트는 설명서대로 사용자에게 알리고 기다린다. 명령이 허브에 갔는지 알 수 없다는 메시지("명령이 기록됐는지 알 수 없다")가 나오면 `chute status`로 확인한다.

허브가 영영 돌아오지 않으면 대기열을 옮길 수 없다. 새 허브로 쓸 PC에서 `chute hub set --local`을 실행하고(이 PC를 허브로 되돌린다), 다른 PC에서 `chute hub set <새 허브>`를 실행하고, 새 허브에서 `chute up`을 실행한다. 옛 대기열은 잃는다. PR은 GitHub에 남아 있으므로 에이전트가 다시 submit하면 된다.

### 허브가 둘이 되는 실수

새 PC에서 `chute hub set <허브>`를 잊으면 그 PC도 스스로를 허브로 안다. 이 PC의 submit은 그 PC의 대기열에 들어가고 데몬도 따로 돈다. 대기열이 둘이 되지만 기본 브랜치가 깨지지는 않는다. 두 데몬 모두 검증한 트리를 강제 push 없이 올리고, 먼저 올라간 쪽 때문에 push가 거절되면 새 기준으로 다시 만든다. 두 쪽이 같은 PR을 따로 검증할 수 있어 시간이 낭비되고 한쪽은 반송을 엉뚱하게 받을 수 있다.

알아차리는 법: 각 PC에서 `chute hub`를 실행한다. "허브: 이 PC"가 둘 이상의 PC에서 나오면 허브가 둘이다. 허브가 다른 PC인 저장소의 `chute status`에는 `허브: <이름>` 줄이 있다. 그 줄이 없는 PC는 스스로를 허브로 아는 것이다. 현황판도 저장소 카드에 허브를 보인다.

고치는 법: 허브로 둘 PC를 정한다. 다른 PC에서 `chute down`으로 데몬을 멈추고, 그 PC의 큐가 비기를 기다린다(머지되거나 `chute drop`으로 뺀다). 그다음 `chute hub set <허브>`를 실행한다. 큐에 항목이 남아 있으면 허브를 옮기면 잃는다는 이유로 거절한다.

### 신뢰 범위

herdr의 머신 연결은 ssh다. 허브에 명령을 넘기는 PC는 허브에 그 계정으로 셸을 가진 것과 같고, 허브도 각 PC에 셸을 가진다. 이 위에서 chute가 권한을 나누어도 그 사람은 `hc exec --on <허브>`로 `state.json`을 직접 고칠 수 있다. 그래서 chute는 **한 사람이 쓰는 여러 PC**를 대상으로 하고, 사람을 구별하지 않으며 승인 권한을 나누지 않는다. 서로 셸을 내주는 것을 받아들인 여러 사람도 같은 방식으로 쓸 수 있지만, `approval = "human"`의 승인은 어느 PC의 현황판에서 눌러도 허브로 넘어가 같은 효과를 낸다. 허브와 각 PC가 서로 믿는 범위 밖의 사람과는 쓰지 않는다.

### 허브가 다른 PC일 때 사람이 하는 일

- `chute revert`는 허브에서 실행한다. 다른 PC에서 실행하면 종료 코드 2로 "revert는 허브에서만 실행한다"를 내고, 허브에서 실행할 `hc run --on <허브> --cwd <경로> chute revert <batch>` 명령을 보인다. 되돌린 트리의 검증이 `hc exec`의 제한 시간을 넘을 수 있어 넘기지 않는다.
- `chute daemon`도 허브에서만 돈다. 다른 PC에서는 `chute up`이 허브에 데몬을 띄운다.
- `chute approve`, `chute drop`, `chute batches`, `chute giveup`, `chute up`, `chute down`, `chute status`, `chute events`, `chute flaky`는 어느 PC에서 실행해도 허브로 넘어간다.
- `chute init`, `chute install`, `chute hub`, `chute gc`, `chute --skill`은 그 PC의 일이다. 허브로 넘기지 않는다. 다른 PC에 남은 로그와 보고서는 그 PC에서 `chute gc`로 지운다.

### 첫 실제 가동에서 확인할 것

여러 PC의 시험은 가짜 `hc`가 다른 머신을 흉내 내는 로컬 환경에서 돌았다. 실제 머신 사이에서는 다음이 확인되지 않았다. 처음 쓸 때 본다.

1. **다른 머신 에이전트의 터미널 주소.** 현황판에서 다른 머신의 에이전트를 누르면 `terminal.peek`에 `desk/t1ru` 같은 `<머신 이름>/<터미널>` 주소를 넘긴다. Herdr 앱이 이 형식을 받는지 확인하지 못했다. 눌러도 터미널이 열리지 않으면 알린다.
2. **허브가 다른 머신일 때의 현황판.** 상태 폴더 감시는 이 PC의 폴더만 지켜보므로 허브가 다른 머신인 저장소는 5초마다 허브를 읽는다. 허브의 변화가 몇 초 안에 보이는지, 허브를 끄면 "허브에 닿지 않는다"로 바뀌고 켜면 돌아오는지 본다.
3. **`chute`의 `PATH`.** 허브와 각 PC에서 `hc exec --on <이름> -- chute --version`이 두 머신 모두 같은 버전을 내는지 본다.
4. **반송이 다른 PC의 에이전트에게 닿는지.** 다른 PC의 에이전트가 일부러 실패하는 PR을 submit해서, 반송 한 줄이 그 PC의 터미널에 입력되고 보고서 경로가 그 PC에 있는지 본다. 에이전트가 끝난 PC면 `respawn`이 비어 있을 때는 새 에이전트가 뜨지 않고 반송 줄이 submit한 터미널에 `delivered=unreachable`로 오는지, `respawn`에 명령을 넣었을 때는 chute가 그 PC에서 새 에이전트를 띄우는지도 본다.
5. **큰 반송.** 보고서 묶음이 `hc exec`의 stdin 한도를 넘으면 git bundle이 빠지고 로그가 잘린다. 검증 로그가 큰 저장소에서 반송 보고서가 읽을 만한지 본다.

## `chute.toml`

대상 저장소의 루트에 둔다. 기본 브랜치의 것을 읽는다. PR이나 batch 안의 `chute.toml`은 읽지 않는다.

```toml
approval = "agent"
budget = 3
max_batch = 8
serialize = false
require_ci = false
require_head = false
max_parallel = 2
preflight = "when_queued"
env_check = []
reuse_verified = true
reuse_hours = 24
reuse_other_machines = false
post_verify_interval = 0
post_verify_hold = false
respawn = []
# remote = "origin"

[retention]
logs_days = 7
reports_days = 30

[cache]
seed = []

[flaky]
probe_repeats = 20
probe_seconds = 180
chronic = 3
chronic_days = 30

[[verify]]
name = "test"
run = "cargo test"
timeout = 3600
failed = 'test (\S+) \.\.\. FAILED'
# probe = "cargo test {tests} -- --exact"
# paths = ["src/", "Cargo.toml"]

# 머지 뒤에 기본 브랜치에서 돌릴 단계. 없으면 머지 뒤 검사는 꺼져 있다.
# [[post_verify]]
# name = "e2e"
# run = "npm run e2e"
# timeout = 1800
```

| 키 | 기본값 | 뜻 |
|---|---|---|
| `approval` | `"agent"` | `"agent"`면 submit한 PR이 바로 큐에 들어가 자동으로 머지된다. `chute.toml`을 고치는 PR도 같다. 다만 PR의 `chute.toml`을 이 chute가 읽지 못하거나 PR이 `chute.toml`을 지우면 머지 뒤 큐가 서므로 `"agent"`여도 승인 대기로 간다. `"human"`이면 `승인 대기`에 머물다가 사람이 승인해야 큐에 들어간다. 승인은 검증 뒤가 아니라 큐에 들어가기 전의 관문이다. 승인은 HEAD에 묶인다. 새 커밋으로 다시 submit하면 다시 승인받는다 |
| `budget` | `3` | PR마다 연속 반송 한도. 반송을 이만큼 보낸 뒤 또 실패하면 큐에서 빼고 submit한 터미널에 알린다. 다시 submit하면 0부터 센다 |
| `max_batch` | `8` | 한 batch의 PR 수 상한. 1 이상 |
| `serialize` | `false` | `true`면 검증을 한 번에 하나만 돈다. preflight와 batch 검증은 기본으로 동시에 돈다. 포트나 DB처럼 겹치는 자원을 쓰는 저장소가 켠다 |
| `require_ci` | `false` | `true`면 PR마다 등록된 HEAD의 GitHub check가 통과한 PR만 batch에 넣는다. 아래에 설명한다 |
| `require_head` | `false` | `true`면 `--head` 없는 submit을 거절한다 |
| `max_parallel` | `2` | 동시에 도는 preflight 수의 상한(슬롯 수의 상한). 1 이상 |
| `preflight` | `"when_queued"` | `chute preflight`가 검사를 돌리는 때. `"when_queued"`는 합칠 PR(큐의 `queued`와 `in_batch` 가운데 자기 브랜치가 아닌 것)이 있을 때만 검사하고, `"always"`는 늘, `"never"`는 한 번도 검사하지 않는다. 생략하면 검사 슬롯을 잡지 않고 종료 코드 0으로 끝나며 `result`는 `skipped`, 이유는 `skip_reason`(`no_queue`, `never`)과 출력에 남는다. 커밋하지 않은 변경 거절, 저장소 등록 확인, 허브 확인은 생략할 때도 한다. 정책은 preflight가 방금 가져온 기본 브랜치의 `chute.toml`에서 읽는다. 읽지 못하면 생략하지 않는다. 생략한 결과는 상태에 `skipped`로 기록하지만 같은 HEAD의 실제 검사 결과가 이미 있으면 덮어쓰지 않는다. 구버전 chute는 `skipped`를 읽지 못하니 PC마다 같은 버전을 쓴다 |
| `env_check` | `[]` | 검증 전에 돌려 결과를 남길 명령의 목록. 빈 항목은 안 된다. 예: `["cargo --version"]`. 슬롯 폴더에서 `sh -c`로 하나씩 돌린다. 명령마다 30초 제한이 있고, 출력은 첫 줄 200자만 남긴다. 명령이 실패해도 검증은 그대로 돈다. 머신 이름과 hc 코어 id는 목록이 비어도 남는다. batch는 한 번, preflight는 실행마다 기록한다. 반송 보고서의 "검사 환경" 절은 같은 HEAD의 preflight와 다른 줄만 나란히 보인다 |
| `reuse_verified` | `true` | `true`면 batch가 합친 트리가 이미 통과한 트리와 같을 때 검증을 다시 하지 않고 그 통과를 쓴다. 트리 SHA(같은 기준, 같은 큐 구성, 같은 PR HEAD로 합친 결과), 검증 설정의 지문(`verify` 단계의 이름, `run`, `timeout`, `paths`), `paths`로 건너뛴 단계가 모두 같아야 같은 트리다. preflight와 batch가 통과하면 그 트리를 상태에 기록한다. 실패는 기록하지 않는다. 다시 돌아서 통과한 검증도 기록하지 않는다. 흔들림 장부(`chronic_days` 안의 `chute flaky` 목록)에 오른 단계를 도는 검증은 다시 쓰지 않는다. 다시 쓰면 `chute events`와 batch 기록에 `reused`로 어느 검증(`preflight`나 `batch`)의 결과인지 남고 `status --json`의 `batch.reused`에도 보인다. 검증 실패와 이분 탐색의 판정은 바뀌지 않는다 |
| `reuse_hours` | `24` | 통과 기록을 다시 쓸 수 있는 시간(시간 단위). 지나면 환경이 바뀌었을 수 있어 다시 검증한다. 1 이상 168 이하 |
| `reuse_other_machines` | `false` | 기본으로는 같은 머신에서 검증한 결과만 다시 쓴다. preflight는 작업자 PC에서, batch는 허브에서 도는 v2에서는 둘이 다른 머신이라 다시 쓰지 않는다. `true`면 `env_check`가 비어 있지 않고 두 머신의 `env_check` 결과(명령과 출력, 모두 성공)가 같을 때 다른 머신의 통과도 쓴다 |
| `post_verify_interval` | `0` | 머지 뒤 검사를 시작하는 최소 간격(분). 앞 검사를 시작한 때부터 센다. 0이면 기본 브랜치가 움직인 뒤 지금 검사가 끝나는 대로 돈다. `[[post_verify]]` 단계가 없으면 0이어야 한다 |
| `post_verify_hold` | `false` | `true`면 머지 뒤 검사가 실패한 동안 실패한 단계의 `paths`에 해당하는 PR을 batch에 넣지 않고 큐에 둔다. `paths`가 없는 단계가 실패하면 복구 PR을 뺀 모든 PR이다. 검사가 다시 통과하면 풀린다. `[[post_verify]]` 단계가 없으면 `false`여야 한다 |
| `respawn` | `[]` | 반송을 받을 에이전트가 없을 때 띄우는 명령. 비어 있으면 띄우지 않고 반송 줄을 submit한 터미널에 대신 보낸다 |
| `remote` | 없음 | chute가 관리하는 GitHub 저장소를 가리키는 git 원격의 이름. 없으면 chute가 정한다. 아래 "원격이 여러 개일 때"를 본다. `chute init`은 작업 폴더의 `chute.toml`에서 이 키를 읽는다 |
| `retention` (`[retention]`) | 아래 두 키 | 정리할 때 남기는 기간 |
| `logs_days` | `7` | 검증 로그와 preflight 로그를 남기는 날수. 1 이상 |
| `reports_days` | `30` | 반송 보고서, preflight 보고서, 실패 트리 참조를 남기는 날수. 1 이상 |
| `cache` (`[cache]`) | 아래 한 키 | 슬롯의 빌드 캐시 |
| `seed` | `[]` | 새 슬롯에 복사해 올, git이 무시하는 경로의 목록. 빈 항목은 안 된다. 예: `["target"]` |
| `flaky` (`[flaky]`) | 아래 네 키 | 흔들린 시험의 처리 |
| `probe_repeats` | `20` | 흔들린 시험을 되풀이하는 횟수의 상한. 1 이상 |
| `probe_seconds` | `180` | 되풀이하는 시간의 상한(초). 1 이상 |
| `chronic` | `3` | 기간 안에 이만큼 흔들린 시험을 만성으로 본다. 1 이상 |
| `chronic_days` | `30` | 만성을 세는 기간(일). 1 이상 |
| `verify` (`[[verify]]`) | 없음. 하나 이상 필요 | 검증 단계. 적은 순서대로 돌고 하나라도 실패하면 그 batch는 실패다 |
| `verify.name` | 필수 | 단계 이름. 겹치거나 `/`를 담거나 `.`으로 시작하면 안 된다 |
| `verify.run` | 필수 | 검증 worktree에서 `sh -c`로 실행하는 명령 |
| `verify.timeout` | `3600` | 초. 넘기면 그 단계는 실패(`시간 초과`)다. 시간 초과는 다시 돌려 보지 않는다 |
| `verify.probe` | 없음 | 선택. 흔들린 시험만 되풀이하는 명령. `{tests}` 자리가 있어야 하며 없으면 설정 오류다. 예: `cargo test {tests} -- --exact` |
| `verify.failed` | 없음 | 선택. 실패한 시험 이름을 뽑는 정규식. 캡처 그룹이 있으면 그 값이, 없으면 일치한 글이 이름이다. 없으면 보고서에 로그의 끝부분이 들어간다 |
| `verify.paths` | 없음 | 선택. 경로 접두어의 목록. 검증하는 트리가 기준과 다른 파일 가운데 하나라도 이 가운데 어느 것으로 시작하면 그 단계를 돌고, 아니면 건너뛴다. 없으면 늘 돈다. 빈 목록이나 빈 항목은 설정 오류다 |
| `post_verify` (`[[post_verify]]`) | 없음 | 머지 뒤 검사의 단계. 모양은 `[[verify]]`와 같다. 적은 순서대로 돈다. 아래 "머지 뒤 검사" |
| `post_verify.name` | 필수 | 단계 이름. `[[verify]]`의 이름을 포함해 겹치면 안 된다(흔들림 장부가 단계를 이름으로만 구분한다). `/`를 담거나 `.`으로 시작하면 안 된다 |
| `post_verify.run` | 필수 | 검사 worktree에서 `sh -c`로 실행하는 명령 |
| `post_verify.timeout` | `3600` | 초. 넘기면 그 단계는 실패(`시간 초과`)다. 다시 돌려 보지 않는다 |
| `post_verify.failed` | 없음 | 선택. `verify.failed`와 같다 |
| `post_verify.probe` | 없음 | 선택. `verify.probe`와 같다(`{tests}` 자리가 없으면 설정 오류다). 머지 뒤 검사에서 단계가 한 번 실패하면 단계 전체 대신 이 명령으로 실패한 시험만 다시 돈다. 아래 "머지 뒤 검사" |
| `post_verify.paths` | 없음 | 선택. `verify.paths`와 같다. 기준은 마지막으로 통과한 커밋이고, 통과한 적이 없으면 모든 단계를 돈다 |

### 단계의 `paths`

`paths`가 있는 단계는 바뀐 파일이 맞을 때만 돈다. 비교는 문자열 접두어다(`"web/"`는 `web/app.js`에 맞고 `docs/web/x`에는 맞지 않는다). 이름이 바뀐 파일은 옛 경로와 새 경로를 모두 센다. 기준은 다음과 같다.

- batch는 그 batch를 만든 기본 브랜치 커밋과 검증하는 트리를 비교한다. 나눠 다시 검증하는 batch도 같은 커밋을 기준으로 한다.
- preflight는 그 preflight가 합친 기본 브랜치 커밋과 비교한다. 큐의 PR이 바꾼 파일도 들어가므로 batch보다 덜 건너뛴다.
- 기준 검증, 기준 재검증, revert 검증은 `paths`를 무시하고 모든 단계를 돈다. 기준이 깨졌는지 보는 검사에서 단계를 건너뛰면 깨진 기준을 놓친다.

건너뛴 단계는 batch 기록(`batch.json`의 `skipped`: 검증 회차와 단계 이름)과 preflight 결과에 남는다. `chute preflight`는 "건너뜀(바뀐 파일이 paths에 없다)"을 단계 이름과 함께 보이고 `--json`에는 `skipped_steps`가 있다. 건너뛴 트리도 기준으로 검증한 것으로 센다. `paths`를 잘못 적으면 그 단계가 필요한 PR에서도 건너뛰니 설정을 올리기 전에 `chute config-check`로 본다.

### 설정 확인

`chute config-check [--file <경로>] [--json]`은 작업 폴더(`--repo`를 주면 그 폴더, `--file`을 주면 그 파일)의 `chute.toml`을 읽어 검증 단계마다 이름, `paths`, 시간 제한을 보인다. 설정 오류가 있으면 이유를 내고 종료 코드 1로 끝난다. 저장소를 등록하지 않고 상태 폴더도 쓰지 않으며 등록되지 않은 저장소에서도 된다. chute는 기본 브랜치에 올라간 설정만 읽으므로 올리기 전에 확인하는 데 쓴다. `--json`은 `ok`, `file`, `error`, `verify`와 `post_verify`(`name`, `paths`, `timeout_s`)를 낸다. `[[post_verify]]` 단계는 "머지 뒤 검사 단계 N개" 아래에 따로 보인다.

### 원격이 여러 개일 때

chute는 `origin`이라는 이름을 고정해 쓰지 않는다. 관리하는 저장소는 GitHub URL의 `owner/name`으로 찾는다. ssh 주소, https 주소, `.git`이 붙은 주소와 안 붙은 주소는 같은 저장소다. 쓸 원격은 다음 순서로 정한다.

1. `chute.toml`의 `remote = "<이름>"`이 있으면 그 원격을 쓴다.
2. 없으면 이미 `chute init`으로 등록한 저장소의 URL과 맞는 원격이 하나뿐일 때 그 원격을 쓴다. 등록한 것이 아직 없으면 원격이 하나뿐일 때 그 원격을 쓴다.
3. 맞는 원격이 여럿이어도 주소가 모두 같으면(ssh와 https 형태, `.git` 유무 차이는 같은 주소다) `origin`을, 없으면 이름순 첫째를 쓰고 `chute init`이 한 줄로 알린다. 주소가 서로 다른 원격이 둘 이상이거나 맞는 원격이 없으면 오류로 멈춘다. 오류가 `git remote -v`로 확인하고 `remote` 키를 적으라고 안내한다.

예를 들어 fork를 `origin`으로 두고 GitHub 저장소를 `upstream`으로 둔 clone은 `chute.toml`에 `remote = "upstream"`을 적고 `chute init`을 실행한다. 그 뒤로 `chute submit`, batch 머지, 설정 읽기, 기본 브랜치 push는 모두 `upstream`으로 간다. `chute init`은 고른 원격의 이름을 등록 정보(`repo.json`의 `remote_name`)에 적는다. `remote` 키를 바꿨으면 `chute init`을 다시 실행해 등록을 고친다. `remote_name`이 없는 옛 등록은 `origin`으로 본다.

`remote` 키는 작업 폴더의 `chute.toml`에서 읽고, 없으면 원격의 기본 브랜치(`<원격>/main`, `<원격>/master`)에 올라간 `chute.toml`에서 읽는다. 기본 브랜치에도 올려 두면 다른 clone에서도 같은 원격을 고른다. 허브가 다른 PC일 때 그 PC의 원격 이름은 그 PC의 등록 정보를 따른다.


**`respawn`의 기본값은 `[]`이다.** 반송을 받을 에이전트가 없거나 터미널이 끝났으면 chute는 에이전트를 띄우지 않고 반송 줄을 submit한 터미널에 대신 보낸다(`delivered=unreachable`). PR은 `bounced`에 그대로 있다. 값을 넣으면 그 명령으로 새 에이전트를 띄운다. 실행 파일과 인자를 나눠 적는다(`respawn = ["claude"]`). **`--dangerously-skip-permissions`를 넣으면 새 에이전트가 권한 확인 없이 명령을 실행하게 한다.** 무인으로 뜨는 에이전트가 권한 대화상자에서 멈추지 않게 하려는 선택이지만, 사용자가 직접 켜는 옵션으로만 둔다(`respawn = ["claude", "--dangerously-skip-permissions"]`). 새 에이전트는 PR 브랜치의 worktree에서 돈다. 원래 에이전트의 worktree가 남아 있으면 그곳이고, 없으면 `~/.herdr/worktrees/<저장소>/<브랜치>`에 만든다. 사용자의 chute 체크아웃이 그 PR 브랜치를 쥐고 있으면 새 에이전트가 그 폴더에서 돈다. 권한 확인을 건너뛰는 값을 넣었다면 그곳에서도 권한 확인 없이 실행된다.

**`require_ci`.** 켜면 batch에 넣기 전에 PR마다 등록된 HEAD의 GitHub check를 `gh`로 읽는다. 모두 통과했으면 batch에 넣는다(건너뛴 check와 중립은 통과로 본다). check가 도는 중이거나 아직 하나도 없으면 PR을 `대기(CI 기다림)`으로 두고 이번 batch에서 뺀다. 다음 batch 때 다시 보고, 큐에 CI를 기다리는 PR만 남았으면 30초쯤마다 다시 본다. check가 submit부터(반송 뒤 다시 submit했으면 마지막 반송부터) 10분 넘게 하나도 없으면 현황판에 경고를 한 번 남긴다. CI가 없는 저장소에서 옵션만 켠 경우를 알리기 위해서다. check가 생기거나 PR이 큐에서 빠지거나 옵션을 끄면 경고가 사라진다. 실패했거나 취소됐으면 실패한 check의 이름과 링크를 담아 반송하고 재시도 예산을 한 번 쓴다. 보는 것은 PR 자신의 HEAD에 대한 check뿐이다. 합친 트리는 여전히 로컬 검증이 판정한다. **로컬 검증이 최종이라는 원칙은 그대로이고 이 옵션은 그 앞의 추가 관문이다.** `chute preflight`는 이 옵션과 무관하다.

**`CHUTE_HOME`.** 상태는 `$CHUTE_HOME`(없으면 `$XDG_STATE_HOME/chute`, 그것도 없으면 `~/.local/state/chute`) 아래에 저장소마다 폴더를 두고 쌓인다. `CHUTE_HOME`을 바꿔 쓴다면 주의한다. `chute up`이 `hc`로 띄운 데몬 터미널이 `CHUTE_HOME`을 물려받지 못하면 다른 상태 폴더를 보고 에이전트의 submit을 못 본다. 데몬이 도는 터미널과 에이전트의 셸이 같은 `CHUTE_HOME`을 갖는지 확인한다. `chute status`가 보이는 큐가 에이전트가 본 것과 다르면 이것을 의심한다.

**PC 전체 검증 상한 (`machine.toml`).** 저장소마다 데몬과 preflight가 따로 돌기 때문에 저장소 여럿의 검증이 한꺼번에 겹치면 PC가 버티지 못한다. 이 PC에서 동시에 도는 검증 수는 `$CHUTE_HOME/machine.toml`의 `max_verify`(1 이상, 파일이 없으면 3)로 제한한다. 이 파일은 PC의 것이라 `chute.toml`에 둘 수 없고 저장소 clone에도 없다. 다른 PC는 그 PC의 `machine.toml`을 따르며 허브로 넘기는 것이 없다.

```toml
# $CHUTE_HOME/machine.toml (선택)
max_verify = 3
```

batch, preflight, 기준 검증, revert, flaky 확인이 모두 검증 명령을 띄우기 전에 `$CHUTE_HOME/machine/verify-<k>.lock` 가운데 빈 것 하나를 잡고 끝나면 놓는다. 모두 잡혀 있으면 자리가 날 때까지 기다린다. `serialize = true`인 저장소는 저장소의 `verify.lock`을 먼저 잡고 PC 자리를 나중에 잡는다. preflight는 기다리기 시작할 때 stderr에 `이 PC에서 검증 N개가 돌고 있어 기다린다`를 한 번 쓴다. 데몬의 batch는 `chute status`에 `PC 자리를 기다리는 중`으로 보이고 `--json`의 `batch.waiting_machine`이 `true`가 된다. 모르는 키나 0 같은 틀린 값은 그 파일 경로를 적은 오류다. 이 상한은 chute의 검증만 센다. chute 밖에서 도는 프로세스는 세지 않는다. `max_verify`를 줄이면 옛 값으로 시작한 검증이 끝날 때까지는 새 값을 넘어 돌 수 있다. 다음 검증부터 새 값을 지킨다.

## 사람이 하는 일

### 현황판 읽기

`chute status`(또는 Herdr의 chute 현황판)가 보인다. 현황판은 `chute status --json --all`의 출력만 그린다. 현황판의 화면 문구는 영어이고, chute가 낸 문장(경고, 허브 오류, 실패 요약, 명령 오류)은 한국어 그대로 보인다. 위에서 아래로 다음 순서다.

1. 멈춤과 경고: 기준 커밋이 깨져 큐가 멈췄는지(실패한 단계와 로그, chute가 push한 커밋인지, 복구 PR의 상태), 데몬이 꺼졌는지, 설정이나 기록의 문제가 있는지(흔들림이 만성이 된 시험 포함).
2. 검증 중인 batch: 기준, PR 목록, 단계, 회차.
3. 대기: 승인 대기, 대기, `대기(CI 기다림)`. 항목에 `급함`(`--priority`), `복구`, `#N 뒤`(그 PR이 머지되길 기다림) 표시가 붙을 수 있다.
4. 반송: 에이전트에게 돌려보낸 PR과 이유.
5. 오늘의 숫자: 머지, 반송, flaky, batch 수, 머지까지 걸린 시간의 중앙값, 머지 뒤 검사가 마지막으로 통과한 커밋(검사가 도는 중이면 단계도).

현황판은 저장소 머리 줄(이름과 상태 한 줄)을 누르면 그 저장소를 접고 펼치며, 선택은 저장소마다 기억한다. 머리의 `▾`는 모두 접기와 모두 펼치기다. 접어 둔 저장소에도 손쓸 일(멈춤, 머지 뒤 실패, 승인 대기, 데몬 꺼짐)이 새로 생기면 잠시 펼쳐지고, 그 문제가 모두 사라지면 다시 접힌다. 저장소는 문제가 있는 것이 위로 오도록(멈춤·승인 대기, 진행 중, 대기, 정상) 정렬되고 같은 등급은 `chute status`의 순서를 지킨다. 다른 Herdr mod는 `shared("chute.status")`로 현황판의 요약을 읽을 수 있다. 값은 `{ grade, issues, running, repos, since }`이고 `grade`는 `ok`, `running`, `attention`, `failed` 가운데 하나다. 값이 바뀔 때만 갱신되고 현황판이 닫히면 `null`이 된다.

현황판은 항목마다 시간을 오른쪽 끝에 흐리게 보인다. 대기 항목은 처음 submit한 뒤 기다린 시간(`waiting 3m`, 분 단위), 반송 항목은 둘째 줄 오른쪽에 반송된 지 얼마(`12m ago`), 최근 머지는 submit부터 머지까지 걸린 시간(`18m 4s`)이고 머지 시각은 그 칸에 올리면 뜨는 설명에 있다. 값이 없으면 칸을 비운다. batch, 대기, 반송 구역은 항목이 없어도 제목과 `0`을 흐리게 한 줄 남긴다(허브에 닿지 않는 저장소는 그리지 않는다). 경고와 알 수 없는 상태 구역은 있을 때만 보인다.

머지 뒤 검사가 실패했으면 `chute status`와 현황판의 1번(멈춤과 경고)에 실패한 단계, 처음 실패한 머지의 PR, 마지막 통과 커밋, 되돌리는 명령이 보인다.

흔들린 시험(flaky)은 에이전트에게 보내지 않고 기록한다. 한 단계가 실패한 뒤 같은 트리에서 그 단계만 다시 돌려 통과하면 flaky로 보고 그 batch를 통과로 머지한다. 흔들리는 시험을 새로 만든 PR도 머지될 수 있다. 머지한 뒤 근거를 모은다. `[[verify]]`에 `probe`가 있으면 흔들린 시험만 되풀이해 기준 트리에서도 실패하는지(`base`), 흔들린 트리에서만 실패하는지(`made`)를 `history.jsonl`에 남기고 `made`면 그 batch PR을 submit한 터미널에 `flaky` 알림을 보낸다. 같은 시험이 `chronic_days`일 안에 `chronic`번 흔들리면 만성으로 경고를 올린다. `chute flaky`가 이 장부를 보인다. 되풀이는 다음 batch를 `probe_seconds`까지 늦춘다. `probe`가 없는 단계는 되풀이하지 않는다.

### 승인과 빼기

현황판의 버튼이 같은 명령을 실행한다. 터미널에서도 쓸 수 있다.

- `chute approve <pr> [--head <SHA>]`: 승인 대기 PR을 큐에 넣는다. 현황판의 버튼은 사람이 본 HEAD를 `--head`로 넘긴다. 그 사이 PR HEAD가 바뀌었으면 승인하지 않는다.
- `chute drop <pr>`: 승인 대기, 대기, 반송된 PR을 큐에서 뺀다. 반송 한도를 넘거나 `chute giveup`을 한 PR은 이미 큐에서 빠져 있으니 다시 넣으려면 다시 submit한다.

### batch 보기와 되돌리기

- `chute batches [--json] [-n <수>]`: 최근 batch를 새것부터 보인다(기본 20개). batch마다 id, 끝난 시각, 머지된 PR, 반송된 PR, 되돌려졌는지를 적는다. `--json`은 같은 내용을 배열로 낸다.
- `chute revert <batch> [--no-verify]`: 그 batch의 merge commit들을 역순으로 `git revert -m 1`로 `origin/<기본 브랜치>` 위에 쌓고, 되돌린 트리를 검증한 뒤 push한다. `--no-verify`는 검증을 건너뛴다. 강제 push로 기본 브랜치를 되감지 않는다. push가 거절되면 새 기준으로 처음부터 다시 하고, 3번까지 시도한다. 아무것도 push하지 않고 종료 코드 1로 끝나는 경우는 다음과 같다. 없는 batch, 머지한 PR이 없는 batch, 이미 되돌린 batch, 기록이 불완전한 batch(데몬이 도중에 멈춰 기록과 `history.jsonl`이 어긋난 경우는 추측하지 않는다), 뒤에 머지된 변경과의 충돌(그 뒤 batch 목록을 보인다), 되돌린 트리의 검증 실패(로그 경로를 보인다). 이미 기본 브랜치에서 사라진 PR은 건너뛰고 나머지를 되돌린다. 되돌리기는 사람이 내린 결정이라 작업자에게 따로 알리지 않고 큐 항목도 바꾸지 않는다. submit한 터미널은 `reverted` 알림을 받는다. 되돌린 PR의 커밋은 기본 브랜치의 이력에 남아 있어서 같은 브랜치를 다시 머지해도 변경이 돌아오지 않는다. `chute revert`는 되돌린 PR 번호와 PR마다 revert 커밋의 SHA, 다시 올리는 방법(브랜치에 기본 브랜치를 합친 뒤 `git revert <revert 커밋>`으로 되돌리고 고쳐서 다시 submit한다)을 출력한다. 되돌린 사실은 batch 폴더의 `revert.json`과 `history.jsonl`에 남는다.

### 데몬 멈추기

- `chute down`: 지금 도는 batch가 끝난 뒤 데몬이 멈춘다. 도는 머지 뒤 검사는 기다리지 않고 취소한다. 다음에 데몬이 뜨면 그때의 tip을 처음부터 검사한다.
- `chute down --now`: 돌던 검증을 취소하고 바로 멈춘다.
- 다시 시작하려면 `chute up`.

`chute daemon`은 `chute up`이 데몬 터미널에서 실행하는 명령이다. 직접 쓸 일은 거의 없다.

## 동작 규칙

- **검증한 트리만 push한다.** 기본 브랜치에 push하는 커밋은 검증 명령이 통과한 바로 그 트리다. 강제 push는 하지 않는다. 그사이 기본 브랜치가 움직여 push가 거절되면 새 기준으로 다시 만든다(3회까지).
- **등록된 HEAD만 머지한다.** submit할 때 PR의 HEAD를 기록하고, batch를 만들 때 PR의 HEAD가 다르면 건너뛰고 에이전트에게 "HEAD가 바뀌었다. 준비되면 다시 submit하라"고 알린다. 예산은 깎지 않는다.
- **`chute.toml`을 고치는 PR도 `approval = "agent"`에서는 승인 없이 큐에 들어간다.** 검증은 PR의 설정이 아니라 지금 기본 브랜치의 `chute.toml`로 돌고, PR이 바꾼 규칙은 머지된 뒤 다음 batch부터 쓰인다. 알고 받아들인 위험이 있다. 반송을 피하려고 검증 단계를 빼거나 시간 제한을 늘리는 변경도 검증만 통과하면 머지된다. 예외는 둘이다. PR의 `chute.toml`을 이 chute가 읽지 못하거나 PR이 파일을 지우면, 머지되는 순간 큐가 서므로 신뢰 모드에서도 승인 대기로 간다. `approval = "human"`이면 모든 PR이 승인 대기다. 설정을 고치려고 들어온 PR은 `history.jsonl`의 `submitted` 이벤트(`touches_config`)와 `chute events`, 현황판의 `chute.toml` 표시로 알아본다.
- **기준이 깨지면 큐가 멈춘다.** 단독 실패로 반송하기 전에, 그 기준 커밋이 검증을 통과한 적이 없으면 기준만 검증한다. 같은 기준 위에서 서로 다른 PR 둘이 연달아 단독 실패해도, 그 기준이 이미 통과한 적이 있어도 두 번째를 반송하기 전에 다시 검증한다. 기준도 실패하면 아무도 반송하지 않고 큐를 멈춘다(`큐 멈춤`). 기본 브랜치를 직접 push로 깨뜨렸거나 환경이 깨졌을 때 모든 에이전트가 틀린 반송을 받는 것을 막는다. 멈춘 큐는 복구 PR로 푼다("복구 PR").
- 기본 브랜치를 향한 열린 PR만 받는다. draft나 다른 브랜치를 향한 PR은 submit이 거절한다.
- push가 끝나면 `gh`로 PR이 Merged인지 본다. 아니면 현황판에 경고를 남긴다.

## 머지 뒤 검사

batch 검증은 PR을 묶어 머지하기 전에 돈다. 오래 걸리는 단계를 거기에 두면 큐 전체가 그 시간을 기다린다. `[[post_verify]]`에 둔 단계는 batch가 머지한 뒤에 기본 브랜치에서 돈다. batch는 `[[verify]]`의 빠른 단계만 돌려 머지한다. `[[post_verify]]`가 없으면 아무것도 달라지지 않는다.

- **큐와 따로 돈다.** 데몬이 큐를 보는 일과 다른 스레드에서 기본 브랜치의 tip을 본다(10초마다 가져온다). tip이 마지막으로 통과한 커밋(last green)이나 마지막으로 실패한 커밋과 다르고 `post_verify_interval`이 지났으면 검사한다. 슬롯은 batch와 다른 `post`를 쓰고, PC 전체 검증 상한(`machine.toml`의 `max_verify`)을 batch, preflight와 함께 지킨다. 검사가 도는 동안 큐는 계속 머지한다. `serialize = true`의 잠금은 batch와 preflight의 검증만 묶고 머지 뒤 검사는 묶지 않는다(`max_verify`는 함께 지킨다). `[[post_verify]]`가 없는 저장소는 가져오지도 경고하지도 않고 지금과 똑같이 동작한다. 데몬은 batch, submit, 데몬 시작이 가져와 둔 기본 브랜치의 `chute.toml`에 `[[post_verify]]`가 있는지 먼저 보므로, 그 단계를 직접 push로 더했으면 한 번 `git fetch`하거나 데몬을 다시 띄워야 검사가 시작된다.
- **tip만 검사한다.** 검사가 도는 동안 새 머지가 오면, 지금 검사가 끝난 뒤 그때의 tip을 한 번 검사한다. 사이 커밋을 하나씩 검사하지 않는다.
- **`paths`는 마지막 통과 커밋과 비교한다.** 그 커밋과 검사하는 커밋이 다른 파일 가운데 `paths`에 맞는 것이 없으면 그 단계를 건너뛴다. 통과한 적이 없으면 모든 단계를 돈다.
- **통과하면** 그 커밋이 last green이 된다. `chute status`, 현황판, `chute events`(`머지 뒤 검사 통과`)에 보이고 `status --json`의 `post_verify.last_green`에 있다.
- **한 번만 실패하면 흔들림이다.** 실패한 단계는 같은 커밋에서 한 번 더 돈다. 단계에 `probe`가 있고 첫 시도의 로그에서 `failed`로 시험 이름을 하나 이상 얻었으면, 단계 전체 대신 `probe`를 한 번 돈다. 이때 `{tests}`에는 그 시험 이름만 넣는다(`[[verify]]`의 `probe`와 같은 규칙이고 이름은 64 KiB까지 넣는다). `probe`가 없거나 시험 이름을 얻지 못했거나 이름을 하나라도 넣지 못하면(너무 길거나, NUL이나 UTF-8이 아닌 바이트가 있거나, 명령이 인자 길이 상한을 넘으면) batch처럼 단계 전체를 다시 돈다. 둘째 시도가 통과하면 흔들림이다. `flaky` 사건을 남기고 검사는 통과한다. 사건의 시험 이름은 첫 시도에서 실패하고 다시 돌아 통과한 것이고, batch 이름은 `post-<커밋 12자>`이며, `chute flaky` 장부에 오른다. 누구에게도 알리지 않는다. 같은 시험이 `chronic_days`일 안에 `chronic`번 흔들리면 만성 경고(`flaky_chronic`)만 올린다. 둘째 시도도 실패하면 진짜 실패다. 실패한 단계 이름은 원래 단계 이름이고, 로그는 `<단계>.2.log`다. 실패한 시험은 두 시도에 모두 나온 시험이다. 둘째 시도가 단계 전체일 때 그때만 한 번 실패한 시험은 들어가지 않는다. 겹치는 것이 없거나 한쪽에서 이름을 얻지 못했으면 둘째 시도의 이름을 쓰고, 그것도 없고 `probe`를 돌았으면 첫 시도의 이름을 쓴다. 이 값이 `post_failed`의 `tests`, 보고서, `status --json`의 `red.tests`가 되고 "같은 실패" 비교와 범인 찾기에 쓰인다. 전체로 돌 때만 실패하고 혼자 돌면 통과하는 시험은 `probe`에서 통과하므로 흔들림으로 처리된다. 그런 시험은 장부와 만성 경고에서 드러난다. 시간 초과는 다시 돌리지 않는다. batch와 preflight의 둘째 시도는 그대로 단계 전체다.
- **진짜 실패하면 범인 머지를 찾는다.** last green과 실패한 tip 사이에 기본 브랜치로 머지된 커밋(첫 부모만 따라간 머지 커밋)을 이분 탐색한다. 가운데 커밋마다 `[[post_verify]]` 단계를 처음부터 돌린다. 이 첫 시도는 단계 전체다. 그 단계가 실패하면 둘째 시도로 tip이 실패한 시험만 `probe`로 다시 돈다(`{tests}`에 tip의 시험을 넣는다. 그 커밋이 첫 시도에서 실패한 시험이 아니다). 그 커밋의 옛 실패를 다시 확인해도 tip의 실패에 대해 답이 나오지 않기 때문이다. 시간 초과는 다시 돌지 않는다. 그 단계에 `probe`가 없거나, tip의 시험 이름을 넣지 못하거나(위와 같은 상한), 검사 도중 `failed`가 바뀌어 tip의 이름과 모양이 다를 수 있으면 첫 시도의 시험을 `probe`로 돌거나 단계 전체를 다시 돈다. 판정은 이렇다.
  - 첫 시도나 둘째 시도가 통과하면 통과다. 흔들림 장부에는 첫 시도에서 실패했고 다시 돌아 통과한 시험만 남는다. tip의 시험으로 돌았으면 첫 시도의 실패 가운데 tip의 시험인 것만이다. 다시 돌지 않은 옛 실패는 남지 않는다.
  - 둘째 시도도 실패하면, tip의 실패한 시험이 모두 첫 시도와 둘째 시도의 실패에 모두 나올 때만 같은 실패다. 아니면 다른 실패다. `probe`가 시험 이름 없이 실패한 경우(옛 스크립트가 새 `probe`를 받지 못할 때 등)도 다른 실패다. 그래서 그 커밋을 범인으로 가리지 않는다. tip에서도 `probe`가 이름을 내지 못했으면 이 엄격한 기준을 쓰지 않고, 첫 시도의 이름으로 채우던 전과 같이 판정한다.
  - tip의 실패에 시험 이름이 없으면(`failed`가 없는 단계) 단계만 같으면 같은 실패다.

  `probe`와 `failed`는 커밋 하나를 검사할 때마다 기본 브랜치를 가져와 그 tip의 `chute.toml`에서 같은 이름의 단계의 것을 새로 읽는다(둘은 짝으로 바꾼다. 읽지 못하거나 같은 이름의 단계가 없으면 검사를 시작할 때 읽은 것을 쓴다). 검사 도중 기본 브랜치에 `probe`가 생기면 그 뒤에 검사하는 커밋부터 쓴다. `run`, `timeout`, `paths`와 단계 목록은 검사를 시작할 때 읽은 것 그대로다. 대략 log₂(머지 수)번 단계 전체를 더 돈다. 이 탐색도 큐를 막지 않는다. 찾은 커밋을 batch 기록(`batch.json`의 `merges`)에서 찾아 PR, batch, submit한 터미널, 작업자를 얻는다. last green이 없거나(처음 도는 검사) 기본 브랜치의 이력에서 빠졌으면 범인을 찾지 못한다. chute가 머지하지 않은 커밋이 처음 실패한 머지면 PR은 없다.
- **알린다.** 범인 PR을 submit한 터미널과 작업자에게 `post_failed` 알림을 보낸다(같은 터미널이면 한 번). 범인 PR이 없으면 받을 곳이 없다. `chute events`에 `놓친 알림`으로 남는다.
- **`post_failed` 줄의 형식.** `chute 결과 post_failed slug=<owner/name> at=<epoch ms> commit=<12자> step=<단계> [tests=<이름,이름>] [last_green=<12자>] [pr=<번호>] [batch=<id>] log=<로그 경로> [report=<보고서 경로>] <한국어 한 문장>`이다. `pr`와 `batch`는 범인을 찾았을 때만 있다.
- **보고서.** `report.md`는 `reports/commit-<12자>/post/`에 있고 실패한 단계, 시험, 로그, 마지막 통과 커밋, 처음 실패한 머지, 되돌리는 명령을 적는다. 로그도 같은 폴더 아래에 있어 `reports_days` 뒤에 정리된다.
- **같은 실패는 한 번만 알린다.** 실패가 남은 채 새 tip도 같은 단계의 같은 시험(위의 두 시도에 모두 나온 시험)으로 실패하고 범인이 아직 기본 브랜치에 있으면, 범인을 다시 찾지도 다시 알리지도 않는다. 단계나 시험이 다르면 새 실패로 다시 찾는다. 범인을 찾는 중에 tip의 실패(같은 단계, tip이 실패한 시험을 모두 포함하는 실패)가 아닌 모양으로 실패하는 커밋은 판정에 쓰지 않고 이웃 커밋을 대신 돌린다. 다른 실패라 tip의 실패에 대해서는 아무것도 말해 주지 않기 때문이다. 이웃까지 모두 그렇게 실패하면 범인을 가리지 못하고 범인 없이 알린다. `failed`가 없는 단계는 시험 이름을 몰라 단계만 같으면 같은 실패로 보므로, 옛 실패가 남은 채 새로 깨지면 새 범인을 구별하지 못한다. 범인을 가리려면 단계에 `failed`를 둔다.
- **되돌리는 일은 오케스트레이터가 정한다.** chute는 자동으로 되돌리지 않는다. 알림과 보고서가 `chute revert <batch>`를 안내할 뿐이다.
- **고쳐지면 풀린다.** 실패가 남은 뒤 어느 tip이 통과하면 `post_fixed` 알림을 같은 터미널들에 보내고(`chute 결과 post_fixed slug=<owner/name> at=<epoch ms> commit=<12자> was=<12자> [pr=<번호>] <한국어 한 문장>`) `chute events`에 `머지 뒤 검사 풀림`이 남는다.
- **선택: 고칠 때까지 PR을 잡아 둔다.** `post_verify_hold = true`면 실패가 남은 동안 batch가 실패한 단계의 `paths`에 해당하는 큐의 PR(그 PR이 기본 브랜치와 갈라진 뒤 바꾼 파일로 비교한다)을 가져가지 않는다. 다른 PR은 머지된다. 고치는 PR은 `chute submit --repair`로 넣으면 잡히지 않고 먼저 머지된다. 잡혀 있는 동안 데몬은 15초마다 본다. 실패가 풀리면 잡혔던 PR이 다시 batch에 들어간다. 기본은 끈다.
- **상태 JSON.** `chute status --json`의 `post_verify`는 `last_green`(`commit`, `at_ms`), `run`(도는 검사: `tip`, `phase`는 `verifying`이나 `searching`, 지금 보는 `commit`, `step`, `started_ms`, `waiting_machine`), `red`(남은 실패: `commit`, `step`, `tests`, `log`, `report`, `since_ms`, `last_green`, `culprit`의 `commit`·`pr`·`batch`, `holding`), `last_started_ms`다. `run`은 검사가 PC 자리를 기다리면 `waiting_machine`이 `true`다.
- 항목별 시간은 `chute status --json`의 `recent_merged[].submitted_ms`(그 PR을 처음 submit한 시각)와 `queue[].bounced_ms`(상태가 `bounced`인 항목의 가장 늦은 반송 시각, 다른 상태는 `null`)에 있다. 값이 없으면 `null`이다.
- **데몬이 멈추면** 도는 검사는 취소되고 기록은 남지 않는다. 다음에 데몬이 뜨면 그때의 tip을 처음부터 검사한다.

## 첫 가동 점검

chute는 검증한 batch를 기본 브랜치에 직접 push하고 PR마다 merge commit을 남겨 GitHub이 PR을 Merged로 표시하길 기대한다. **이 가정은 자동 시험으로 확인되지 않았다.** 시험은 가짜 `gh`와 가짜 `hc`, 로컬 bare 원격으로 도는데, 가짜는 GitHub의 판정을 흉내 낼 뿐이다. 2026-11-16 Soochol/chute에서 처음 실제로 돌려 확인했다. chute가 push한 #40, #41이 GitHub에서 Merged로 표시됐고, 기준이 깨졌을 때의 대기열 멈춤과 `--repair` 복구도 설계대로 돌았다. 다른 저장소에서 처음 쓸 때도 다음 절차로 확인한다.

1. 실제 저장소에서 `chute init`, 설정 커밋과 push, `chute up`까지 마친다.
2. 내용이 사소한 PR을 하나 올린다(문서 한 줄 고치기면 충분하다).
3. 그 브랜치에서 `chute preflight`, 이어서 `chute submit`을 실행한다.
4. `chute status`로 batch가 도는 것을 보고, 끝나기를 기다린다.
5. GitHub의 그 PR 페이지가 **Merged**(보라색)로 표시되는지 본다. `gh pr view <번호> --json state`가 `MERGED`를 내는지로도 확인한다. 기본 브랜치의 이력에 `Merge pull request #N from …` 커밋이 있어야 한다.
6. Merged로 표시되지 않으면 `chute status`의 경고에 "GitHub이 아직 머지된 PR로 보지 않는다"가 나온다. 이때는 이 방식이 이 저장소에서 통하지 않는 것이니 더 쓰지 말고 알린다.

### 오케스트레이터와 쓰기 전의 점검

v3의 알림, 복구, 정리는 가짜 `gh`, 가짜 `hc`, 가짜 머신 망으로만 시험했다. 실제 GitHub과 실제 머신에서만 확인되는 다음을 사용자 승인 뒤 첫 가동에서 본다.

1. 오케스트레이터 터미널에서 `chute submit <pr> --head <HEAD> --agent <작업자>`를 실행하고, 머지 뒤 오케스트레이터 터미널에 `chute 결과 merged …` 한 줄이 들어오는지 본다. GitHub이 PR을 Merged로 보는지도 본다.
2. 다른 PC의 오케스트레이터가 submit했을 때 허브가 `hc send <머신>/<터미널>`로 알림을 보내는지 본다.
3. `require_head = true`인 저장소에서 `--head` 없는 submit이 거절되고, 허브가 아닌 PC에서 넘긴 submit도 거절되는지 본다.
4. 일부러 깨뜨린 main에서 큐가 멈추고 `halted` 알림에 단계, 로그, `pushed_by_chute`가 맞게 들어오는지 본다. `submit --repair`로 넣은 복구 PR이 혼자 검증돼 머지되고 `resumed`가 오는지 본다.
5. 작업자 터미널을 닫은 뒤 반송이 나면 새 에이전트가 뜨지 않고 반송 줄이 `delivered=unreachable`로 submit한 터미널에 오는지 본다.
6. 두 PC의 `env_check` 결과가 보고서의 "검사 환경" 절에 나란히 보이는지 본다.
7. preflight를 `max_parallel`보다 많이 동시에 띄워 "다른 preflight N개가 돌고 있어 기다린다" 메시지를 보고, 새 슬롯이 `target/`을 씨앗(`[cache] seed`)으로 받아 빌드 시간이 줄었는지 잰다.
8. `chute gc --dry-run`이 실제 상태 폴더에서 지울 것과 크기를 맞게 보이고, 멈춤 로그를 지우지 않는지 본다.
9. 허브가 아닌 PC에서 `chute events --since 1h --json`이 허브의 사건을 돌려주는지, 알림을 놓친 경우가 `notified`로 보이는지 본다.
10. 실제 시험 묶음에서 `probe`가 다음 batch를 얼마나 늦추는지 잰다.

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
- 위험도에 따른 자동/사람 확인 세분화. `approval`의 켜고 끔과, 머지되면 큐가 서는 `chute.toml` 변경(읽지 못하거나 지우는 PR)의 승인 대기만 있다
- orch 스킬 자체를 chute에 맞게 고치는 일. chute는 계약(명령, 알림 형식)만 정한다
- 머지된 브랜치와 worktree 정리
- 별도 clone에서의 preflight, fork에서 온 PR, 기본 브랜치가 아닌 곳을 향한 PR
- 흔들린 시험의 범인 찾기와 머지 보류. 흔들린 batch는 머지하고 근거와 만성 표시만 남긴다
- 검증 명령의 샌드박스(설계만 있다)
- 놓친 알림의 재전송. `chute events`로 따라잡는다
- GitHub 외 호스팅. 여러 PC는 v2가 위의 "여러 PC에서 쓰기"로 다룬다

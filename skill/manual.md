# chute 사용 설명서 (에이전트용)

이 설명서만 읽고 따른다. 설명서에 없는 일은 하지 않는다.

## chute가 무엇인가

chute는 이 저장소의 머지 큐다. 여러 에이전트가 PR을 동시에 올리면 머지가 그 속도를 따라가지 못하고, 앞 PR이 머지될 때마다 뒤 PR이 충돌한다. chute가 PR들을 묶어 로컬에서 한 번에 검증하고, 통과한 것만 기본 브랜치(보통 main)에 머지한다. 실패하거나 충돌한 PR은 원인과 함께 만든 에이전트에게 돌려보낸다.

그래서 에이전트가 PR을 직접 머지하면 안 된다. 직접 머지하면 chute가 검증한 트리와 main이 어긋나고, 큐에 있는 다른 PR의 검증이 틀어진다. 머지는 chute만 한다.

## 규칙

1. PR을 올리기 전에 `chute preflight`를 실행한다. 통과해야 올린다.
2. PR을 올린 뒤 `chute submit`을 실행한다. 이것으로 큐에 들어간다. 오케스트레이터 아래에서 일하거나 이 저장소가 `require_head = true`이면 submit하지 않고 PR 번호와 HEAD를 보고한다(아래 "오케스트레이터 아래에서 일할 때"). 설명서 맨 앞에 "이 저장소에서는 submit하지 말고 PR과 HEAD를 보고한다" 줄이 있으면 그 저장소다.
3. `gh pr merge`나 다른 방법으로 PR을 직접 머지하지 않는다.
4. 기본 브랜치인 main에 push하지 않는다. PR 브랜치에만 push한다. plugin이 켜져 있으면 훅이 3과 4를 막고 `chute preflight`와 `chute submit`을 쓰라고 안내한다. 막혀도 다른 방법으로 우회하지 않는다.
5. chute가 PR을 돌려보내면(아래 "반송") 보고서를 읽고 고쳐서 PR 브랜치에 push한 뒤 다시 `chute submit`을 실행한다. 오케스트레이터 아래에서는 submit하지 않고 새 HEAD를 보고한다. 이 설명서와 반송 줄, 보고서, 훅 메시지가 "`chute submit`을 실행한다"고 적었어도 오케스트레이터 아래에서는 이 보고가 우선한다.
6. 못 풀겠으면 `chute giveup --reason "<이유>"`를 실행하고 멈춘다. 사람 차례로 넘어간다.
7. `chute.toml`의 검증 명령이나 시간 제한을 고쳐서 검증을 통과시키려 하지 않는다. 이 파일을 고치는 PR은 사람 승인을 기다린다.

### 원격이 여러 개일 때

이 설명서의 `origin`과 `origin/main`은 chute가 관리하는 GitHub 원격의 이름과 그 기본 브랜치다. git 원격이 여러 개인 저장소는 이름이 다를 수 있다. 예를 들어 fork가 `origin`이고 GitHub 저장소가 `upstream`이면 `upstream`이다. 어느 원격인지는 `chute.toml`의 `remote` 키와 chute 등록 정보가 정하고, 반송 보고서의 명령도 그 이름으로 나온다. 훅은 chute가 관리하는 원격의 기본 브랜치로 가는 push만 막는다. fork나 미러로 가는 push는 막지 않는다. chute가 원격을 하나로 정하지 못해 멈추면("chute가 쓸 git 원격을 하나로 정하지 못했다") 우회하지 말고 보고한다. `chute.toml`에 `remote`를 적는 것은 사용자가 정한다.

PR은 기본 브랜치를 향한 열린 PR이어야 하고 draft가 아니어야 한다. 저장소가 chute에 등록돼 있지 않으면 `chute`가 "등록되지 않은 저장소"라고 알린다. 그때는 사용자에게 알린다.

## 명령

모든 명령은 저장소 안(PR 브랜치의 worktree)에서 실행한다. 다른 곳에서 실행하려면 `--repo <경로>`를 붙인다.

### `chute preflight [--json]`

기본 브랜치 + 지금 큐에 있는 PR들 + 지금 HEAD를 합쳐 검증하고 결과를 그 자리에서 돌려준다. PR을 올리기 전에 쓴다. 시간이 걸린다. 검증 명령이 오래 걸리면 그만큼 기다린다.

- 종료 코드 `0`: 통과. 올려도 된다.
- 종료 코드 `1`: 검증 실패나 충돌. 보고서 경로가 출력된다. 보고서를 읽고 고친다.
- 종료 코드 `4`: 기준이 깨졌다. 기본 브랜치의 기준 커밋이 검증을 통과하지 못해 이 HEAD를 판정할 수 없다. 이 커밋 탓이 아니니 고치지 않는다. 출력의 `기준이 깨졌다`와 기준 검증 로그 경로를 오케스트레이터에게 알리고 blocked로 보고한다. `--json`의 `result`는 `base_broken`이고 `base_broken`에 `step`과 `log`가 있다.
- 종료 코드 `2`: 실행할 수 없다. 커밋하지 않은 변경이 있거나 저장소가 등록되지 않았거나 설정을 읽지 못한 경우다. 허브가 다른 PC인데 허브에 닿지 않아 큐를 알 수 없는 경우도 있다. stderr의 이유를 읽는다. 허브에 닿지 않는 것이면 아래 "허브가 다른 PC일 때"를 따른다.
- 커밋하지 않은 변경이 있으면 거절한다. 먼저 커밋한다.
- `--json`은 같은 결과를 JSON으로 출력한다(`result`, `head`, `base`, `with`, `skipped`, `flaky`, `report` 등).

통과 출력 예:

```
통과: HEAD 1a2b3c4d5e6f (기준 9f8e7d6a5b4c, 함께 검증한 PR #12, #13)
```

실패 출력 예:

```
검증 실패(단독): test 단계, 실패한 시험 2개
보고서: /home/me/.local/state/chute/repos/acme__widget/reports/commit-1a2b3c4/1.md
HEAD 1a2b3c4d5e6f · 기준 9f8e7d6a5b4c · 함께 검증한 PR #12
```

출력에 `건너뛴 PR`이 나오면 큐의 다른 PR 하나를 합치지 못해 뺀 것이다. 내 변경의 문제가 아니다. `흔들린 단계`가 나오면 한 번 실패한 뒤 다시 돌리니 통과한 것이고 통과로 본다. `건너뛴 검증 단계`가 나오면 바뀐 파일이 그 단계의 `paths`에 없어 돌리지 않은 것이다. 문제가 아니다.

preflight를 통과해도 머지가 보장되지는 않는다. 최종 판정은 큐의 batch 검증이다.

### `chute submit [<pr>] [--head <SHA>] [--agent <주소>] [--priority] [--after <PR>]… [--repair] [--json]`

PR을 큐에 올린다. 작업자는 `--head`와 `--agent` 말고는 쓰지 않는다. `--priority`, `--after`, `--repair`는 오케스트레이터가 쓴다(아래 "오케스트레이터가 쓰는 것"). PR 번호가 없으면 지금 브랜치의 PR이다. 그 순간의 PR HEAD를 큐에 기록하고, **기록한 HEAD만 머지한다.** 그 뒤에 push하면 HEAD가 달라져 머지되지 않고 알림이 온다. 고칠 것이 끝났을 때 submit한다.

- 종료 코드 `0`: 큐에 올렸다(또는 이미 올라 있다).
- 종료 코드 `1`: 거절. 닫힌 PR, 이미 머지된 PR, draft, 기본 브랜치가 아닌 곳을 향한 PR, 지금 batch에서 검증 중인 PR, 사람 차례인 PR, `--head`와 PR HEAD가 다른 경우, `chute.toml`을 기본 브랜치(`origin/main`. 원격 이름은 `chute.toml`의 `remote`나 등록 정보를 따른다)에서 읽지 못한 경우, `require_head = true`인 저장소에서 `--head` 없이 submit한 경우, `--after`나 `--repair`의 조건을 어긴 경우다. 이유가 stderr에 나온다. `chute.toml`을 읽지 못했다는 이유면 `git fetch`를 먼저 실행하고 다시 한다.
- 종료 코드 `2`: 실행할 수 없다. PR을 찾지 못했거나(번호를 인자로 주거나 PR이 있는 브랜치에서 실행한다) 저장소가 등록되지 않은 경우다. 허브가 다른 PC일 때 허브에 닿지 않는 경우도 종료 코드 `2`다(아래 "허브가 다른 PC일 때").
- `--head <SHA>`: 확인한 커밋. GitHub의 PR HEAD가 이 값과 다르면 거절한다(종료 코드 `1`). 7자 이상의 짧은 SHA도 된다. 허브가 다른 PC일 때는 이 PC의 HEAD가 자동으로 들어가므로 줄 필요가 없다(`require_head = true`인 저장소는 이 자동 값을 받지 않는다). 허브에서 직접 실행할 때 push한 커밋을 못 박으려면 쓴다.
- `--agent <주소>`: 반송을 받을 `hc` 터미널 주소. 없으면 환경 변수 `HC_TERMINAL_ID`를 쓴다. Codex처럼 이전 터미널의 `HC_TERMINAL_ID`를 물려받을 수 있는 에이전트는 자기 주소를 `--agent`로 직접 준다.
- `--json`은 결과를 JSON으로 출력한다.
- 여러 번 실행해도 안전하다. 이미 올라 있고 바뀐 것이 없으면 "이미 등록돼 있고 바뀐 것이 없다"고 알린다.
- 데몬이 떠 있지 않으면 submit이 띄운다.

출력 예:

```
등록했다: #14
HEAD 1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b · 대기 · 큐 3번째
```

사람 승인이 필요하면(저장소 설정이 `approval = "human"`이거나 PR이 `chute.toml`을 고친 경우) 상태가 `승인 대기`로 나온다. 사람이 승인할 때까지 기다린다. 승인은 HEAD에 묶이므로 새 커밋을 push하고 다시 submit하면 다시 승인받아야 한다.

### `chute giveup [<pr>] --reason "<이유>"`

못 풀겠다고 선언한다. PR이 사람 차례로 넘어가고 chute는 더 돌려보내지 않는다. `--reason`은 필수이고 막힌 까닭을 구체적으로 적는다. 출력은 `사람 차례로 넘겼다: #14 (포기: <이유>)`다. 종료 코드 `0`이 성공, `1`이 거절(큐에 없는 PR, 지금 batch에서 검증 중인 PR, 이미 사람 차례인 PR)이다.

### `chute status [--json] [--all]`

큐, 지금 batch, 사람 차례, 오늘의 숫자를 보인다. 자기 PR이 지금 어느 상태인지 볼 때 쓴다. 이 명령은 아무것도 바꾸지 않는다. `--json`은 JSON으로, `--all`은 등록된 모든 저장소를 배열로 출력한다.

출력 예:

```
acme/widget (main) · 자동 머지 · 데몬 실행 중 (t1zz)
batch b20261115-153000 Verifying test · 1회차 · #12 #13
  #12 검증 중 · impl-a · 로그인 화면 추가
  #13 검증 중 · impl-b · 설정 저장 고치기
  #14 대기 · impl-c · 문서 정리
오늘: 머지 7 · 반송 2 · flaky 1 · batch 3 · 머지까지 중앙값 412초
경고: …
```

PR의 상태는 다음 가운데 하나다.

| 상태 | 뜻 | 할 일 |
|---|---|---|
| `승인 대기` | 사람 승인을 기다린다 | 기다린다 |
| `대기` | batch를 기다린다 | 기다린다 |
| `대기(CI 기다림)` | 원격 CI(GitHub check)가 끝나기를 기다린다. `ci_pending` | 기다린다 |
| `검증 중` | 지금 batch에 들어 있다 | 기다린다. 이때 submit이나 giveup은 거절된다 |
| `반송(충돌)`, `반송(검증 실패)`, `반송(시간 초과)`, `반송(HEAD 바뀜)`, `반송(CI 실패)` | 에이전트에게 돌려보냈다 | 아래 "상황별로 할 일" |
| `사람 차례(예산 소진)`, `사람 차례(전달 실패)`, `사람 차례(포기)` | 사람이 풀 차례다(`needs_human`) | 더 submit하지 않는다. 사용자에게 알린다 |

`대기`인 PR의 `chute status --json` 항목에 `after`가 비어 있지 않으면 그 PR들이 머지되길 기다리는 것이고 batch에 들어가지 않는다. `priority`는 먼저 batch에 들어가는 표시, `repair`는 복구 PR 표시다. `submitter`는 submit한 터미널이다(작업자와 같거나 오케스트레이터다).

저장소 전체가 멈추면 첫 줄 아래에 `큐 멈춤: 기준 커밋 …이 검증을 통과하지 못한다`가 나온다. 기본 브랜치가 깨져 있다는 뜻이다. 기준이 깨졌으면 아무도 반송하지 않는다. 사람이 main에 직접 push해서 풀지 않고, 오케스트레이터가 에이전트에게 복구 PR을 만들게 해 `chute submit --repair`로 넣는다. 작업자는 PR을 고치려 하지 않고 기다린다. 복구 PR을 만들라는 지시를 받았을 때만 만든다. 큐가 멈춘 이유(실패한 단계, 로그, 깨진 커밋을 chute가 push했는지)는 `chute status --json`의 `blocked`(`step`, `log`, `pushed_by_chute`, `rechecked`)와 알림 `halted`에 있다.

## 허브가 다른 PC일 때

여러 PC가 한 대기열을 쓰면 대기열과 데몬은 PC 한 대(허브)에 있다. 허브가 이 PC가 아니어도 에이전트가 쓰는 명령(`preflight`, `submit`, `giveup`, `status`)은 똑같이 쓴다. 이 PC에서 실행하면 chute가 허브로 넘긴다. 허브가 어디인지는 사람이 `chute hub`로 본다. 에이전트가 허브를 고르거나 바꾸지 않는다.

달라지는 것은 다음과 같다.

- **push한 뒤에 submit한다.** 허브가 다른 PC일 때 submit은 이 PC의 로컬 HEAD를 함께 넘긴다. 허브는 GitHub의 PR HEAD가 그 HEAD와 다르면 거절한다(종료 코드 `1`). 메시지는 "push하지 않은 커밋이 있거나 그사이 다른 커밋이 push됐다"다. 이때는 커밋을 모두 push하고 다시 submit한다. PR을 올리고 나서 커밋을 더했다면 그 커밋도 push해야 한다. 메시지에 나온 두 HEAD 가운데 어느 쪽이 맞는지 모르겠으면 `git status`와 `git log origin/<브랜치>..HEAD`로 본다.
- **허브에 닿지 않으면 사람에게 알리고 기다린다.** 종료 코드 `2`와 허브를 가리키는 메시지를 받으면 명령이 허브에 기록되지 않은 것이다. 메시지는 "허브 `<이름>`에 닿지 않는다", "허브 `<이름>`로 명령을 넘기지 못했다", "허브 `<이름>`에서 chute를 실행하지 못했다"처럼 나오고 모두 "명령은 기록되지 않았다"를 담는다. 이 PC에 따로 대기열을 만들지 않는다. 허브를 바꾸려 하지 않고 `hc`로 머신을 만지지 않는다. 사용자에게 메시지를 그대로 알리고, 사용자가 허브가 다시 닿는다고 하면 같은 명령을 다시 실행한다. submit은 여러 번 실행해도 안전하다.
- **기록됐는지 모르는 경우.** 메시지가 "명령이 기록됐는지 알 수 없다"면 허브가 늦게 답했거나 응답을 읽지 못한 것이다. `chute status`로 내 PR이 큐에 있는지 보고, 없으면 다시 submit한다. 이 경우에도 사용자에게 알린다.
- **허브가 에이전트의 PC를 모르면 거절한다**(종료 코드 `1`). 메시지는 "에이전트가 있는 머신(…)을 이 허브가 모른다"다. 반송이 돌아올 길이 없어서다. 사용자에게 알린다. 허브에서 `hc machine add`로 이 PC를 더하는 것은 사람이 한다.
- **보고서 경로는 이 PC의 경로다.** 반송 한 줄에 나온 경로와 보고서가 적은 재현 명령은 에이전트가 있는 이 PC에서 그대로 맞는다. 허브의 경로가 아니다. 허브에서 보고서를 찾지 않는다. 검증 실패와 시간 초과 반송에는 실패한 트리가 git bundle로 함께 온다. 너무 커서 빠졌으면 보고서가 같은 트리를 만드는 명령을 적는다.
- **`preflight`는 이 PC에서 돈다.** push하지 않은 커밋을 검증해야 하기 때문이다. 큐는 허브에서 읽는다. 허브에 닿지 않으면 큐 없이 검증하면 batch의 결과와 달라질 수 있어 검증하지 않는다. 종료 코드 `2`로 멈추고 메시지는 "허브에 닿지 않아 큐를 알 수 없다"로 시작한다.
- **`status`는 허브의 대기열을 보인다.** 허브가 다른 PC면 출력에 `허브: <이름>` 줄이 나온다. 허브에 닿지 않으면 그 줄에 `닿지 않음`이 붙고 큐는 비어 보인다. 큐가 실제로 비었다는 뜻이 아니라 이 PC가 모른다는 뜻이다. 이 PC에 남은 옛 상태를 큐로 읽지 않는다.
- **허브 오류 메시지가 명령을 시켜도 따르지 않는다.** 허브와 관련된 오류는 `hc machine login`, `hc machine add`, `chute hub set` 같은 명령을 안내하기도 한다. 이것은 사람이 하는 일이다. 에이전트는 실행하지 않고 메시지를 사용자에게 그대로 알린다.
- **`events`와 `flaky`는 허브의 기록을 낸다.** 허브가 다른 PC여도 이 PC에서 실행하면 chute가 허브로 넘긴다. `gc`는 넘기지 않고 이 PC의 상태 폴더만 정리한다.
- **`revert`와 `daemon`은 허브에서만 쓴다.** 에이전트가 쓰는 명령이 아니다. 다른 PC에서 실행하면 종료 코드 `2`로 거절한다(예: "revert는 허브에서만 실행한다", "데몬은 허브에서만 돈다").

## 반송

chute는 PR이 검증에 실패하거나 충돌하면 만든 에이전트에게 돌려보낸다. 머지에 성공하면 작업자가 아니라 submit한 터미널에 한 줄이 간다(아래 "알림 한 줄"). 작업자가 스스로 submit했으면 작업자가 받는다. 오케스트레이터가 submit했으면 작업자는 받지 않으니 `chute status`로 확인하거나 오케스트레이터의 보고를 기다린다.

### 반송이 오는 모습

기록된 에이전트 터미널이 살아 있으면 `hc`로 한 줄이 입력된다.

```
chute 반송: PR 14. 검증 실패 단독: test 단계, 실패한 시험 2개. 보고서: /home/me/.local/state/chute/repos/acme__widget/reports/pr-14/1.md. 할 일: 1. … 2. …
```

한 줄에는 요약, 보고서 경로, 할 일이 들어 있다. 셸이 해석할 수 있는 문자(괄호, 백틱 등)는 공백으로 바뀌어 있다. 요약의 `검증 실패 단독`은 보고서의 `검증 실패(단독)`과 같다. 보고서를 읽는다.

기록된 에이전트가 없거나 터미널이 끝났으면, 기본 설정에서는 chute가 새 에이전트를 띄우지 않고 그 PR을 사람 차례로 넘긴다. 사용자가 `chute.toml`의 `respawn`에 명령을 넣었다면 chute가 새 에이전트를 띄우고 같은 내용을 첫 일로 준다. 새로 띄워진 에이전트라면 알아야 할 것이 하나 있다. `respawn`에 `--dangerously-skip-permissions`가 들어 있으면 이 에이전트는 권한 확인 없이 명령을 실행한다. 그러니 보고서가 가리키는 PR의 범위 밖을 건드리지 않는다. 지금 폴더가 그 PR 브랜치의 worktree이니 먼저 `git fetch origin`과 `git status`로 최신 HEAD인지 본다.

### 보고서 읽는 법

보고서는 Markdown(`<번호>.md`)이고 같은 내용의 JSON(`<번호>.json`)이 옆에 있다. 번호는 반송마다 하나씩 커진다. 가장 큰 번호가 최신이다. 담긴 것은 다음과 같다.

- **결과**: 충돌인지, 검증 실패인지, 시간 초과인지.
- **범위: 단독 / 조합.** 단독은 기준 브랜치에 내 PR만 얹어도 같은 단계가 실패한다는 뜻이다. 내 변경이 원인이다. 조합은 내 PR만으로는 통과하지만 함께 묶인 PR과 합치면 실패한다는 뜻이다. 보고서가 `함께 묶였던 PR`의 번호, 제목, 바뀐 파일, diff 파일 경로를 알려 준다. 그 diff를 읽고 두 변경이 어디서 어긋나는지 찾아 내 쪽을 맞춘다.
- **충돌한 파일**: 충돌한 파일과, 그 파일을 고친 기준 브랜치 쪽 커밋(SHA와 제목).
- **실패한 단계, 실패한 시험, 로그 경로**: 단계 설정에 시험 이름을 뽑는 정규식이 있으면 실패한 시험 이름이 나오고, 없으면 로그의 끝부분이 나온다. 로그 전체는 보고서가 가리키는 경로에 있다.
- **시간 초과**: 결과가 `시간 초과`다. 그 단계가 제한 시간 안에 끝나지 않아 chute가 멈춘 것이다. 시간 제한을 늘려서 풀지 않는다. 로그 끝에서 멈춘 곳을 찾는다.
- **재현**: 실패한 트리를 커밋으로 남겨 두었다. 보고서의 재현 절차를 그대로 실행하면 실패한 트리에서 그 단계 명령을 돌려 볼 수 있다. 재현은 임시 worktree에서 하고 끝나면 지우는 명령까지 적혀 있다. 시간 초과 재현은 `timeout` 명령이 있는 환경이 필요하다.
- **다음에 할 일**: 보고서 끝의 번호 붙은 목록. 이 순서로 한다.

원격 CI 실패(`반송(CI 실패)`)는 보고서에 실패한 check의 이름과 링크가 들어 있다. 링크를 열어(`gh run view`, `gh pr checks`) 원인을 보고 고친다. 이 반송도 재시도 예산을 한 번 쓴다.

### 재시도 예산

PR마다 연속 반송 횟수를 센다. 기본 3번 반송한 뒤에 또 실패하면 chute는 더 돌려보내지 않고 `사람 차례(예산 소진)`로 올린다. 머지되거나 사람이 `chute retry`를 하면 0으로 돌아간다. 같은 실패를 되풀이하지 말고, 반송마다 보고서의 원인을 고친다.

## 상황별로 할 일

- **충돌**(`반송(충돌)`): `git fetch origin`, `git merge origin/main`으로 기준 브랜치를 합치고 충돌을 푼다. 푼 뒤 시험을 돌리고 push한 다음 `chute submit`을 실행한다. 충돌 상대가 아직 머지되지 않은 큐의 PR이면 그 PR이 머지된 뒤에 합쳐도 된다.
- **조합 실패**(`범위: 조합`): 보고서의 상대 PR diff 파일을 읽고 내 변경과 어디서 어긋나는지 찾아 내 쪽을 맞춘다. 상대 PR이 이미 머지됐으면 기준 브랜치를 합친 뒤 확인한다. 상대 PR을 고치려 하지 않는다. 그 PR은 다른 에이전트의 것이다.
- **단독 실패**(`범위: 단독`): 보고서의 재현 방법으로 같은 단계를 돌려 실패를 직접 확인하고 고친다. 고친 뒤 `chute preflight`로 확인하고 push한 다음 `chute submit`을 실행한다.
- **HEAD가 바뀌었다는 알림**(`chute 알림: PR N의 HEAD가 바뀌었다`): 등록한 뒤 PR에 push해서 생긴 알림이다. 머지는 등록한 HEAD만 한다. 작업이 준비됐을 때 `chute submit`을 다시 실행한다. 예산은 쓰지 않는다.
- **사람 승인 대기**(`승인 대기`): 기다린다. 승인은 사람이 한다. 에이전트가 승인하지 않는다.
- **원격 CI를 기다리는 중**(`대기(CI 기다림)`, `ci_pending`): 기다린다. chute가 다음 batch 때, 큐에 이런 PR만 남았으면 30초쯤마다 다시 본다.
- **CI 실패 반송**(`반송(CI 실패)`): 보고서의 check 이름과 링크를 보고 고친 뒤 push하고 `chute submit`을 실행한다.
- **사람 차례**(`needs_human`): 더 submit하지 않는다. 사람이 `chute retry`나 `chute drop`으로 푼다. 사용자에게 상태를 알린다.
- **큐가 멈춤**(`큐 멈춤`): 기본 브랜치가 검증을 통과하지 못한다. PR을 고치려 하지 않고 기다린다. 풀 길은 복구 PR(`chute submit --repair`)이고 오케스트레이터가 한다. 다른 이유로 기본 브랜치가 바뀌면 멈춤이 저절로 풀린다.
- **되돌려진 PR**: 사람이 `chute revert`로 PR이 든 batch를 되돌릴 수 있다. 되돌리기는 submit한 터미널이 `reverted` 알림으로 받는다. 작업자가 스스로 submit했으면 작업자가 받고, 오케스트레이터가 submit했으면 작업자는 받지 않는다. 되돌려진 PR의 커밋은 기본 브랜치 이력에 남아 있어서, 같은 브랜치를 다시 머지해도 변경이 돌아오지 않는다. 브랜치에서 revert 커밋을 되돌린 뒤(`git revert <기본 브랜치의 revert 커밋>`, `chute revert`가 PR마다 그 SHA를 출력한다) 문제를 고치고 push하고 `chute submit`을 실행한다.
- **chute가 설정이나 데몬 때문에 실행되지 않는다**: 종료 코드 `2`와 stderr의 이유를 사용자에게 알린다. 데몬을 직접 만지지 않는다.
- **허브에 닿지 않는다**(종료 코드 `2`, "허브 `<이름>`에 닿지 않는다"): 사용자에게 알리고 기다린다. 다른 대기열을 만들거나 PR을 직접 머지하지 않는다.

## 오케스트레이터 아래에서 일할 때

오케스트레이터(orch)가 작업자들을 부리는 저장소에서는 머지를 오케스트레이터가 실행한다. 작업자와 오케스트레이터가 하는 일이 다르다. 오케스트레이터도 이 설명서를 읽으니 둘 다 적는다.

### 작업자가 하는 일

1. 구현하고 시험하고 리뷰를 받는다.
2. `chute preflight`를 통과시킨다. 통과해야 PR을 올린다.
3. PR을 만들고 push한다.
4. 오케스트레이터에게 **PR 번호와 HEAD(커밋 SHA)를 보고**하고 멈춘다. **`chute submit`을 실행하지 않는다.** 오케스트레이터가 그 HEAD를 확인하고 `chute submit <pr> --head <HEAD> --agent <작업자 터미널>`을 실행한다. 이 submit이 머지 실행이다.

`require_head = true`인 저장소는 늘 이렇게 한다. 그 저장소에서 `--head` 없이 submit하면 거절한다(종료 코드 `1`). 메시지는 "이 저장소는 확인한 HEAD(`--head`) 없이 submit을 받지 않는다. 오케스트레이터 아래에서 일한다면 submit하지 말고 PR과 HEAD를 보고한다"다. 이 메시지를 받으면 submit을 되풀이하지 말고 그대로 보고한다. 허브가 다른 PC여도 같다. 그 PC가 로컬 HEAD를 채워 넘겨도 `--head`를 직접 주지 않았다는 표시가 함께 가서 거절한다.

맞출 대상은 **확정된 main(`origin/main`)뿐**이다.

- 큐에 있는 다른 PR의 diff에 맞추지 않는다. 그 PR은 아직 머지되지 않았고 반송될 수도 있다.
- `chute preflight`의 조합 실패 상대(출력의 `함께 검증한 PR`이나 보고서의 `함께 묶였던 PR`)가 **아직 큐에 있으면 고치지 말고 blocked로 보고한다.** 상대 PR 번호와 보고서 경로를 적는다. 오케스트레이터가 상대의 결과를 보고 순서를 정한다(`--after`).
- 상대가 이미 머지됐으면 `git fetch origin`으로 main을 받아 합치고 고친다.

반송을 받으면 위 "반송"과 "상황별로 할 일"대로 고친다. 그 글들이 "`chute submit`을 실행한다"고 해도 **고친 뒤에 submit하지 않는다.** push하고 새 HEAD를 오케스트레이터에게 다시 보고한다.

`chute giveup`은 PR이 큐에 올라 있을 때만 된다(큐에 없으면 "큐에 없다"로 거절한다). 오케스트레이터가 submit하기 전에 막히면 giveup하지 말고 오케스트레이터에게 막힌 점을 보고한다.

작업자가 받는 것은 반송 한 줄과 HEAD가 바뀌었다는 알림뿐이다. 머지, 사람 차례, 큐 멈춤, 되돌리기는 submit한 터미널(오케스트레이터)이 받는다.

### 오케스트레이터가 쓰는 것

**`chute submit <pr> --head <HEAD> --agent <작업자 터미널>`.** 작업자가 보고한 HEAD를 PR HEAD와 맞춰 보고(GitHub의 PR HEAD가 다르면 거절) 큐에 올린다. 반송은 `--agent`의 작업자에게 가고, 결과 알림은 submit을 실행한 터미널(`HC_TERMINAL_ID`)에 온다. 같은 PR을 다시 submit하면 받는 쪽이 새 submit한 터미널로 바뀐다.

| 옵션 | 뜻 |
|---|---|
| `--priority` | 급한 PR로 표시해 batch에 먼저 넣는다. 순서는 복구 PR, `--priority`, 나머지이고 각자 올린 순서다. 돌고 있는 batch를 끊지 않는다. chute는 누가 쓰는지 막지 않는다. **사용자가 허락한 범위에서만 쓴다.** 같은 HEAD로 다시 submit하며 주거나 빼면 표시만 바뀐다. 새 HEAD로 다시 submit할 때(반송을 고친 뒤)도 `--priority`와 `--after`는 그 submit에 준 값으로 바뀌므로 필요하면 매번 다시 준다 |
| `--after <PR>` | 그 PR이 머지될 때까지 batch에 넣지 않는다. 여러 번 줄 수 있다. 그 PR은 큐에 있거나 GitHub에서 이미 머지됐어야 한다. 자기 자신, 서로 기다리는 순환, 큐에도 없고 머지도 안 된 PR은 거절한다. 다시 submit할 때는 `--after`를 다시 주어야 유지된다. 그 PR이 반송, 사람 차례, 큐에서 빠짐(`chute drop`, PR 닫힘)이 되면 이 PR은 계속 잡혀 있고 `waiting` 알림이 온다. 기다리거나, `chute drop`하거나, `--after` 없이 다시 submit한다 |
| `--repair` | 멈춘 큐를 푸는 복구 PR로 넣는다(아래) |

`--priority`나 `--after`만 바뀐 다시 submit은 `--json`의 `change`가 `options_updated`다.

**복구 PR(`chute submit <pr> --head <HEAD> --repair`).** 큐가 멈춰 있을 때만 받고 한 번에 하나만 받는다. 멈춰 있지 않으면 "큐가 멈춰 있지 않아 복구 PR로 받지 않았다", 이미 복구 PR이 있으면 "복구 PR … 한 번에 하나만 받는다"로 거절한다(종료 코드 `1`). 그 복구 PR이 사람 차례여도 다음 복구 PR은 그 PR이 머지되거나 사람이 `chute drop`으로 빼야 받는다. 사용자에게 알린다. `--after`와 함께 쓸 수 없다. 순서는 이렇다.

1. 큐가 멈추면 `halted` 알림이 온다. 단계, 로그, 깨진 커밋을 chute가 push했는지가 들어 있다.
2. 에이전트에게 복구 PR을 만들게 하고 `chute preflight`를 통과시킨다.
3. `--repair`로 submit한다. 데몬이 멈춘 채로도 그 PR 하나만 main 위에서 검증한다. 다른 PR과 묶지 않는다.
4. 통과하면 push하고 큐가 풀린다. submit한 터미널에 `merged`(`repair=yes`)와 `resumed`(`by=repair`)가 온다.
5. 실패하거나 충돌하면 보통 반송과 같이 작업자에게 보고서가 가고 예산을 한 번 쓴다. 큐는 계속 멈춰 있다. 작업자가 고치고 새 HEAD를 보고하면 다시 `--repair`로 submit한다. **작업자가 직접 submit하지 않는다.**

`--head`와 승인 규칙은 일반 submit과 같다. `chute.toml`을 고치는 복구 PR은 사람 승인을 기다린다. 멈춘 사이 누가 main에 직접 push해 기준이 바뀌면 멈춤이 풀리고 복구 PR은 일반 PR처럼 되며 batch를 고를 때 맨 앞에 선다(`resumed by=main_moved`).

기준 재검증도 알아 둔다. 같은 기준 커밋 위에서 서로 다른 PR 둘이 연달아 단독 실패하면 chute가 두 번째를 반송하기 전에 그 기준을 다시 검증한다. 기준이 실패하면 반송하지 않고 멈추고, `halted`의 `rechecked=yes`와 `suspect=<PR>`(이미 반송된 첫 PR)이 그 사실을 알려 준다. `suspect` PR은 환경 탓으로 반송됐을 수 있으니 복구 뒤 다시 submit한다.

**`chute events [--since <시각>] [--pr <n>] [--limit <n>] [--json]`.** 놓친 알림을 따라잡는다. history의 사건을 시간 순으로 한 줄에 하나씩 낸다. 허브가 다른 PC여도 허브의 것을 낸다.

- `--since`는 epoch 밀리초, RFC 3339 시각, 상대 시간(`30m`, `2h`, `1d`) 가운데 하나다. 그 시각 이후(같은 시각 포함)의 사건을 낸다. 같은 시각의 사건이 여럿일 수 있으니 이미 처리한 사건은 `(at_ms, event, pr)`로 거른다.
- 알림 줄의 `at=`는 그 사건의 `at_ms`와 같거나 몇 밀리초 어긋난다(`flaky` 알림은 어긋날 수 있다). `chute events --since <at - 1000>`처럼 조금 앞선 값으로 같은 사건을 찾는다.
- `--limit`의 기본값은 500이고 가장 최근 것만 남긴다. 잘리면 stderr에 알린다.
- `--json`은 history 한 줄과 같은 JSON을 한 줄에 하나씩 낸다. 사건 종류에 `submitted`, `dropped`, `notified`, `resumed`, `flaky_probe`가 있다. `notified`의 `delivered`가 `sent`가 아니면 사람용 출력의 그 줄 끝에 `(놓친 알림)`이 붙는다. 값 가운데 `no_terminal`, `gone`, `blocked`, `unknown_machine`, `failed`는 알림이 가지 못한 것이다. `same_as_agent`는 작업자와 같은 터미널이라 반송 줄 하나만 보낸 것이니 갔다고 본다. 그 줄에도 표시가 붙는다.

**`chute flaky [--days <n>] [--json]`.** 흔들린 시험의 장부다. (단계, 시험)마다 흔들린 횟수, 마지막 시각, 근거(`base`, `made`, `unresolved`), 만성 여부를 보인다. `made`는 그 batch가 흔들림을 만들었을 수 있다는 뜻이다. chute는 흔들린 batch를 그대로 머지하고 기록만 한다. 허브가 다른 PC여도 허브의 것을 낸다.

**`chute gc [--dry-run] [--json]`.** 오래된 검증 로그, preflight 로그, 반송 보고서, 실패 트리 참조, 남는 preflight 슬롯을 지운다. `--dry-run`은 지울 것과 크기만 보인다. 이 PC의 상태 폴더만 다루고 허브로 넘기지 않는다. 기록(`history.jsonl`, `state.json`, `batch.json`, `revert.json`)과 큐가 멈춘 이유의 로그는 지우지 않는다. 데몬이 시작할 때와 한 시간마다 같은 정리를 하니 직접 쓸 일은 드물다.

### 알림 한 줄

머지 결과는 submit한 터미널에 `hc`로 한 줄이 입력된다. 받을 터미널이 없거나 끝났으면 기록만 남고(`chute events`에서 `notified`), 다시 보내지 않는다. 형식은 이렇다.

```
chute 결과 <kind> slug=<owner/name> at=<epoch ms> <key>=<value> … <한국어 한 문장>
```

글자와 숫자, 공백, `.,:/_-+=@%`만 쓴다. 값에는 공백이 없고 목록은 쉼표로 잇는다(`prs=12,13`). `key=value`가 아닌 첫 낱말부터 끝까지가 사람용 문장이다. SHA는 12자다. 경로에 쓸 수 없는 글자가 있으면 그 키를 빼고 문장에 "chute events로 본다"가 붙는다. 종류는 여덟 가지다.

| kind | 언제 | 키 | 할 일 |
|---|---|---|---|
| `merged` | PR이 머지됐다 | `pr`, `head`, `batch`, `commit`, `repair=yes`(복구 PR이면) | 작업을 마무리한다 |
| `bounced` | 반송했다. 작업자와 같은 터미널이면 오지 않는다 | `pr`, `head`, `reason`(`conflict`, `verify`, `timeout`, `head_changed`, `ci`), `mode`(`solo`, `combo`), `partners`, `attempts`, `budget`, `to`(작업자 주소), `delivered`(`sent`, `spawned`), `report` | 작업자가 고치는 중이다. 새 HEAD 보고를 기다린다 |
| `human` | PR이 사람 차례가 됐다 | `pr`, `reason`(`budget`, `unreachable`, `gave_up`) | submit하지 않는다. 사용자에게 알린다 |
| `halted` | 큐가 멈췄다 | `base`, `step`, `pushed_by_chute`(`yes`, `no`), `rechecked`(`yes`, `no`), `log`, `suspect` | 복구 PR을 만들게 한다 |
| `resumed` | 멈춤이 풀렸다 | `base`, `by`(`repair`, `main_moved`), `pr`(복구 PR이면) | 다시 submit할 PR이 있는지 본다 |
| `reverted` | 사람이 batch를 되돌렸다 | `batch`, `prs`, `commit` | 되돌려진 PR의 작업자에게 알린다 |
| `waiting` | `--after` 대상이 머지되지 못해 잡혀 있다 | `pr`, `after`, `why`(`bounced`, `human`, `dropped`, `closed`) | 기다리거나 `chute drop`하거나 `--after` 없이 다시 submit한다 |
| `flaky` | 흔들린 시험이 만들어졌거나 만성이다 | `batch`, `prs`, `step`, `tests`, `evidence`(`made`), `chronic`(`yes`) | 그 시험을 확인한다 |

예:

```
chute 결과 merged slug=acme/widget at=1763270000123 pr=14 head=1a2b3c4d5e6f batch=b20261116-140000 commit=9f8e7d6a5b4c PR 14: 기본 브랜치에 머지됐다.
chute 결과 bounced slug=acme/widget at=1763270000123 pr=15 head=1a2b3c4d5e6f reason=verify mode=combo partners=12,13 attempts=1 budget=3 to=lap/t4 delivered=sent report=/home/me/.local/state/chute/repos/acme__widget/reports/pr-15/1.md PR 15: 반송했다 검증 실패. 1/3번째. 작업자에게 반송 줄을 보냈다.
chute 결과 halted slug=acme/widget at=1763270100456 base=9f8e7d6a5b4c step=test pushed_by_chute=yes rechecked=yes log=/home/me/.local/state/chute/repos/acme__widget/batches/b20261116-141000/verify-3/test.1.log suspect=15 큐가 멈췄다. 복구 PR을 chute submit --repair로 넣는다.
```

### 바뀐 동작

- **기본 설정에서 chute는 새 에이전트를 띄우지 않는다.** 반송을 받을 작업자가 없으면 그 PR은 사람 차례(`전달 실패`)가 되고 submit한 터미널이 `human reason=unreachable`을 받는다. 오케스트레이터가 다음을 정한다. 사용자가 `chute.toml`의 `respawn`에 명령을 넣었을 때만 띄운다.
- **큐 멈춤은 사람이 아니라 복구 PR로 푼다.** 위 "복구 PR".
- **머지 성공이 알려진다.** 받는 쪽은 submit한 터미널이다.
- **chute가 push한 main도 늘 믿지 않는다.** 위 "기준 재검증".

## 에이전트가 하지 않는 것

다음은 사람이 쓰는 명령이다. 에이전트는 실행하지 않는다.

- `chute approve <pr> [--head <SHA>]`: 사람이 승인 대기 PR을 큐에 넣는다. 현황판의 승인 버튼은 사람이 본 HEAD를 `--head`로 함께 넘기고, 그 사이 HEAD가 바뀌었으면 승인하지 않는다.
- `chute retry`: 사람이 사람 차례의 PR을 큐에 다시 넣는다.
- `chute drop`: 사람이 PR을 큐에서 뺀다.
- `chute revert`: 사람이 batch를 되돌린다.
- `chute up`, `chute down`: 사람이 데몬을 띄우고 멈춘다. 허브가 다른 PC여도 허브의 데몬에 닿는다.
- `chute hub`, `chute hub set`, `chute hub take`: 사람이 허브를 보고 정하고 옮긴다.
- `chute install`: 사람이 Codex 스킬 파일과 현황판을 설치한다.

`chute batches`, `chute init`, `chute daemon`도 사람이나 데몬의 일이다. 작업자가 쓸 명령은 `preflight`, `submit`, `giveup`, `status` 넷이다. 오케스트레이터 아래의 작업자는 `submit`도 쓰지 않는다. `submit`의 `--priority`, `--after`, `--repair`와 `chute events`, `chute flaky`, `chute gc`는 오케스트레이터가 쓴다(위 "오케스트레이터 아래에서 일할 때").

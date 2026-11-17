# 현황판 NEEDS YOU 카드를 "무엇을 묻는지" 바로 알게 고치는 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 "NEEDS YOU 카드 개선도 진행해줘"라고 했다. 지금 카드(main `f7dec19`, `mod/chute/main.js` `humanCard`)는 이렇게 보인다.

```
#432 feat(...)                                   ↗
Awaiting approval · Preflight skipped · chute.toml changed
● impl-h (t1rp)                         Retries 0/3
[Approve] [Drop]
```

사용자가 실제로 헷갈린 점은 셋이다.

- 질문(승인할지)과 정보(`Preflight skipped`)와 이유(`chute.toml changed`)가 한 줄에 같은 무게로 나온다. "preflight를 이렇게 해도 되냐고 묻는 건가?"로 읽혔다.
- 무엇이 바뀌는지 보이지 않아 판단하려면 터미널에서 diff를 봐야 했다.
- 누가 submit했는지(어느 터미널, 오케스트레이터인지) 보이지 않았다. 지금은 submitter가 에이전트와 같으면 숨기고(`board.js` `submitterOf`), 에이전트 칩에도 "submit했다"는 말이 없다.

카드를 그리는 데이터도 모자란다. status JSON의 큐 항목에는 chute.toml을 고치는지(`touches_config`)만 있고 무엇을 고치는지는 없다. 항목이 왜 승인 대기인지도 항목에는 없고, 저장소의 `approval`과 `touches_config`를 함께 봐야 안다.

## 2. 확정된 결정

요청에 적힌 것이다.

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 승인 정책 | chute.toml을 고치는 PR은 지금처럼 모두 사람이 승인한다. 강화만 하는 변경도 자동으로 통과시키지 않는다 |
| 2 | 질문 | 카드 맨 위에 질문을 문장으로 둔다. chute.toml 변경이면 `Approve config change?`, `approval = "human"`이면 `Approve to queue?` |
| 3 | 이유 | 질문 아래 한 줄로 이유를 적는다. 예: `This PR edits chute.toml, so a person must approve it.` |
| 4 | 변경 요약 | chute.toml 변경이면 추가·삭제된 `[[verify]]`/`[[post_verify]]` 단계와 바뀐 키를 보인다(예: `clippy: run changed`, `+ step account-server-test`). status에 데이터가 없으면 chute 본체가 submit 때 기준과 PR의 chute.toml을 비교한 요약을 큐 항목에 남긴다. 표시용이고 판정이나 자동 통과에 쓰지 않는다 |
| 5 | diff 버튼 | `View diff ↗` 버튼이 GitHub PR의 files 탭을 `cx.open_url`로 연다 |
| 6 | 부가 정보 | preflight 결과, 재시도, 에이전트는 흐린 작은 줄로 따로 둔다. submitter(이름과 터미널)를 보인다 |
| 7 | 겹침 주의 | 같은 줄을 고치는 승인 대기 PR이 둘 이상일 때의 주의는 플래너가 정한다(4.3) |
| 8 | 화면 | 문구는 영어, 색은 테마 토큰, 좁은 패널(약 368px) |
| 9 | 방식 | 화면 위주면 `basic`, 본체에 큐 항목 필드를 더하면 그 부분은 기준대로 정한다 |
| 10 | 작업 수 | 최소로 한다. 사용자 주간 사용량이 90%대다 |
| 11 | 머지 | chute 저장소이고 chute로 머지한다 |

## 3. 목업 (영어 화면, 패널 폭 약 368px)

chute.toml을 고치는 PR:

```
┌ NEEDS YOU ─────────────────────────────── 1 ┐
│ Approve config change?                       │  ← 질문: 굵게, blocked 색
│ #432 feat(ci): split server tests          ↗ │  ← 지금의 제목 줄
│ This PR edits chute.toml, so a person must   │  ← 이유: muted, 줄바꿈
│ approve it.                                  │
│ + step account-server-test                   │  ← 추가: ansi2
│ ~ clippy: run changed                        │  ← 바뀜: ansi3, 값은 tooltip
│ − post-merge step e2e                        │  ← 삭제: ansi1
│ ~ budget: 3 → 5                              │  ← 짧은 값은 줄에 바로
│ #433 also changes clippy. These may conflict.│  ← 겹침 주의(4.3), ansi3
│ [Approve] [Drop]                 View diff ↗ │
│ Preflight skipped · Retries 0/3 · waited 4m  │  ← 흐린 작은 줄
│ ● impl-h (t1rp) · submitted by orch (t1zz)   │  ← 흐린 작은 줄
└──────────────────────────────────────────────┘
```

`approval = "human"` 저장소의 평범한 PR:

```
│ Approve to queue?                            │
│ #431 fix login redirect                    ↗ │
│ This repo asks a person to approve every PR. │
│ [Approve] [Drop]                 View diff ↗ │
│ Preflight passed · Retries 0/3               │
│ ● submitted by impl-g (t1ro)                 │  ← 에이전트와 submitter가 같을 때
```

## 4. 스스로 정한 결정과 그 이유

### 4.1 본체: 큐 항목에 chute.toml 변경 요약을 남긴다

| 결정 | 이유 |
|---|---|
| `QueueItem`과 `ItemView`에 `config_change`(없으면 `null`, `#[serde(default)]`)를 더한다. `touches_config`가 참일 때만 submit이 채운다 | 지금 status에는 무엇이 바뀌는지가 없다(결정 4). 필드를 더하는 것은 AGENTS.md가 허용한다. 옛 `state.json`도 그대로 읽힌다 |
| 비교 기준은 `merge-base(refs/remotes/<remote>/<기본 브랜치>, PR HEAD)`의 chute.toml이고, 비교 대상은 PR HEAD의 chute.toml이다 | 기본 브랜치 끝과 PR HEAD를 바로 비교하면 PR이 갈라진 뒤 다른 PR이 머지한 chute.toml 변경까지 "이 PR이 바꾼 것"으로 보인다. GitHub files 탭(결정 5)도 merge-base 기준이라 버튼으로 연 diff와 요약이 맞는다. 기준 ref는 submit이 설정을 읽는 그 ref(`load_default_branch_config`)다 |
| PR HEAD 객체가 저장소에 없으면 `git fetch --quiet --no-tags --no-write-fetch-head --refmap= <remote> refs/pull/<n>/head`로 객체만 가져온다. ref와 `FETCH_HEAD`는 쓰지 않는다. 모두 `Ext`를 거친다 | 에이전트 worktree는 같은 객체 저장소를 쓰므로 보통은 이미 있어 네트워크를 쓰지 않는다. 허브로 넘긴 submit은 허브에서 같은 `submit` 함수가 돌아 허브 저장소에서 가져온다. 객체만 쓰므로 데몬의 fetch와 부딪히지 않는다 |
| 읽기나 가져오기가 실패하면 submit은 경고 한 줄(한국어)을 stderr에 내고 `kind: "unreadable"`로 등록을 마친다. 요약 실패로 submit을 거절하지 않는다 | 요약은 표시용이다(결정 4). 승인은 어차피 사람이 하므로 요약이 없으면 diff를 보면 된다 |
| 비교는 chute.toml 텍스트 둘을 받는 순수 함수로 `src/config.rs`에 둔다. TOML 값(`toml::Value`) 수준에서 비교하고 `Config`로 풀어 비교하지 않는다 | 순수 함수라 단위 시험이 쉽다. `Config`는 기본값을 채우고 정규식과 시간으로 바꿔 "원문에서 무엇을 고쳤는지"를 잃는다. 값 비교라 주석과 서식만 바꾼 PR은 변경 0건으로 나온다 |
| `[[verify]]`와 `[[post_verify]]`는 `name`으로 짝짓는다. 이름이 없거나 겹치면 위치(`verify[2]`)로 짝짓는다. 같은 이름 집합에서 순서만 바뀌면 `reordered` 한 건이다. 그 밖의 표(`retention`, `cache`, `flaky`)는 키마다 내려가 비교하고, 배열 값(`respawn`, `env_check`, `paths` 등)은 통째로 비교한다 | 사용자가 든 예가 단계의 추가·삭제와 단계 안 키의 변경이다. 단계는 순서대로 돌므로 순서 변경도 판단에 필요하다 |
| PR의 텍스트가 `Config::parse`를 통과하지 못하면 `kind: "invalid"`로 남기고 `changes`는 비운다 | 승인자가 알아야 할 가장 중요한 사실이다. 머지되면 다음 batch부터 설정을 읽지 못한다 |

`config_change`의 모양(status JSON과 `state.json`이 같다):

```json
"config_change": {
  "kind": "edited",
  "changes": [
    {"op": "added",   "path": ["verify", "account-server-test"], "before": null, "after": "{ name = \"account-server-test\", run = \"cargo test -p server\" }"},
    {"op": "changed", "path": ["verify", "clippy", "run"], "before": "\"cargo clippy\"", "after": "\"cargo clippy --all-targets -- -D warnings\""},
    {"op": "removed", "path": ["post_verify", "e2e"], "before": "{ name = \"e2e\", run = \"npm run e2e\" }", "after": null},
    {"op": "changed", "path": ["budget"], "before": "3", "after": "5"},
    {"op": "reordered", "path": ["verify"], "before": null, "after": null}
  ],
  "more": 0
}
```

| 필드 | 뜻 |
|---|---|
| `kind` | `edited`(PR이 chute.toml을 고친다. `changes`가 0건이면 주석·서식만 바꿨다), `added`(merge-base에 chute.toml이 없었다), `removed`(PR이 지운다), `invalid`(PR의 chute.toml이 설정으로 읽히지 않는다), `unchanged`(두 텍스트가 같다. gh가 파일 100개까지만 주어 chute.toml을 고치는지 몰랐던 PR에서 나온다), `unreadable`(읽지 못했다) |
| `changes[].op` | `added`, `removed`, `changed`, `reordered` |
| `changes[].path` | 문자열 배열. 단계는 `[구역, 단계 이름]`, 단계 안 키는 `[구역, 단계 이름, 키]`, 위쪽 키는 `[키]`, 표 안 키는 `[표, 키]`. 이름에 점이 있어도 헷갈리지 않게 배열로 둔다 |
| `changes[].before`, `after` | TOML 인라인 표기 문자열. 한쪽에만 있으면 다른 쪽은 `null`. 각 500자에서 자른다 |
| `more` | 20건을 넘어 뺀 수 |

다시 submit해 HEAD가 바뀌면 새로 계산한다. HEAD가 같으면 지금의 `touches_config`처럼 옛 값을 둔다(merge-base 기준이라 HEAD가 같으면 PR의 변경도 같다).

### 4.2 화면: 카드 모양

| 결정 | 이유 |
|---|---|
| 카드 순서: 질문 → 제목 줄 → 이유 → 변경 요약 → 겹침 주의 → 버튼 줄 → 부가 정보 두 줄 | 결정 2·6. 판단에 쓰는 것(질문, 이유, 변경)과 버튼을 붙이고 부가 정보는 버튼 아래로 내린다 |
| 질문과 이유는 표로 정한다(아래). chute.toml 변경과 `approval = "human"`이 함께면 chute.toml 쪽을 보인다 | chute.toml 이유가 더 구체적이다. 신뢰 모드에서도 승인이 필요한 이유를 알려 준다 |
| 변경 줄: `+ step <이름>` / `− step <이름>`(verify), `+ post-merge step <이름>`(post_verify), `~ <단계>: <키> changed`, `~ post-merge <단계>: <키> changed`, `~ steps reordered`, `~ <키>: <before> → <after>`, `+ <키> = <after>`, `− <키>`. `before`와 `after`가 둘 다 짧으면(합쳐 24자 이하) 줄에 값을 바로 보이고, 길면 `changed`만 쓰고 tooltip에 두 값을 보인다. `more`가 있으면 `+ N more changes` | 사용자가 든 예(`clippy run changed`, `+ step account-server-test`)를 따른다. `run` 명령은 길어 좁은 패널에서 줄에 넣을 수 없다 |
| 변경 줄 색: 추가 `ansi2`, 삭제 `ansi1`, 바뀜과 순서 `ansi3`. 기호(`+ − ~`)를 함께 써 색만으로 구분하지 않는다 | 테마 토큰만 쓴다(결정 8). 색맹 사용자도 기호로 읽는다 |
| `kind`별 요약 대신 보이는 한 줄: `added` → `Adds chute.toml`, `removed` → `Removes chute.toml`(ansi1), `invalid` → `chute.toml in this PR is invalid`(ansi1), `edited`에 0건 → `Only comments or formatting changed`, `unreadable`이나 필드 없음(옛 항목) → `No change summary. Check the diff.` | 요약이 없을 때도 무엇을 보면 되는지 알린다 |
| `View diff ↗`는 버튼 줄 오른쪽 끝의 작은 링크 버튼이다. 주소는 `pullUrl(slug, pr) + "/files"`, slug가 없으면 버튼을 숨긴다. 승인 대기 카드 모두에 둔다 | 결정 5. 승인 대상이 무엇이든 diff를 보고 판단한다. 주 버튼(Approve)과 무게가 겹치지 않게 오른쪽 링크로 둔다 |
| 부가 정보 첫 줄: preflight 결과 · `Retries a/b` · 기다린 시간. 둘째 줄: 에이전트 칩(지금처럼 누르면 `terminal.peek`) · `submitted by <이름> (<터미널>)`. 에이전트와 submitter가 같으면 `submitted by`만 한 번 보이고 칩처럼 누를 수 있다. 둘 다 없으면 `submitted by unknown terminal`. 모두 `text_size(11~12)`, `t.dim` | 결정 6. 지금은 submitter가 에이전트와 같으면 숨겨 누가 submit했는지 모른다 |
| 승인 대기 카드의 `chute.toml changed/unchanged` 문구와 `Awaiting approval` 문구는 없앤다. 구역 제목 `Needs you`는 그대로 둔다 | 질문과 이유가 같은 말을 대신한다. 한 줄에 같은 무게로 섞인 것이 헷갈림의 원인이었다 |
| HEAD를 모르면(`approvalBlocked`) 지금처럼 Approve를 숨기고 이유 자리 아래에 `Can't approve: HEAD unknown`을 ansi1로 둔다 | 지금 동작을 지킨다 |

질문과 이유:

| 조건 | 질문 | 이유 |
|---|---|---|
| `touches_config`이고 `kind`가 `edited`·`added`·`removed`·`invalid` | `Approve config change?` | `This PR edits chute.toml, so a person must approve it.` |
| `touches_config`이고 `kind`가 `unreadable`이거나 필드가 없음 | `Approve config change?` | `This PR may edit chute.toml, so a person must approve it.` |
| `touches_config`이고 `kind`가 `unchanged`, 저장소 `approval`이 `agent` | `Approve to queue?` | `chute couldn't check every changed file, so a person must approve it.` |
| 그 밖(저장소 `approval`이 `human`) | `Approve to queue?` | `This repo asks a person to approve every PR.` |

### 4.3 겹침 주의

| 결정 | 이유 |
|---|---|
| 같은 저장소의 승인 대기 항목 둘 이상이 `config_change.changes`에서 같은 대상을 건드리면 각 카드에 `#433 also changes clippy. These may conflict.`를 ansi3 한 줄로 보인다. 대상은 `path`의 앞 두 칸(단계면 `[구역, 이름]`, 위쪽 키면 `[키]`)이다. 상대가 여럿이면 `#433, #435 also change …`, 대상이 여럿이면 첫 대상만 적는다 | 둘 다 승인하면 같은 batch에서 텍스트 충돌로 하나가 반송되거나, 둘이 다른 값을 넣어 뒤의 것이 앞의 것을 덮는다. 데이터가 4.1로 생기므로 mod 안 계산 몇 줄로 끝난다. 실제 충돌을 가리는 것이 아니라 "같은 곳을 고친다"만 알리므로 문구를 `may`로 둔다 |
| `touches_config`이지만 요약이 없는 항목끼리는 주의를 보이지 않는다 | 같은 곳인지 모르는데 주의를 띄우면 소음이 된다 |

### 4.4 작업과 방식

| 작업 | 프로필 | 방식 | 이유 |
|---|---|---|---|
| `needs-card` | `implementer` | `poteto` | 본체가 `state.json`에 남는 큐 항목 필드를 더하고(상태 저장·데이터), submit에서 git 객체를 가져온다. `basic` 조건에 걸린다. 화면 부분만 떼어 `basic` 작업으로 두면 그 작업은 이 데이터를 써야 하므로 순차가 되고, 작업자 세션과 batch가 하나씩 더 든다(결정 10) |

작업이 하나라 순차로 둔 작업은 없다.

지금 돌고 있는 `flaky-two`(플랜 `docs/plans/flaky-tests.md`)는 `src/cmd/daemon.rs`, `src/gc.rs`, `tests/daemon.rs`를 고친다(그 worktree의 `git diff --stat`으로 확인). 이 작업이 고칠 파일(`src/config.rs`, `src/model.rs`, `src/queue.rs`, `src/cmd/submit.rs`, `src/cmd/status.rs`의 시험, `tests/` 아래 submit·status·old_state 시험과 기대 JSON, `tests/fixtures/status-full.json`, `mod/chute/`)과 겹치지 않으므로 순서를 두지 않는다. 둘 다 chute batch에서 합쳐 검증된다.

## 5. 범위 밖

- 승인 정책 변경. chute.toml 변경 PR의 자동 통과나 "강화만 하는 변경" 판정(결정 1). `config_change`는 판정에 쓰지 않는다.
- `chute submit`과 `chute status`의 터미널 출력에 변경 요약을 보이는 것. 데이터는 생기므로 뒤에 따로 더할 수 있다.
- 승인 대기 알림(`board.js`의 notice, hc 알림) 문구 변경.
- herdr 탭 이름 표시. status에는 터미널 id와 이름만 있다.
- 실제 충돌 검사(두 PR을 합쳐 보는 것). 4.3은 같은 대상을 건드리는지만 본다.
- 이 기능이 머지되기 전에 승인 대기로 들어간 항목의 요약. 그 항목은 `No change summary. Check the diff.`를 보이고, 다시 submit하면(HEAD가 바뀔 때) 요약이 생긴다.

## 6. 사용자가 정해야 하는 결정

승인 때 바꾸지 않으면 아래 기본안대로 한다.

1. **겹침 주의를 넣을지.** 기본안은 넣는다(4.3). 빼면 mod 시험 몇 개가 줄 뿐 다른 설계는 같다.
2. **짧은 값을 줄에 바로 보일지.** 기본안은 합쳐 24자 이하면 `~ budget: 3 → 5`처럼 줄에 보이고 길면 tooltip이다. 늘 tooltip으로만 보이게 할 수도 있다.

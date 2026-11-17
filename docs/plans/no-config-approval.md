# chute.toml을 고치는 PR에 사람 승인을 요구하지 않게 하는 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 "그냥 toml 수정할 때 approve 승인 제거해줘"라고 했다. 지금(main `a64be36`)은 `approval = "agent"`(신뢰 모드)여도 PR이 `chute.toml`을 고치거나 고치는지 모르면 `승인 대기`로 두고 `chute approve`를 기다린다.

- 판정: `src/queue.rs`의 `Submission::needs_approval`가 `approval == Human || touches_config`다.
- `touches_config`는 `src/cmd/submit.rs`에서 `ConfigEdit::Yes`(바뀐 파일에 `chute.toml`이 있다)와 `ConfigEdit::Unknown`(gh가 파일 목록을 다 주지 않았다)일 때 참이다.
- 문구: submit 출력 "chute.toml을 고치는 PR이라 신뢰 모드에서도 사람 승인이 필요하다", README "동작 규칙"과 "복구 PR", 설명서(`skill/manual.md`) 규칙 7과 submit·복구 PR 절, 반송 보고서의 시간 초과 안내(`src/feedback.rs`), 현황판 NEEDS YOU 카드의 이유 문구(`mod/chute/board.js` `questionOf`).

이 PC의 두 저장소(chute, vallythevally)는 둘 다 기본 브랜치의 `chute.toml`이 `approval = "agent"`다. 이 둘에서 `chute.toml`을 고치는 PR이 사람 승인 없이 큐에 들어가야 한다.

## 2. 확정된 결정

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 승인 요구 | 신뢰 모드에서 `chute.toml`을 고치는 PR에 사람 승인을 요구하지 않는다 |
| 2 | 알고 받아들인 위험 | 작업자가 반송을 피하려고 검증 단계를 빼거나 시간 제한을 늘리는 `chute.toml` 변경도 검증만 통과하면 머지된다. 오케스트레이터 위임이나 지금처럼 사람 승인하는 대안은 고르지 않았다 |
| 3 | 사람 모드 | `approval = "human"` 저장소의 동작은 바꾸지 않는다. 모든 PR이 승인 대기로 간다 |
| 4 | 복구 PR | `--repair`의 `chute.toml` 승인 규칙도 일반 PR과 같게 맞춘다 |
| 5 | 설명서 규칙 7 | 앞문장("`chute.toml`의 검증 명령이나 시간 제한을 고쳐서 검증을 통과시키려 하지 않는다")은 남기고, 뒷문장("이 파일을 고치는 PR은 사람 승인을 기다린다")만 고친다 |
| 6 | 머지 | chute 저장소이고 chute로 머지한다. 이 플랜과 구현 PR은 `chute.toml`을 고치지 않는다 |

## 3. 스스로 정한 결정과 그 이유

### 3.1 규칙: 설정 키를 두지 않고 요구를 없앤다

| 결정 | 이유 |
|---|---|
| 새 설정 키를 두지 않는다. 신뢰 모드의 승인 판정에서 "`chute.toml`을 고친다"는 조건을 없앤다 | 사용자 뜻은 "제거"이고 두 저장소 모두 같은 동작을 원한다. 켜고 끄는 키를 두면 쓰는 곳 없는 분기와 시험, 설명이 생긴다. 그 키도 `chute.toml`에 있으니 "키를 바꾸는 PR은 승인이 필요한가"라는 질문이 되돌아온다 |
| 예외 둘만 남긴다. PR의 `chute.toml`이 이 chute로 읽히지 않거나(`config_change.kind`가 `invalid`) PR이 `chute.toml`을 지우면(`removed`) 신뢰 모드에서도 승인 대기로 간다 | 이 두 경우가 머지되면 데몬(`Engine::load_config`)과 submit(`load_default_branch_config`)이 기본 브랜치의 설정을 읽지 못해 저장소의 큐가 모두 선다. 풀려면 chute를 거치지 않고 main을 고쳐야 한다. 검증은 기본 브랜치의 옛 `chute.toml`로 돌기 때문에 이 변경을 잡지 못한다. 사용자가 받아들인 위험(결정 2)은 "검증이 느슨해진다"는 것이고 "큐가 선다"는 것은 아니다. chute 저장소에서 새 설정 키와 그 코드를 함께 올리는 PR도 설치된 chute에게는 `invalid`다. 머지되면 사람이 chute를 다시 설치해야 큐가 돈다. 이 경우를 거절하지 않고 승인 대기로 두는 것은 그 흐름이 오늘도 있기 때문이다(사람이 승인하고 다시 설치한다). 거절로 바꿀지는 6절 1번에 적는다 |
| 판정은 `Submission::needs_approval` 한 곳에서 한다. `approval == Human`이거나 `config_change`의 `kind`가 `Invalid`나 `Removed`면 승인이 필요하다. `touches_config`는 판정에 쓰지 않는다 | 상태 전이는 `src/queue.rs`에만 둔다(AGENTS.md). submit은 이미 `config_change`를 계산해 `Submission`에 넣는다 |
| `kind`가 `unreadable`(요약을 만들지 못했다)이면 승인 없이 들어간다 | 요약은 PR HEAD 객체를 가져오지 못할 때 실패하고, 그때도 submit은 경고를 낸다. 드문 경우까지 승인으로 돌리면 사용자가 다시 승인 버튼을 누르게 된다. 이 경우 머지 뒤 설정을 읽지 못할 수 있다는 위험은 남는다 |
| gh가 파일 목록을 다 주지 않은 PR(`ConfigEdit::Unknown`)도 승인 없이 들어간다. `touches_config`는 지금처럼 참으로 두고, "사람 승인을 요구한다" 경고는 없앤다 | 요약(`summarize_config_change`)이 git으로 merge-base와 PR HEAD의 `chute.toml`을 직접 비교하므로 실제로 고쳤는지, 고쳤다면 `invalid`인지를 안다. `touches_config`의 뜻("고치거나 고치는지 모른다")은 status JSON 필드라 바꾸지 않는다 |
| needs-card 플랜(`docs/plans/needs-card.md` 4.1)의 "`config_change`는 표시용이고 판정에 쓰지 않는다"를 바꾼다. `kind`가 `invalid`·`removed`인지만 판정에 쓴다. 단계별 변경 목록(`changes`)은 여전히 판정에 쓰지 않는다 | 이 두 종류는 텍스트를 읽어 바로 정해지고 판정이 흔들리지 않는다. 다른 종류는 모두 승인 없이 들어가므로 `changes`를 해석할 일이 없다 |

### 3.2 이미 승인 대기에 있는 항목

| 결정 | 이유 |
|---|---|
| 승인 대기 항목을 HEAD를 바꾸지 않고 다시 submit했는데 지금 규칙으로는 승인이 필요 없으면 큐(`queued`)로 옮긴다. `queue::step`의 `(AwaitingApproval, Resubmit { head_changed: false, needs_approval: false })`가 `Queued(None)`이 된다. submit 출력은 "다시 큐에 올렸다"다 | 이 변경이 배포되기 전에 `chute.toml` 때문에 승인 대기로 들어간 항목이 남는다. 지금은 같은 HEAD로 다시 submit해도 그대로 머문다. 사람 모드에서는 `needs_approval`가 늘 참이라 동작이 같다(결정 3) |
| 데몬이 시작할 때 남은 승인 대기 항목을 스스로 옮기지는 않는다 | 배포 때 한 번만 생기는 일이다. 사람이 승인하거나 작업자·오케스트레이터가 다시 submit하면 된다. 데몬에 옮기는 경로를 하나 더 두지 않는다 |

### 3.3 기록: 무엇을 남기는가

| 결정 | 이유 |
|---|---|
| 큐 항목의 `touches_config`와 `config_change`는 지금처럼 submit이 채운다. status JSON(`tests/fixtures/status-full.json`)의 필드 이름과 뜻은 바꾸지 않는다 | 승인 없이 들어가도 무엇을 고치는지는 현황판과 `chute status --json`에서 보여야 한다. 필드를 그대로 두면 mod와 픽스처의 계약이 그대로다 |
| `history.jsonl`의 `submitted` 이벤트에 `touches_config`(bool, `#[serde(default)]`)를 더한다. `chute events`는 그 줄에 ` · chute.toml 변경`을 붙인다 | 승인이라는 기록이 사라지므로 "언제 어느 PR이 설정을 바꾸려고 들어왔는지"를 이벤트로 따라갈 수 있게 한다. 필드를 더하는 것은 허용된다. 옛 기록은 `false`로 읽힌다. 바뀐 내용 자체는 git 이력(`git log -p -- chute.toml`)이 원본이라 이벤트에 요약을 복사하지 않는다 |
| submit 출력: 신뢰 모드에서 `chute.toml`을 고치는 PR이 큐에 들어가면 "chute.toml을 고치는 PR이다. 검증은 지금 기본 브랜치의 chute.toml로 하고, 바뀐 설정은 머지 뒤 다음 batch부터 쓴다." 한 줄을 더한다. `invalid`로 승인 대기면 "PR의 chute.toml을 이 chute가 읽지 못한다. 머지되면 큐가 서므로 신뢰 모드에서도 사람 승인이 필요하다.", `removed`면 "chute.toml을 지우는 PR이다. 머지되면 큐가 서므로 신뢰 모드에서도 사람 승인이 필요하다."를 낸다 | 작업자가 자기 설정 변경이 이번 검증에 쓰이지 않는다는 것을 바로 안다. 승인 대기일 때는 왜 기다리는지 안다 |

### 3.4 현황판(`mod/chute/`, 화면 문구는 영어)

| 결정 | 이유 |
|---|---|
| 승인 대기 카드의 질문(`Approve config change?`/`Approve to queue?`)과 변경 요약, 겹침 주의, `View diff ↗`는 그대로 둔다 | needs-card(PR #75)가 만든 화면이다. 사람 모드와 `invalid`·`removed`에는 여전히 필요하다 |
| 이유 문구(`questionOf`의 `why`)만 새 규칙에 맞춘다. 아래 표 | 지금 문구 "This PR edits chute.toml, so a person must approve it."는 신뢰 모드에서 더는 참이 아니다 |
| 승인 대기가 아닌 카드(대기, batch 안, 반송)에 `touches_config`가 참이고 `config_change.kind`가 `unchanged`가 아니면 `marksOf`에 `chute.toml` 표시(tone `warn`)를 단다 | 승인 없이 지나가는 설정 변경 PR을 사용자가 현황판에서 알아볼 수 있게 한다. `unchanged`는 gh가 목록을 잘라 생긴 것이라 실제로 고치지 않는다 |

이유 문구(위에서부터 처음 맞는 줄):

| 조건 | 질문 | 이유 |
|---|---|---|
| `touches_config`이고 `kind`가 `invalid` | `Approve config change?` | `chute can't read the chute.toml in this PR, so the queue stops after merge.` |
| `touches_config`이고 `kind`가 `removed` | `Approve config change?` | `This PR removes chute.toml, so the queue stops after merge.` |
| 저장소 `approval`이 `human` | 지금과 같다(`touches_config`와 `kind`에 따라) | `This repo asks a person to approve every PR.` |
| 그 밖(신뢰 모드에 남은 옛 항목) | 지금과 같다 | `Waiting under an old chute rule. Approve, or resubmit to queue.` |

## 4. 작업과 방식

| 작업 | 프로필 | 방식 | 이유 |
|---|---|---|---|
| `no-config-approval` | `implementer` | `poteto` | main에 무엇이 들어가는지 정하는 승인 판정과 큐 상태 전이(`src/queue.rs`)를 바꾼다. `basic` 조건에 걸린다 |

작업이 하나라 순차로 둔 작업은 없다. 플랜을 쓰는 지금 이 저장소에 도는 다른 작업 worktree는 없다.

## 5. 범위 밖

- 검증을 느슨하게 하는 `chute.toml` 변경(단계 삭제, 시간 제한 증가)을 따로 잡는 것. 사용자가 위험을 알고 받아들였다(결정 2).
- `chute.toml` 변경을 오케스트레이터 승인에 맡기는 것. 사용자가 고르지 않았다.
- PR의 `chute.toml`로 그 batch를 검증하는 것. 검증은 지금처럼 기본 브랜치의 설정으로 한다.
- 두 PR이 각각은 읽히는 `chute.toml` 변경인데 합치면 읽히지 않는 경우. batch 안에서 합친 트리의 설정을 따로 읽어 보지 않는다.
- 현황판의 최근 머지 줄에 `chute.toml` 표시를 다는 것. `recent_merged`에는 그 정보가 없다.
- `approval = "human"` 저장소의 동작.

## 6. 사용자가 정해야 하는 결정

승인 때 바꾸지 않으면 아래 기본안대로 한다.

1. **머지되면 큐가 서는 `chute.toml`(읽히지 않거나 지우는 PR)을 어떻게 할지.** 기본안은 신뢰 모드에서도 승인 대기로 둔다(3.1). 다른 안은 (가) submit이 거절하고 작업자가 고치게 하는 것(chute 저장소에서 새 설정 키를 코드와 함께 올리는 PR도 막혀, 코드를 먼저 머지하고 다시 설치한 뒤 키를 올려야 한다), (나) 예외 없이 승인 없이 들어가는 것(머지되면 큐가 서고 사람이 main을 직접 고친다)이다.
2. **남은 승인 대기 항목을 같은 HEAD로 다시 submit하면 큐로 옮길지.** 기본안은 옮긴다(3.2). 빼면 그 항목은 사람이 승인하거나 빼야 한다.

# 반송 한도·포기·작업자 사라짐을 submit한 쪽에 알리는 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자의 말: "여기서 사람 차례로 가는 게 아니라 오케스트레이터한테 알림이 가게만 하면 될 것 같은데, 오케스트레이터가 사람과 얘기하던 정해진 규약이 있던 그걸로 가게 하면 될 것 같아. chute는 반송만 하고"

지금 chute는 세 경우에 PR을 사람 차례(`needs_human`)로 둔다. 사람이 `chute retry`나 `chute drop`으로 풀기 전에는 새 HEAD로 submit해도 거절한다(`src/queue.rs`의 `explain`).

| 사유 | 언제 | 지금 하는 일 (2026-11-17 main `f86a804`) |
|---|---|---|
| `budget` | 반송을 `budget`(기본 3)번 보낸 뒤 또 실패 | `record_failure`가 `NeedsHuman(Budget)`으로 옮긴다. 반송 보고서는 쓰지만 아무에게도 전달하지 않는다. submit한 터미널에 `chute 결과 human reason=budget` |
| `gave_up` | 작업자가 `chute giveup --reason` | `NeedsHuman(GaveUp)`. `gave_up` 이벤트, submit한 터미널에 `human reason=gave_up` |
| `unreachable` | 반송을 받을 작업자가 없거나 막혔고 `respawn = []` | 반송으로 센 뒤 `NeedsHuman(Unreachable)`. 반송 줄은 아무에게도 가지 않고 submit한 터미널에 `human reason=unreachable`만 간다 |

orch는 이 알림을 받으면 사용자에게 묻고(`ask add --kind decide`) 승인 뒤 `chute retry`/`drop`을 실행한 다음 다시 `land`한다(orch `SKILL.md` 187~199줄, `orch.py`의 `chute-human-turn`). 사람 차례는 사람이 아니라 오케스트레이터가 늘 같은 규약으로 푸는 단계라, chute가 큐를 막아 둘 까닭이 없다.

## 2. 확정된 결정

요청과 사용자의 추가 승인에서 정해진 것이다.

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | chute의 몫 | 반송과 알림까지. 판단하지 않는다. 세 경우 모두 큐를 막지 않고 submit한 터미널에 알린다 |
| 2 | 다시 넣기 | 그냥 다시 submit한다(새 HEAD든 같은 HEAD든) |
| 3 | `unreachable` | 반송 보고서를 submit한 터미널에 대신 보낸다 |
| 4 | submit한 터미널이 없을 때 | 지금처럼 기록만 남기고 `chute events`로 따라잡는다 |
| 5 | orch 없이 에이전트가 스스로 submit한 경우 | 그 에이전트가 알림을 받는다 |
| 6 | 유지 | `chute.toml`을 고친 PR의 사람 승인과 `approval = "human"`의 승인 대기(`awaiting_approval`)는 그대로다 |
| 7 | orch 처리 | 한도 소진·포기 → 사용자에게 알리고 `ask add --kind decide`(버릴지, 지시를 바꿔 다시 맡길지) → 승인 뒤 다시 `land` 또는 정리. 작업자 사라짐 → `plan restart`와 반송 보고서 `send` → 새 HEAD 확인 뒤 `land` |
| 8 | 반송 뒤 재검증 (사용자 승인 2026-11-17 "응 넣어줘") | 작업자가 반송 보고서를 받아 고치면 `merge-ready`를 다시 보고하기 전에 그 저장소의 **전체 시험**(지시서의 범위 시험이 아니라 저장소 기준 전체)을 돌리고 결과를 보고서에 적는다. 오케스트레이터는 새 HEAD를 확인할 때 그 결과를 본다. `preflight` 설정(`never`)과 chute 코드는 바꾸지 않는다. 배경: PR #65가 범위 시험만 돌리고 다른 파일의 시험으로 batch에서 반송됐다 |
| 9 | 저장소와 순서 | chute 작업(chute로 머지) 뒤 재설치, 그다음 orch 작업(로컬, 원격 없음). orch 작업은 `plan add --cwd ~/.claude/skills/orch`로 올린다 |

## 3. 스스로 정한 결정

| 번호 | 항목 | 결정 | 이유 |
|---|---|---|---|
| A | `budget`·`gave_up`의 상태 | 큐에서 뺀다(항목을 지운다). `needs_human` 상태는 없어진다 | 요청의 "큐에서 빼고"를 따른다. 큐에 남기면 다시 submit할 때 반송 횟수를 어떻게 할지 따로 정해야 하고, 남은 항목이 `--repair` 한 자리 같은 큐 규칙을 계속 잡는다. 빼면 "다시 넣기 = 다시 submit"이 그대로 된다 |
| B | 빠진 PR을 다시 submit | 새 항목으로 들어가고 반송 횟수는 0부터 센다 | 옛 `chute retry`가 횟수를 0으로 돌린 것과 같다. 다시 맡길지는 오케스트레이터(또는 에이전트)가 정한다(결정 1) |
| C | `unreachable`의 상태 | 상태를 바꾸지 않는다. 반송으로 센 그대로 `bounced`에 둔다 | 반송은 이미 일어났고 받을 사람만 바뀐다. `bounced`는 큐를 막지 않고, 다시 submit하면 지금처럼 `queued`로 간다. 반송 횟수도 이어서 센다 |
| D | 알림 이름 | 새 kind `removed`: `chute 결과 removed slug=… at=… pr=<n> head=<sha> reason=budget\|gave_up` 뒤에 `budget`이면 `attempts`, `budget`, `report`(마지막 반송 보고서), `gave_up`이면 문장 안에 이유. 문장 예: "PR 12: 반송 한도 3번을 넘어 큐에서 뺐다. 다시 넣으려면 다시 submit한다." `human` kind는 없어진다 | 한 kind에 사유 두 개를 두면 받는 쪽 표가 한 줄이다. 보고서 경로를 줄에 넣어 받는 쪽이 `events`를 다시 읽지 않아도 된다 |
| E | `unreachable` 알림 | 지금의 `chute 결과 bounced` 줄을 submit한 터미널에 보내되 `delivered=unreachable`을 적는다(지금 값은 `sent`, `spawned`). `report=`는 보고서가 있으면 적는다(HEAD 바뀜 반송에는 없다). 문장은 "작업자에게 닿지 않아(<이유>) 반송을 이 터미널로 보낸다. 보고서를 작업자에게 넘기거나 고쳐서 다시 submit한다" | 반송 보고서를 대신 받는 것이니 반송 줄이 맞다. 같은 터미널이라도 건너뛰지 않는다(지금 `notify_unless_same`은 작업자와 같으면 보내지 않는다). 그 터미널도 막혔거나 없으면 `notified`에 `blocked`/`gone`/`no_terminal`로 남는다(결정 4) |
| F | 받는 곳 | 세 경우 모두 항목의 `submitter`. 작업자와 같아도 보낸다 | 결정 5. orch 아래에서는 `land`가 `HC_TERMINAL_ID`를 오케스트레이터로 두어 submitter가 오케스트레이터다 |
| G | 이벤트 | 새 `removed {pr, reason, detail, report}`를 쓴다. `reason`은 `budget`, `gave_up`, `upgrade`(I). `bounced` 이벤트에 필드 `unreachable`(이유 글, 없으면 `null`)을 더한다. `needs_human`, `gave_up` 이벤트와 `notified`의 `kind: human`, `waiting`의 `why: human`은 더 쓰지 않지만 옛 `history.jsonl` 줄은 그대로 읽고 `chute events`가 보여 준다 | 이벤트 하나로 orch가 "큐에서 빠짐"을 찾는다. 옛 기록을 못 읽으면 `chute events` 전체가 깨진다 |
| H | `waiting` 알림 | `why=human` 대신 `why=removed`. 기다리던 PR은 지금처럼 잡혀 있다 | 빠진 PR이 다시 submit돼 머지되면 기다리던 PR이 풀린다. 문장 안내(기다리거나, drop하거나, `--after` 없이 다시 submit)는 그대로 맞다 |
| I | 옛 상태 파일 | `state.json`에 `needs_human` 항목이 있어도 읽기가 실패하지 않는다. 처음 큐를 고칠 때(데몬 시작의 `recover`든 다른 명령의 `RepoStore::update`든) 큐에서 빼고 항목마다 `removed reason=upgrade` 이벤트를 한 번 남긴다(`detail`에 옛 사유). 알림은 보내지 않는다 | 그 항목은 이미 그때 `human` 알림이 갔고 오케스트레이터가 사용자에게 물었다. 다시 알리면 같은 질문이 두 번 생긴다. 빼면 사용자가 결정한 뒤 다시 submit하면 된다. 2026-11-17 이 PC의 `~/.local/state/chute/repos/*/state.json`에는 `needs_human` 항목이 없다 |
| J | `chute retry` | 명령을 지운다(`src/cmd/retry.rs`, `tests/retry.rs`, `cli.rs`의 줄, 현황판의 Retry 버튼) | 풀 상태가 없다. 다시 넣기는 submit이다. 호출하던 곳(현황판, orch 문서, `orch.py` 안내)은 이 플랜의 두 작업에서 모두 고친다 |
| K | `chute drop` | 그대로 둔다. `awaiting_approval`, `queued`, `bounced` PR을 큐에서 뺀다 | 요청의 예상대로다. 반송된 PR을 버릴 때와 승인 대기를 거절할 때 쓴다 |
| L | `chute giveup` | 큐에서 빼고 `removed reason=gave_up`을 알린다. 출력은 "큐에서 뺐다: <PR> (포기: <이유>). submit한 터미널에 알렸다" | 결정 A |
| M | 반송 보고서의 안내 | `src/feedback.rs`의 "못 풀겠으면 `chute giveup` … 사람 차례로 넘어간다"를 "큐에서 빠지고 submit한 터미널에 알린다"로 바꾼다 | 문구가 사실과 맞아야 한다 |
| N | status JSON | 필드 이름은 그대로다. `state`에 `needs_human` 값이 더 나오지 않는다. `tests/fixtures/status-full.json`의 `needs_human` 세 항목을 지금 있는 상태로 바꾸고 `mod/chute/`를 같은 PR에서 고친다 | 저장소 규칙. 필드를 빼거나 이름을 바꾸지 않는다 |
| O | 현황판 NEEDS YOU | 승인 대기만 남는다. `needs_human`의 사유 문구 세 개(`Out of retries`, `Couldn't reach the agent`, `Agent gave up`), Retry 버튼, 알림을 지운다. 큐에서 빠진 PR은 현황판에서 사라진다 | 사람이 할 일은 승인뿐이다 |
| P | 반송 한도의 뜻 | README와 `chute --skill`의 `budget` 설명을 "반송을 이만큼 보낸 뒤 또 실패하면 큐에서 빼고 submit한 터미널에 알린다. 다시 submit하면 0부터 센다"로 바꾼다. `chute --skill`은 `removed reason=budget`을 받은 에이전트에게 같은 방식으로 바로 다시 submit하지 말고 원인을 다시 보고 사용자(또는 오케스트레이터)에게 알리라고 적는다 | 결정 5. chute는 막지 않으니 되풀이를 막는 것은 받는 쪽의 안내다 |
| Q | orch 라벨 | `ls`의 chute 줄에서 `사람 차례`를 지우고, 큐에 없는 PR의 마지막 이벤트가 `removed`면 `큐에서 빠짐`으로 보인다. health의 `chute-human-turn`을 `removed` 이벤트를 보는 문제(이름은 작업자가 정한다)로 바꾸고 `다음:`에 알림 표의 `removed` 행을 적는다 | 지금 health는 큐에 없고 머지되지 않은 PR을 "결과 알림을 놓쳤을 수 있다"로만 낸다. 빠진 사유를 알면 할 일을 바로 적을 수 있다 |
| R | 작업 수 | 둘: chute 하나, orch 하나 | 사용자 주간 사용량이 90%대다. chute 쪽은 상태 전이, 알림, 문서, 현황판이 한 기능이고 서로를 써서 나누면 순차가 된다. orch는 다른 저장소다 |

### 작업과 방식

| 작업 | 저장소 | 방식 | 이유 |
|---|---|---|---|
| `no-human` | chute | `poteto` | 큐 상태 전이(main에 무엇이 들어가는지 정하는 코드)와 `state.json` 옮기기(상태 저장)를 바꾼다 |
| `orch-no-human` | orch | `poteto` | 문서가 대부분이지만 실행 중인 설치본의 코드(`orch.py`의 health와 `ls` 라벨)를 고친다 |

순차다(`no-human` → 재설치 → `orch-no-human`). orch 문서와 `orch.py`가 chute의 새 알림 이름과 이벤트를 쓰고, 설치된 chute가 바뀌기 전에 orch를 바꾸면 그 사이 `human` 알림을 처리할 행이 없다.

### 올리는 순서

```sh
# 1. 플랜 승인 뒤
$ORCH plan load <초안>/tasks.json --cwd /home/dev/code/chute --head <플랜 PR HEAD>
# 2. no-human이 chute로 머지되면 chute를 다시 설치하고 현황판 mod와 데몬을 새 판으로 바꾼다
cargo install --git https://github.com/Soochol/chute --locked --force
chute install
chute down && chute up        # chute 저장소 폴더에서
# 3. 그다음 orch 작업을 올린다
$ORCH plan add orch-no-human --cwd /home/dev/.claude/skills/orch --task-file <초안>/briefs/orch-no-human.md
```

`tasks.json`에는 chute 작업 하나만 있다. `plan load`는 `--cwd` 하나만 받기 때문이다. orch 작업은 재설치 뒤에 올리므로 `--after`를 주지 않는다.

## 4. 범위 밖

- `chute.toml`의 `preflight` 설정과 batch 검증 규칙(결정 8은 작업자 절차만 바꾼다).
- 사람이 `chute drop`으로 뺀 PR을 submit한 터미널에 알리는 것. 지금처럼 기다리던 PR에만 `waiting`이 간다.
- 큐에서 빠진 PR을 현황판에 따로 보여 주는 것.
- `respawn`의 동작(설정했을 때 새 에이전트를 띄우는 것)은 그대로다.
- orch가 `removed`를 받은 뒤 작업을 버릴 때의 정리 절차 자체. orch에 이미 있는 절차를 가리키기만 한다.

## 5. 사용자가 정해야 하는 결정

없음. 다음 둘은 기본값으로 정했으니 다르게 바라면 알려 준다.

- I: 업그레이드 때 옛 사람 차례 항목은 알림 없이 큐에서 빼고 이벤트만 남긴다.
- J: `chute retry`를 지운다(다시 넣기는 submit).

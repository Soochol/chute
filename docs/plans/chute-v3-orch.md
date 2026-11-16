# chute v3 플랜: orch 연동과 운영 보강

> 작성일 2026-11-16. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.
> v1 설계는 `docs/plans/chute-v1.md`, v2 설계는 `docs/plans/chute-v2-multi-machine.md`에 있다. 이 문서는 그 위에 더하고 바꾸는 것만 적는다.

## 1. 사용자가 풀려는 문제

chute는 지금 에이전트가 스스로 `chute submit`을 하는 흐름만 안다. 사용자는 orch 오케스트레이터 아래에서 chute를 머지 수단으로 쓰려고 한다. 그러려면 세 가지가 모자란다.

- **결과가 submit한 쪽에 돌아오지 않는다.** v1은 머지 성공을 아무에게도 알리지 않고(v1 4.3), 반송은 작업자에게만 간다. 오케스트레이터는 `chute status`를 계속 들여다봐야 한다. 놓친 알림을 따라잡을 방법도 없다.
- **권한이 너무 넓다.** 작업자가 없으면 chute가 `--dangerously-skip-permissions`로 새 에이전트를 띄운다(v1 8절의 2번). 사용자의 말: "CHUTE가 다시 띄우는건 권한이 너무 강한것 같아서 오케스트레이터 알려주고 다음을 어떻게할지 판단해서".
- **main이 깨지면 사람이 고쳐야 한다.** 큐가 `base_broken`으로 멈추면 풀 길이 사람의 직접 push뿐이다.

여기에 운영 중에 드러난 빈틈 열 가지(요청의 개선 목록, 사용자: "모두 필요한 기능같네")와, chute가 만든 파일이 지워지지 않고 쌓이는 문제(추가 요청)를 함께 다룬다.

## 2. 확정된 결정

사용자와 오케스트레이터가 정한 정책이다. 그대로 따른다.

| 항목 | 결정 |
|---|---|
| 작업자의 일 | 구현, 시험, 리뷰, `chute preflight`(필수), PR 생성, merge-ready 보고까지. submit하지 않는다 |
| 머지 실행 | 오케스트레이터가 확인한 HEAD로 `chute submit <pr> --head <HEAD> --agent <작업자 터미널>`을 실행한다. 이 submit이 머지 실행이다 |
| 결과 알림 | `chute wait`는 만들지 않는다. chute가 submit한 터미널을 기록하고 결과(merged, bounced 요약, human과 그 이유, halted, reverted)를 `hc send`로 알린다. submit한 쪽과 작업자가 같으면 하나만 보낸다. 받을 터미널이 없으면 기록만 남긴다 |
| 에이전트 띄우기 | chute는 에이전트를 새로 띄우지 않는다. `respawn` 기본값은 `[]`. 작업자가 없으면 사람 차례로 넘기고 알린다. 띄우는 것은 사용자가 직접 켜는 옵션으로만 남긴다 |
| 깨진 main | 사람이 고치지 않는다. 오케스트레이터가 에이전트에게 복구 PR을 만들게 하고 `chute submit --repair`로 넣는다. 멈춘 상태에서만, 한 번에 하나만 받는다. main+그 PR만 검증해 push한다. `--head`와 승인 규칙은 일반 submit과 같고, `chute.toml`을 고치는 PR은 사람 승인을 받는다 |
| 멈춤 알림 | 실패한 단계, 로그, 깨진 커밋, 그 커밋을 chute가 push했는지를 담는다 |
| 기준 재검증 | chute가 push한 main 커밋이라도 환경이 망가진 것으로 보이면(반송이 연달아 날 때 등) 다시 검증한다 |
| 작업자가 맞출 대상 | 대기열에 있는 PR의 diff에 맞추지 않는다. 확정된 main에만 맞춘다. 조합 실패의 상대가 아직 대기열에 있으면 blocked로 보고하고, 오케스트레이터가 상대의 결과를 보고 순서를 정한다 |
| 개선 열 가지 | 검사 환경 기록, preflight 동시 실행 상한과 빌드 캐시 공유, 흔들린 시험 처리 강화, `chute events --since`, `require_head`, revert 알림, `--priority`, `submit --after`, 샌드박스 선택지, 설명서의 "오케스트레이터 아래에서 일할 때" 절 |
| 정리(추가 요청) | 데몬이 주기적으로 정리한다. 기록(batch.json, revert.json, history)은 남기고 로그와 보고서만 지운다. preflight 슬롯 수의 상한을 `max_parallel`과 맞춘다. `chute gc [--dry-run]`. 보관 기간은 `chute.toml`에서 정한다. 다른 PC에 남은 것은 그 PC의 chute가 같은 규칙으로 지운다 |

v1·v2의 원칙(로컬 검증이 최종, 검증된 트리만 push, 강제 push 없음, 사람 확인은 큐 앞의 관문, 허브 하나가 대기열 원본)은 그대로다.

**v1·v2에서 바뀌는 것**은 셋이다. 정책에서 나온 것이라 여기 모아 둔다.

- `respawn` 기본값이 `["claude", "--dangerously-skip-permissions"]`에서 `[]`가 된다(v1 8절의 2번을 사용자가 다시 정했다).
- 머지 성공을 알린다. 다만 받는 쪽은 submit한 터미널이다. 작업자가 스스로 submit했으면 작업자가 받는다(v1 4.3의 "머지 성공은 보내지 않는다"를 바꾼다).
- chute가 push한 기준 커밋을 늘 믿지 않는다(v1 4.2의 8번을 바꾼다. 4.4).

## 3. 확인한 사실

`main`(7fe642b)의 코드와 이 PC에서 확인했다.

**v2 진행 상태.** 같은 orch 계획에 `forward-cmds`가 진행 중이고 `docs-multi`가 그 뒤에서 기다린다. 나머지 v2 작업은 병합됐다(#16~#22). orch의 `plan load`는 이미 계획에 있는 작업을 `after`로 가리킬 수 있다. 그래서 이 플랜의 첫 작업이 두 작업을 기다리게 한다.

**알림과 전달.**

- `src/feedback.rs`의 `deliver`는 반송을 작업자에게만 보낸다. 살아 있는지는 `hc get`의 `state`와 `created_ms`로 본다(v1 3절). 다른 머신의 작업자는 그 머신의 `chute deliver`가 받는다(v2 4.5).
- 큐 항목에는 `agent` 하나뿐이다(`src/model.rs`의 `QueueItem`). `src/cmd/submit.rs`의 `identify_agent`는 `--agent`가 없으면 `HC_TERMINAL_ID`를 쓴다. 오케스트레이터가 `--agent <작업자>`로 submit하면 오케스트레이터 자신의 터미널은 어디에도 남지 않는다.
- `hc send`는 셸 문자를 받지 않는다. 그래서 알림 줄은 `without_shell_syntax`로 글자, 공백, `.,:/_-+=@%`만 남긴다. 알림 형식(5.2)은 이 글자만 쓴다.
- `history.jsonl`은 한 줄에 사건 하나이고 모든 줄에 `at_ms`가 있다(`EventKind`: merged, bounced, flaky, batch_finished, reverted, gave_up, closed, needs_human, blocked). `RepoStore::history`는 읽지 못한 줄을 건너뛰고 번호만 센다. `chute events`는 이 파일을 읽으면 된다.

**멈춤과 기준.**

- `src/engine.rs`의 `run_once`는 `blocked.base`가 지금 main과 같으면 아무것도 하지 않는다. main이 바뀌면 멈춤을 푼다. 복구 PR이 들어갈 길이 없다.
- `base_sound`는 `verified_bases`에 있는 커밋을 다시 검증하지 않는다. chute가 push한 tip은 모두 `verified_bases`에 들어간다(`land`). 그래서 환경이 망가지면 chute가 push한 main 위에서 모든 PR이 단독 실패로 반송된다.
- 기준 검증이 실패해도 그 실패(단계, 로그)는 버려진다. `Blocked`에는 `reason`, `base`, `since_ms`만 있다.

**preflight 슬롯과 정리.**

- `Gitops::lock_preflight_slot`은 1번부터 잡히지 않은 슬롯을 찾고, 모두 잡혀 있으면 새 번호를 만든다. 상한이 없고 슬롯은 줄지 않는다. 슬롯마다 빌드 캐시(`target/` 등)를 따로 가져서 새 슬롯은 처음부터 빌드한다.
- 정리는 `src/cmd/preflight.rs`의 `prune`(preflight 로그, 커밋 보고서, preflight 실패 ref, 7일 `KEEP_FOR`)과 `src/engine.rs`의 `prune_failed_refs`(큐에서 빠진 PR의 batch 실패 ref)뿐이다. preflight를 실행하지 않는 PC에서는 아무것도 지워지지 않는다. 지워지지 않는 것은 추가 요청이 적은 그대로다(batch 검증 로그, `reports/pr-<n>/`, 슬롯, respawn이 만든 worktree, 다른 PC에 `deliver`가 만든 보고서와 ref).

**흔들림.** `verify::run`은 실패한 단계를 같은 트리에서 한 번 더 돌려 통과하면 `FlakyStep`으로 돌려준다. engine은 history와 batch 기록에 남기고 머지한다(v1 결정). 장부, 재현 실험, 만성 표시는 없다. orch 문지기(`~/.claude/skills/orch/SKILL.md`의 flaky 절, `gates.json`의 `probe`)는 기준 트리에서 그 시험만 되풀이해 근거(`base`, `unresolved`, `flaky-made`)를 모으고, 30일에 3번 넘게 흔들린 시험에 `chronic`을 붙인다. 이 플랜은 그 생각을 빌리되 코드를 옮기지 않는다.

**설정과 설명서.** `chute.toml`은 `deny_unknown_fields`로 읽는다. `tests/docs.rs`는 `config::draft`의 모든 키가 README에 있는지, README가 respawn 기본값과 그 권한 효과를 밝히는지 본다. 설명서(`skill/manual.md`)는 바이너리에 들어가는 고정 글이다. 이 PC에 `bwrap`이 있다(`/usr/bin/bwrap`).

## 4. 스스로 정한 결정과 이유

### 4.1 submit한 쪽(submitter)을 따로 기록한다

큐 항목에 `submitter: Option<Agent>`를 더한다. 값은 submit을 실행한 터미널(`HC_TERMINAL_ID`)이고, 확인 방법은 `agent`와 같다(`hc get`의 `created_ms`, 머신 id). `agent`는 지금처럼 `--agent` 또는 `HC_TERMINAL_ID`다. 그래서

- 오케스트레이터가 `--agent <작업자>`로 submit하면 `submitter`는 오케스트레이터, `agent`는 작업자다.
- 작업자가 스스로 submit하면 둘이 같다.
- 사람이 hc 밖의 셸에서 submit하면 `submitter`가 없다. 알림은 기록만 남는다.

같은 PR을 다시 submit하면 `submitter`를 새 값으로 바꾼다(없으면 그대로 둔다). 마지막으로 머지를 지시한 쪽이 결과를 받아야 하기 때문이다. 허브가 아닌 PC의 submit은 v2의 숨긴 에이전트 인자처럼 submitter도 넘긴다.

### 4.2 알림은 새 모듈 하나가 보낸다

`src/notify.rs`가 알림 한 줄을 만들고 보낸다. 반송 전달(`feedback`)과 떼어 둔 이유는, 알림은 실패해도 큐 상태를 바꾸지 않기 때문이다. 반송은 전달하지 못하면 사람 차례(`unreachable`)로 가지만, 알림은 못 보내면 기록만 남는다(2절).

- 받는 터미널이 살아 있고 `created_ms`가 같으면 `hc send`로 보낸다. 다른 머신의 터미널이면 허브가 `hc send <그 머신 이름>/<터미널>`로 바로 보낸다(v2 3.1: `send`는 `machine/t7`을 받는다). 반송처럼 `chute deliver`를 거치지 않는다. 보낼 것이 한 줄뿐이고 파일이 따라가지 않아서다.
- 대상이 `blocked`(종료 코드 9)이거나, 없거나, 끝났거나, 그 머신을 허브가 모르면 보내지 않는다. 어느 경우든 history에 `notified` 사건을 남긴다(`delivered`에 결과). `chute events`가 이것을 보여 주므로 오케스트레이터가 놓친 알림을 찾을 수 있다(4.6).
- 알림은 사건이 history에 쓰인 뒤에 보낸다. 데몬이 알림 도중 죽어도 사건은 남고, 다시 보내지는 않는다. 같은 알림이 두 번 가는 것보다 한 번 빠지는 것이 낫고, 빠진 것은 `events`로 따라잡는다.

**누가 무엇을 받나.**

| 결과 | 작업자(`agent`) | submit한 쪽(`submitter`) |
|---|---|---|
| merged | 받지 않는다 | `merged` |
| 반송 | v1의 반송 줄과 보고서 | `bounced` 요약. 작업자와 같은 터미널이면 보내지 않는다 |
| 사람 차례(예산 소진, 전달 실패, 포기) | 받지 않는다 | `human`과 이유 |
| 큐 멈춤 | 받지 않는다 | `halted`. 그때 큐에 있는 모든 항목의 submitter에게, 터미널마다 한 번 |
| 멈춤 풀림 | 받지 않는다 | `resumed`. 멈춤 알림을 받았던 터미널과 복구 PR의 submitter에게 |
| 되돌리기 | 받지 않는다 | `reverted`. 되돌린 PR의 submitter에게, 터미널마다 한 줄 |
| 선행 PR이 막힘(4.8) | 받지 않는다 | `waiting` |
| 흔들림이 만들어졌거나 만성(4.10) | 받지 않는다 | `flaky` |

"같은 터미널"은 머신 id, 터미널, `created_ms`가 모두 같은 것이다. 되돌린 PR의 submitter는 큐에서 이미 빠졌으므로 batch 기록의 머지 항목(`MergeRef`)에 `submitter`와 `agent`를 함께 남겨 둔다. revert는 그것을 읽는다. 되돌리기는 여전히 작업자에게 알리지 않는다(v1 4.6). 머지를 지시한 쪽이 다음을 정한다.

`respawn`이 비어 있지 않으면(사용자가 켠 경우) v1대로 띄우고, `bounced` 알림의 `delivered=spawned`와 새 터미널로 그 사실을 알린다.

### 4.3 복구 PR (`submit --repair`)

- **받는 조건.** 저장소가 멈춰 있어야 한다(`blocked`가 있음). 큐에 아직 머지되지 않은 복구 항목이 있으면 거절한다. 두 조건 모두 거절 메시지에 지금 상태와 할 일을 적는다(종료 코드 1). `--head`, `require_head`, 승인(`approval = "human"`, `chute.toml`을 고치는 PR은 승인 대기)은 일반 submit과 같다.
- **검증.** 데몬은 멈춰 있어도 복구 항목이 `queued`면 그것 하나만 batch로 가져간다. 지금 main에 그 PR 하나만 합쳐 검증한다. 다른 항목과 묶지 않으므로 이분 탐색과 조합 판정은 없다.
- **통과하면** push하고, 새 tip을 검증된 기준으로 남기고, 멈춤을 푼다. 그 PR의 submitter에게 `merged`를, 멈춤 알림을 받은 터미널들에게 `resumed by=repair`를 보낸다. 그 뒤 다음 batch부터 평소처럼 흐른다.
- **실패하거나 충돌하면** 일반 반송과 같이 작업자에게 보고서를 보내고 예산을 한 번 쓴다. 큐는 계속 멈춰 있다. 다시 고친 HEAD로 `--repair` submit을 하면 같은 항목이 다시 들어간다.
- **멈춤이 다른 이유로 풀리면**(누군가 main에 직접 push해 기준이 바뀜) 복구 항목은 일반 항목이 된다. `repair` 표시는 기록으로 남고, batch를 고를 때 맨 앞에 선다.

복구 PR만 검증하는 것은 정책이다(2절). 큐의 다른 PR을 함께 넣으면 복구가 그 PR들의 실패에 묶인다.

### 4.4 멈춤 알림의 내용과 기준 재검증

`Blocked`에 필드를 더한다. `step`(실패한 단계), `log`(그 로그 경로), `pushed_by_chute`(깨진 커밋이 chute의 batch 기록 `pushed`에 있는가), `rechecked`(이미 검증된 기준을 다시 검증해서 실패했는가). `base_sound`가 버리던 실패를 여기에 남긴다. 정리(4.12)는 `blocked.log`가 가리키는 로그를 지우지 않는다.

**다시 검증하는 조건.** 같은 기준 커밋 위에서 서로 다른 PR 두 개가 연달아 단독 실패(검증 실패나 시간 초과)로 판정되면, 두 번째를 반송하기 전에 그 기준을 다시 검증한다. 그 기준이 `verified_bases`에 있어도 그렇다. 사이에 그 기준 위에서 통과한 검증이 있으면 세지 않는다.

- 기준이 통과하면 두 번째를 반송하고 세기를 0으로 한다.
- 기준이 실패하면 반송하지 않고 멈춘다(`rechecked: true`). 첫 번째 반송은 이미 나갔다. `halted` 알림이 그 사실을 알 수 있게 첫 번째 PR 번호를 함께 적는다(5.2의 `suspect`).

세는 값은 `State.base_doubt`(`{base, prs}`)에 둔다. 데몬이 다시 시작해도 이어진다. 두 번으로 정한 것은, 한 번은 PR 자신의 잘못이 흔하고, 세 번을 기다리면 틀린 반송이 두 번 나가기 때문이다. 문턱은 설정으로 두지 않았다(8절의 3번).

### 4.5 `require_head`

`chute.toml`의 `require_head = true`이면 `--head` 없는 submit을 거절한다(종료 코드 1). 기본값은 `false`다. v1·v2 사용자를 깨지 않기 위해서다.

- 허브가 아닌 PC의 submit은 `--head`가 없으면 로컬 HEAD를 채워 넘긴다(v2 4.4). 이것이 `require_head`를 우회하지 않도록, 넘길 때 사용자가 `--head`를 주지 않았다는 숨긴 표시를 함께 보낸다. 허브는 그 표시가 있으면 `--head` 없는 submit으로 본다.
- 거절 메시지는 "이 저장소는 확인한 HEAD(`--head`) 없이 submit을 받지 않는다. 오케스트레이터 아래에서 일한다면 submit하지 말고 PR과 HEAD를 보고한다"이다.
- `chute --skill`은 지금 폴더가 등록된 저장소이고 기본 브랜치의 설정이 `require_head = true`이면 설명서 맨 앞에 "이 저장소에서는 submit하지 말고 보고한다"는 줄을 붙인다. 설정을 읽지 못하면 붙이지 않고 설명서만 출력한다. 설명서 본문의 "오케스트레이터 아래에서 일할 때" 절(4.13)이 같은 규칙을 설명한다.

### 4.6 `chute events`

`chute events [--since <시각>] [--pr <n>] [--limit <n>] [--json]`이 history를 읽어 출력한다. 허브가 아닌 PC에서는 허브로 넘긴다(history는 허브에 있다).

- `--since`는 epoch 밀리초, RFC 3339 시각, 상대 시간(`30m`, `2h`, `1d`) 가운데 하나다. 그 시각 이후(같은 시각 포함)의 사건을 낸다. 같은 `at_ms`의 사건이 여럿일 수 있어 포함으로 했다. 소비자는 `(at_ms, event, pr)`로 겹침을 거른다.
- `--json`은 history 한 줄과 같은 모양의 JSON을 한 줄에 하나씩 낸다(JSON Lines). 사람용 출력은 한 사건에 한 줄이다.
- `--limit`의 기본값은 500이고 가장 최근 것부터 자른 뒤 시간 순으로 낸다. 잘렸으면 stderr에 알린다. 넘긴 명령의 출력 한도(v2의 `hc exec`) 안에 들게 하려는 것이다.
- 새 사건 종류를 더한다(5.4): `submitted`, `dropped`, `notified`, `resumed`, `flaky_probe`. 그래서 오케스트레이터는 "무엇이 일어났나"와 "무엇을 알렸나"를 한 출력에서 본다. `notified`의 `delivered`가 `sent`가 아닌 줄이 놓친 알림이다.

### 4.7 `--priority`

`submit --priority`는 항목에 `priority` 표시를 붙인다. batch를 고를 때 복구 항목, `priority` 항목, 나머지 순으로 각자 올린 순서대로 가져간다. `max_batch` 상한과 CI 대기 규칙은 그대로다. 이미 돌고 있는 batch를 끊지는 않는다. 같은 HEAD로 다시 submit하면서 `--priority`를 주거나 빼면 표시만 바뀐다(`change: options_updated`).

chute는 누가 `--priority`를 쓸 수 있는지 막지 않는다. v2 4.9와 같은 이유로, 지킬 수 없는 권한을 만들지 않는다. "사용자의 승인 범위 안에서만 쓴다"는 설명서의 규칙이다(4.13).

### 4.8 `submit --after <N>`

`submit --after <N>`(여러 번 줄 수 있다)는 N이 머지될 때까지 그 항목을 batch에 넣지 않는다.

- submit할 때 N은 큐에 있어야 한다(상태는 무엇이든). 큐에 없고 GitHub에서 이미 머지됐으면 기다릴 것이 없으니 표시 없이 받는다. 큐에도 없고 머지되지도 않았으면 거절한다. 자기 자신이나 순환(N이 이미 그 PR 뒤에 있음)도 거절한다.
- N이 머지되면 모든 항목의 `after`에서 N을 지운다. 그래서 N이 든 main 위에서 다음 batch가 그 항목을 가져간다. N과 같은 batch에 들지 않는다.
- N이 반송되거나 사람 차례가 되거나 큐에서 빠지면(drop, PR 닫힘) 그 항목은 계속 잡혀 있고, 그 항목의 submitter에게 `waiting`을 보낸다. 오케스트레이터가 다음을 정한다. 기다리거나, `chute drop`하거나, `--after` 없이 다시 submit한다(같은 HEAD의 다시 submit은 `after`를 새 값으로 바꾼다).
- 잡혀 있는 항목은 상태가 `queued` 그대로이고, 상태 JSON의 `after`가 비어 있지 않다. 새 상태 값을 만들지 않은 이유는 `queue.rs`의 전이표와 mod의 상태 표시를 건드리지 않기 위해서다. mod는 `after`를 보고 "#N 뒤"를 표시한다.

### 4.9 검사 환경 기록 (`env_check`)

`chute.toml`의 `env_check = ["cargo --version", "git --version"]`은 검증 전에 슬롯 폴더에서 `sh -c`로 하나씩 돌릴 명령이다. 기본값은 `[]`이다.

- 명령마다 30초 제한, 출력(stdout과 stderr)의 첫 줄을 200자까지 남긴다. 실패해도 검증은 그대로 돈다(`ok: false`로 남긴다). 검증 결과를 바꾸는 장치가 아니라 근거를 남기는 장치이기 때문이다.
- `env_check`가 비어 있어도 머신(호스트 이름과 hc 코어 id)은 늘 남긴다.
- batch는 한 번 돌리고 그 batch의 기록(`BatchRecord.env`)과 그 batch에서 나온 반송 보고서에 넣는다. preflight는 실행마다 돌리고 preflight 기록(`PreflightRecord.env`)과 보고서, `--json` 출력에 넣는다. 허브가 아닌 PC의 preflight 기록은 v2대로 허브에 넘어가므로 env도 같이 간다.
- 반송 보고서의 "검사 환경" 절은 허브의 환경을 적고, 그 PR의 같은 HEAD에 대한 preflight 기록에 env가 있으면 다른 줄만 나란히 보인다. "preflight는 통과했는데 batch에서 실패했다"의 원인이 PC 차이인지 알아보게 하려는 것이다.

### 4.10 흔들린 시험: 장부, 되풀이 실험, 만성 알림

v1의 결정(흔들림으로 판정된 batch는 머지하고 기록한다)은 그대로다. 그 위에 근거를 모은다.

- **장부.** history의 `flaky` 사건이 장부다. 새 명령 `chute flaky [--days <n>] [--json]`이 (단계, 시험)마다 흔들린 횟수, 마지막 시각, 근거, 만성 여부를 보인다. 허브로 넘긴다. 시험 이름이 없는 흔들림(단계에 `failed` 정규식이 없음)은 단계 이름으로 센다.
- **되풀이 실험(probe).** `[[verify]]` 단계에 `probe = "cargo test {tests} -- --exact"`처럼 `{tests}` 자리가 있는 명령을 둘 수 있다. batch가 끝난 뒤(push와 기록 뒤) 흔들린 시험이 있고 그 단계에 `probe`가 있으면, batch 슬롯에서 먼저 batch를 시작할 때의 기준 트리, 다음으로 흔들린 트리에서 그 시험만 되풀이한다. 첫 실패에서 멈추고, 판마다 `flaky.probe_repeats`(기본 20)번 또는 `flaky.probe_seconds`(기본 180)초까지다. 결과는 `flaky_probe` 사건으로 남는다.
  - 기준에서 실패: `base`. 원래 있던 흔들림이다.
  - 기준에서는 안 나오고 흔들린 트리에서 실패: `made`. 그 batch가 만들었을 수 있다. 이미 머지됐으므로 그 batch PR들의 submitter에게 `flaky` 알림을 보낸다.
  - 어디서도 안 나옴: `unresolved`.
  - 실험은 다음 batch를 그만큼 늦춘다. 시간 상한이 그 값이다. `probe`가 없는 단계는 실험하지 않는다.
- **만성.** 같은 (단계, 시험)이 `flaky.chronic_days`(기본 30)일 안에 `flaky.chronic`(기본 3)번 흔들리면 상태의 경고(`flaky_chronic`)에 올리고, 그 batch PR들의 submitter에게 `flaky` 알림(`chronic=yes`)을 보낸다. 같은 시험의 만성 알림은 하루에 한 번이다.

orch 문지기의 `flaky-made` 분리(묶음을 반씩 나눠 범인을 찾고 머지를 막음)와 `hold`는 넣지 않는다. chute는 흔들린 batch를 머지하기로 사용자가 정했고, 실험을 머지 앞에 두면 그 결정을 뒤집게 된다.

### 4.11 preflight 동시 실행 상한과 빌드 캐시

- **상한.** `max_parallel`(기본 2)이 preflight 슬롯 수의 상한이다. 슬롯이 모두 잡혀 있으면 preflight는 새 슬롯을 만들지 않고 빈 슬롯을 기다린다. 기다리는 동안 stderr에 한 번 "다른 preflight N개가 돌고 있어 기다린다"를 쓴다. batch 슬롯과 revert 슬롯은 세지 않는다. 기본값 2는 cargo 빌드처럼 CPU를 다 쓰는 검사가 batch 검증과 겹치는 것을 생각해 작게 잡았다(8절의 1번).
- **캐시 씨앗.** `[cache] seed = ["target"]`은 슬롯을 새로 만들거나 다시 만들 때, 가장 최근에 쓴 다른 슬롯에서 복사해 올 git이 무시하는 경로다. `cp -a --reflink=auto`로 복사한다(같은 파일 시스템의 btrfs·XFS에서는 거의 공짜, 아니면 보통 복사). 슬롯이 이미 있으면 복사하지 않는다. 복사가 실패하면 경고만 하고 빈 채로 검증한다.
- **공유 폴더.** 검증 명령에 `CHUTE_CACHE_DIR`(저장소마다 하나, `slots/<owner>__<name>/cache`)을 넘긴다. sccache나 내려받기 캐시처럼 여러 슬롯이 동시에 써도 되는 도구가 쓰는 자리다. chute는 이 폴더를 만들기만 하고 내용은 건드리지 않는다.

슬롯끼리 `target/`을 그대로 함께 쓰게 하는 길(`CARGO_TARGET_DIR` 공유)은 택하지 않았다. 서로 다른 트리가 같은 산출물을 덮어써 매번 다시 빌드하고, cargo의 잠금 때문에 동시 실행이 사실상 하나로 줄어든다. 씨앗 복사는 언어와 도구를 가리지 않는다.

### 4.12 정리 (`chute gc`)

`src/gc.rs`가 정리 규칙 하나를 갖고, 세 곳에서 부른다. 데몬(시작할 때와 한 시간마다), `chute gc`, 그리고 데몬이 없는 PC를 위해 `preflight`와 `deliver`의 끝(한 시간에 한 번만, 상태 폴더의 `gc.json`에 마지막 시각을 남긴다). `src/cmd/preflight.rs`의 `prune`은 이 규칙으로 옮긴다.

보관 기간은 `chute.toml`의 `[retention]`이다. 기본값은 `logs_days = 7`(지금의 `KEEP_FOR`와 같다), `reports_days = 30`이다.

| 대상 | 지우는 조건 |
|---|---|
| batch 검증 로그 `batches/<id>/verify-*/` | `logs_days`보다 오래됐다. 지금 batch와 `blocked.log`가 가리키는 것은 남긴다. `batch.json`, `revert.json`은 지우지 않는다 |
| preflight 로그 `preflight/<…>/` | `logs_days`보다 오래됐다 |
| 반송 보고서 `reports/pr-<n>/` | 그 PR이 큐에 없고 `reports_days`보다 오래됐다. 허브가 아닌 PC(큐를 모른다)는 나이만 본다 |
| preflight 보고서 `reports/commit-*/` | `reports_days`보다 오래됐다 |
| batch 실패 ref `refs/chute/failed/pr<n>-<batch>` | 허브: 지금처럼 PR이 큐에서 빠지면. 허브가 아닌 PC: batch id의 시각이 `reports_days`보다 오래됐다 |
| preflight 실패 ref `refs/chute/failed/preflight-…` | `reports_days`보다 오래됐다. 보고서가 이 ref를 가리키므로 보고서와 같이 간다 |
| preflight 슬롯 `preflight-<k>` | `k > max_parallel`이고 아무도 잡고 있지 않다. `git worktree remove --force` 뒤 폴더를 지운다 |

- `history.jsonl`, `state.json`, `batch.json`, `revert.json`은 지우지 않는다. 기록이고 크지 않다.
- **respawn이 만든 worktree(`~/.herdr/worktrees/…`)는 지우지 않는다.** 에이전트의 커밋하지 않은 작업이 남아 있을 수 있고, 사용자의 Herdr 작업 공간이다. `respawn` 기본값이 `[]`가 되면 새로 생기지 않는다. README가 남은 것을 손으로 지우는 방법을 안내한다.
- `chute gc [--dry-run] [--json]`은 지운(또는 지울) 것과 크기를 출력한다. 언제나 이 PC의 상태 폴더만 다룬다. 허브로 넘기지 않는다. "다른 PC에 남은 것은 그 PC의 chute가 같은 규칙으로 지운다"를 그대로 옮긴 것이다.
- 정리가 실패해도 데몬과 preflight는 멈추지 않는다. 상태에 `gc` 경고를 남긴다.

### 4.13 설명서: "오케스트레이터 아래에서 일할 때"

`skill/manual.md`에 절을 더한다. 담을 것은 다음과 같다.

- 오케스트레이터가 있으면 작업자는 preflight를 통과시키고 PR을 만든 뒤 PR 번호와 HEAD를 보고한다. submit하지 않는다. `require_head = true`인 저장소에서는 늘 그렇다.
- 맞출 대상은 확정된 main(`origin/main`)뿐이다. 큐에 있는 다른 PR의 diff에 맞추지 않는다. preflight의 조합 실패 상대가 아직 큐에 있으면 고치지 말고 blocked로 보고한다(상대 PR 번호와 보고서 경로).
- 반송을 받으면 v1대로 고친다. 고친 뒤에도 submit하지 않고 새 HEAD로 다시 보고한다.
- 오케스트레이터가 쓰는 것(`submit --head --agent`, `--priority`는 사용자가 허락한 범위에서만, `--after`, `--repair`, `events --since`, 알림 형식)을 따로 적는다. 오케스트레이터도 이 설명서를 읽기 때문이다.

### 4.14 샌드박스: 이번에는 설계만

검사를 샌드박스에서 돌리는 선택지는 이번에 구현하지 않고 설계만 남긴다. 판단 근거는 셋이다. 이번 플랜이 이미 작업 11개다. 샌드박스의 뜻(네트워크, 쓸 수 있는 경로, 장치)은 도구마다 다르고, 잘못 막으면 모든 PR이 반송되는데 그 판정이 사용자의 저장소마다 다르다. 그리고 v2 4.9처럼 이 대상(한 사람의 PC들)에서는 검증 명령이 이미 사용자 권한으로 돈다.

다음 플랜이 쓸 설계:

```toml
[sandbox]
wrap = ["bwrap", "--die-with-parent", "--unshare-net", "--ro-bind", "/", "/",
        "--bind", "{slot}", "{slot}", "--bind", "{cache}", "{cache}", "--tmpfs", "/tmp", "--"]
```

- `wrap`은 `env_check`, 각 단계의 `run`, `probe` 앞에 붙는 argv다(`wrap + ["sh", "-c", run]`). `{slot}`은 슬롯 폴더, `{cache}`는 `CHUTE_CACHE_DIR`로 바뀐다.
- 프로세스 그룹 종료(v1 4.2의 9번)는 wrapper가 같은 그룹에 있어야 그대로 된다. `bwrap --die-with-parent`는 된다. `systemd-run --scope`처럼 그룹을 옮기는 wrapper는 받지 않는다(설정을 읽을 때 거절할 목록을 둔다).
- `wrap[0]`이 없으면 검증할 수 없다. 반송하지 않고 엔진 경고로 멈춘다.
- 시험은 `bwrap`이 없는 환경에서도 돌도록 가짜 wrapper 스크립트(인자를 기록하고 나머지를 exec)로 한다.

### 4.15 시험

저장소 규칙 그대로다. 알림은 가짜 `hc`가 받은 `send`를 기록하고, 시험은 그 기록으로 확인한다. 다른 머신의 submitter는 v2의 가짜 머신 망으로 확인한다. 흔들림 실험은 실패 횟수를 파일로 세는 셸 단계로 만든다.

## 5. 계약

첫 작업(`v3-core`)이 5.1, 5.3, 5.4의 타입과 설정, 상태 JSON 필드, fixture를 코드로 옮긴다. 뒤 작업은 필드를 더할 수는 있지만 이름과 뜻을 바꾸지 않는다. 5.2는 `notify` 작업이 만들고, orch 전환 작업이 이것을 읽는다.

### 5.1 명령

| 명령 | 누가 | 하는 일 |
|---|---|---|
| `chute submit [<pr>] [--head <SHA>] [--agent <주소>] [--priority] [--after <N>]… [--repair] [--json]` | 오케스트레이터, 에이전트 | 4.3, 4.5, 4.7, 4.8. 거절은 종료 코드 1 |
| `chute events [--since <시각>] [--pr <n>] [--limit <n>] [--json]` | 오케스트레이터, 사람 | 4.6. 허브로 넘긴다 |
| `chute flaky [--days <n>] [--json]` | 사람, 오케스트레이터 | 4.10. 허브로 넘긴다 |
| `chute gc [--dry-run] [--json]` | 사람, 오케스트레이터 | 4.12. 이 PC만 다룬다 |

`submit --json`의 `change`에 `options_updated`가 더해진다(같은 HEAD에서 `priority`나 `after`만 바뀜).

### 5.2 알림 한 줄

```
chute 결과 <kind> slug=<owner/name> <key>=<value> … <한국어 한 문장>
```

- 글자는 글자, 숫자, 공백, `.,:/_-+=@%`만 쓴다(3절). 값에는 공백이 없다. 목록은 쉼표로 잇는다(`prs=12,13`). `key=value`가 아닌 첫 낱말부터 끝까지가 사람용 문장이다.
- SHA는 12자다. 경로에 쓸 수 없는 글자가 있으면 그 키를 빼고, 문장에 "`chute events`로 본다"를 적는다.
- 모든 줄에 `slug`와 `at=<epoch ms>`(그 사건의 history `at_ms`)가 있다. `chute events --since <at>`로 같은 사건을 찾을 수 있다.

| kind | 키 |
|---|---|
| `merged` | `pr`, `head`, `batch`, `commit`, `repair=yes`(복구 PR이면) |
| `bounced` | `pr`, `head`, `reason`(`conflict`, `verify`, `timeout`, `head_changed`, `ci`), `mode`(`solo`, `combo`, 해당 없으면 없음), `partners`(조합이면), `attempts`, `budget`, `to`(작업자 주소), `delivered`(`sent`, `spawned`), `report` |
| `human` | `pr`, `reason`(`budget`, `unreachable`, `gave_up`). 문장에 이유 |
| `halted` | `base`, `step`, `pushed_by_chute`(`yes`, `no`), `rechecked`(`yes`, `no`), `log`, `suspect`(4.4에서 이미 반송된 PR이 있으면) |
| `resumed` | `base`(새 기준), `by`(`repair`, `main_moved`), `pr`(복구 PR이면) |
| `reverted` | `batch`, `prs`, `commit`(revert들을 올린 tip) |
| `waiting` | `pr`, `after`, `why`(`bounced`, `human`, `dropped`, `closed`) |
| `flaky` | `batch`, `prs`, `step`, `tests`, `evidence`(`made`, 만성만이면 없음), `chronic`(`yes`면) |

예:

```
chute 결과 merged slug=acme/widget at=1763270000123 pr=14 head=1a2b3c4d5e6f batch=b20261116-140000 commit=9f8e7d6a5b4c PR 14가 main에 머지됐다.
chute 결과 halted slug=acme/widget at=1763270100456 base=9f8e7d6a5b4c step=test pushed_by_chute=yes rechecked=yes log=/home/me/.local/state/chute/repos/acme__widget/batches/b20261116-141000/verify-3/test.1.log suspect=15 큐가 멈췄다. 복구 PR을 chute submit --repair로 넣는다.
```

### 5.3 설정 `chute.toml`에 더하는 키

```toml
require_head = false    # true면 --head 없는 submit을 거절한다
max_parallel = 2        # 동시에 도는 preflight 수 상한(슬롯 수 상한)
env_check = []          # 검증 전에 돌려 결과를 남길 명령. 예: ["cargo --version"]
respawn = []            # 바뀐 기본값. 작업자가 없으면 사람 차례로 넘긴다

[retention]
logs_days = 7           # 검증 로그와 preflight 로그
reports_days = 30       # 반송 보고서, preflight 보고서, 실패 트리 참조

[cache]
seed = []               # 새 슬롯에 복사해 올 무시된 경로. 예: ["target"]

[flaky]
probe_repeats = 20
probe_seconds = 180
chronic = 3
chronic_days = 30

[[verify]]
probe = "…{tests}…"     # 선택. {tests}가 없으면 설정 오류다
```

`max_parallel`, `retention`의 값, `flaky`의 값은 1 이상이어야 한다. `respawn`이 비어 있지 않으면 v1대로 띄우고, `chute init`의 초안 주석이 그 권한 효과를 밝힌다.

### 5.4 저장 형식과 상태 JSON에 더하는 것

`schema`는 1 그대로다. 있는 필드의 이름과 뜻은 바꾸지 않는다. 더하는 필드는 모두 옛 `state.json`과 `history.jsonl`이 그대로 읽히게 기본값을 갖는다.

- `QueueItem`(상태 JSON의 `queue[]`): `submitter`(상태 JSON에서는 `agent`와 같은 모양의 `AgentView`, 없으면 `null`), `priority`(bool), `after`(PR 번호 배열), `repair`(bool).
- `Blocked`(상태 JSON의 `blocked`): `step`, `log`(없으면 `null`), `pushed_by_chute`, `rechecked`(bool).
- `State.base_doubt`: `{base, prs}` 또는 없음. 상태 JSON에는 내지 않는다.
- `MergeRef`(batch 기록의 `merges[]`): `submitter`, `agent`.
- `BatchRecord.env`, `PreflightRecord.env`: `{machine_id, host, checks: [{run, output, ok}]}`.
- `warnings[].kind`에 `flaky_chronic`, `gc`.
- history의 새 사건: `submitted {pr, head, priority, after, repair}`, `dropped {pr}`, `notified {kind, pr, to, delivered}`(`delivered`: `sent`, `same_as_agent`, `no_terminal`, `gone`, `blocked`, `unknown_machine`, `failed`), `resumed {base, by, pr}`, `flaky_probe {batch, step, tests, evidence, runs, failures}`(`evidence`: `base`, `made`, `unresolved`). 이 버전보다 옛 chute는 이 줄들을 읽지 못한 줄로 센다.
- `tests/fixtures/status-full.json`에 우선 항목, 다른 PR 뒤에 잡힌 항목, 복구 항목, submitter가 작업자와 다른 항목, 새 필드를 채운 멈춤을 넣는다.

## 6. 작업 나누기

작업은 11개이고 PR 하나씩이다. 프로필은 모두 `implementer`다. 첫 작업은 v2의 `forward-cmds`와 `docs-multi`를 기다린다(요청). 그래서 이 플랜의 모든 작업이 그 둘 뒤에 시작한다.

| # | 작업 ID | 맡는 것 | 기다리는 작업 |
|---|---|---|---|
| 1 | `v3-core` | 5.3의 설정 키와 검사, `respawn` 기본값 `[]`, 5.4의 타입·필드·사건, 상태 JSON 채우기, fixture, README의 설정 키 | forward-cmds, docs-multi |
| 2 | `notify` | submitter 기록(허브로 넘기기 포함), `src/notify.rs`, merged·bounced·human·halted 알림, revert 알림, `notified` 사건 | v3-core |
| 3 | `events` | `chute events` | v3-core |
| 4 | `env-check` | `env_check` 실행과 기록, 보고서의 "검사 환경" 절, preflight 기록 | v3-core |
| 5 | `preflight-slots` | `max_parallel` 대기, 캐시 씨앗, `CHUTE_CACHE_DIR` | v3-core |
| 6 | `mod-v3` | 현황판: 우선, "#N 뒤", 복구, submitter, 멈춤의 새 필드, 새 경고 | v3-core |
| 7 | `submit-gates` | `require_head`, `--priority`, `--after`와 `waiting` 알림, `submitted`·`dropped` 사건 | notify |
| 8 | `flaky` | 장부(`chute flaky`), probe, 만성 경고와 `flaky` 알림 | notify |
| 9 | `repair` | `submit --repair`, 멈춘 큐의 복구 batch, `resumed`, 멈춤 필드 채우기, 기준 재검증 | submit-gates |
| 10 | `gc` | `src/gc.rs`, `chute gc`, 데몬·preflight·deliver의 정리, 슬롯 상한 정리 | preflight-slots |
| 11 | `docs-v3` | 설명서의 "오케스트레이터 아래에서 일할 때"와 새 명령, `chute --skill`의 `require_head` 줄, README, 첫 가동 점검 | events, env-check, mod-v3, flaky, repair, gc |

```
forward-cmds ┐
docs-multi ──┴─ v3-core ─┬─ notify ─┬─ submit-gates ── repair ─┐
                         │          └─ flaky ──────────────────┤
                         ├─ events ────────────────────────────┤
                         ├─ env-check ─────────────────────────┼─ docs-v3
                         ├─ preflight-slots ── gc ─────────────┤
                         └─ mod-v3 ────────────────────────────┘
```

**순차로 둔 이유**

- 2~6이 `v3-core`를 기다린다. 모두 그 설정 키, 큐 항목 필드, 사건 종류, fixture를 쓴다. 설정 키를 한 작업이 모두 넣는 것은 `tests/docs.rs`가 초안의 모든 키를 README에서 찾기 때문이다. 키를 작업마다 넣으면 `config.rs`의 초안과 README를 모두가 고친다.
- `submit-gates`와 `flaky`가 `notify`를 기다린다. 둘 다 `notify`의 보내는 함수를 쓴다(`waiting`, `flaky` 알림). `submit-gates`는 `notify`가 submitter를 넣은 `src/cmd/submit.rs`와 `queue::submit`을 다시 고친다.
- `repair`가 `submit-gates`를 기다린다. 같은 `src/cmd/submit.rs`의 인자와 `src/queue.rs`의 batch 고르기(`take_batch`)를 다시 짠다. `notify`의 `halted`·`resumed` 알림도 쓴다(`submit-gates`를 거쳐 이미 병합돼 있다).
- `gc`가 `preflight-slots`를 기다린다. `max_parallel`을 넘는 슬롯을 지우려면 그 작업이 만든 슬롯 잠금과 번호 규칙을 써야 하고, 둘 다 `src/gitops.rs`의 슬롯 코드를 고친다.
- `docs-v3`가 나머지를 모두 기다린다. 설명서는 실제 명령의 동작과 메시지를 적는다. README와 설명서를 고치는 작업은 요청대로 v2의 `docs-multi` 뒤에 있다(`v3-core`를 거쳐).

**병렬로 둔 이유와 겹치는 곳**

- `notify`, `events`, `env-check`, `preflight-slots`, `mod-v3`는 맡는 파일이 다르다. `notify`는 `src/notify.rs`·`src/cmd/submit.rs`·`src/cmd/revert.rs`, `events`는 `src/cmd/events.rs`, `env-check`는 새 환경 모듈·`src/feedback.rs`의 보고서, `preflight-slots`는 `src/gitops.rs`, `mod-v3`는 `mod/chute/`다.
- `src/engine.rs`는 `notify`(알림 부르기), `env-check`(batch마다 한 번 돌리기), `flaky`(batch 뒤 실험), `repair`(멈춘 큐의 갈래, 기준 재검증)가 고친다. `notify`와 `env-check`, `flaky`와 `repair`는 동시에 돌 수 있어 지시서에 서로 다른 함수만 최소로 고치라고 적었다. 같은 함수를 다시 짜는 `repair`와 `notify`는 순차다.
- `src/verify.rs`는 `env-check`(필요하면 명령 하나를 돌리는 함수), `preflight-slots`(`CHUTE_CACHE_DIR` 한 줄), `flaky`(probe 실행)가 몇 줄씩 더한다. `src/cmd/preflight.rs`는 `env-check`(기록), `preflight-slots`(기다림 메시지), `gc`(정리를 옮김)가 고친다. 모두 최소 변경으로 하라고 적었다.
- `src/cli.rs`와 `src/cmd/mod.rs`에는 `events`, `flaky`, `gc`가 명령 한 줄씩을 더한다. `src/model.rs`는 `v3-core`가 필드를 모두 넣고, 뒤 작업은 고쳐야 하면 최소로 더한다.
- `mod-v3`는 fixture만 공유한다. fixture는 `v3-core`가 만든다.

## 7. 범위 밖

- orch 스킬 자체를 고치는 일(`land`, `gate`를 chute로 바꾸기, 알림 받기, `events` 읽기). 별도 작업이다. 이 플랜은 그 작업이 쓸 계약(5.1, 5.2, 4.3)만 정한다.
- 실제 GitHub과 실제 머신으로 하는 첫 가동. 사용자 승인이 필요한 별도 단계다. 점검 목록은 9절에 있다.
- 샌드박스의 구현(4.14). 설계만 남긴다.
- 흔들림의 범인 찾기와 머지 보류(orch의 `flaky-made`, `hold`). 4.10.
- respawn이 만든 worktree의 자동 삭제(4.12).
- chute 저장소 자신의 `chute.toml`에 `require_head = true`를 켜는 일. `chute.toml`을 고치는 PR은 사람 승인을 받으므로 첫 가동 때 사용자가 정한다(8절의 4번).
- 알림을 다시 보내는 장치. 놓친 알림은 `chute events`로 따라잡는다(4.2).

## 8. 사용자가 정해야 하는 결정

작업은 아래의 기본값으로 진행할 수 있다. 바꾸면 영향을 받는 작업을 함께 적었다.

| # | 물을 것 | 기본값 | 바꾸면 |
|---|---|---|---|
| 1 | preflight 동시 실행 상한 `max_parallel`의 기본값 | 2(4.11) | `v3-core`의 기본값 한 줄. 4 같은 큰 값은 CPU가 많은 PC에 맞지만 batch 검증과 겹쳐 느려질 수 있다 |
| 2 | 보관 기간 기본값 | 로그 7일, 보고서 30일(4.12) | `v3-core`의 기본값 |
| 3 | chute가 push한 기준을 다시 검증하는 문턱 | 같은 기준에서 다른 PR 2개가 연달아 단독 실패(4.4) | `repair`의 상수. 설정으로 빼려면 `v3-core`에 키가 하나 는다 |
| 4 | chute 저장소의 `chute.toml`에 `require_head = true`를 켤지 | 켜지 않는다. 첫 가동 때 정한다(7절) | 켜면 그 PR은 사람 승인을 받는다 |
| 5 | 샌드박스를 이번에 설계만 할지 | 설계만(4.14) | 구현하려면 `[sandbox] wrap` 작업 하나를 `v3-core` 뒤에 더한다. `src/verify.rs`를 고치므로 `env-check`, `preflight-slots`, `flaky`와 최소 변경으로 겹친다 |
| 6 | 작업자가 스스로 submit했을 때도 머지 성공을 그 작업자에게 보낼지 | 보낸다. 정책대로 submit한 터미널이 결과를 받는다(4.2) | 보내지 않으려면 `notify`에서 submitter가 작업자와 같을 때 `merged`를 기록만 한다 |

## 9. 첫 가동 점검 목록

실제 GitHub과 실제 머신에서만 확인되는 것이다. 사용자 승인 뒤 별도 단계로 한다. `docs-v3`가 README에도 옮긴다.

1. 오케스트레이터 터미널에서 `chute submit <pr> --head <HEAD> --agent <작업자>`를 실행하고, 머지 뒤 오케스트레이터 터미널에 `chute 결과 merged …` 한 줄이 들어오는지 본다. GitHub이 PR을 Merged로 보는지도 본다(v1의 가정).
2. 다른 PC의 오케스트레이터가 submit했을 때 허브가 `hc send <머신>/<터미널>`로 알림을 보내는지 본다.
3. `require_head = true`인 저장소에서 `--head` 없는 submit이 거절되고, 허브가 아닌 PC에서 넘긴 submit도 거절되는지 본다.
4. 일부러 깨뜨린 main에서 큐가 멈추고 `halted` 알림에 단계·로그·`pushed_by_chute`가 맞게 들어오는지, `submit --repair`로 넣은 복구 PR이 혼자 검증돼 머지되고 `resumed`가 오는지 본다.
5. 작업자 터미널을 닫은 뒤 반송이 나면 새 에이전트가 뜨지 않고 사람 차례가 되며 `human reason=unreachable`이 오는지 본다.
6. 두 PC의 `env_check` 결과가 보고서에 나란히 보이는지 본다.
7. preflight를 `max_parallel`보다 많이 동시에 띄워 기다림 메시지를 보고, 새 슬롯이 `target/`을 씨앗으로 받아 빌드 시간이 줄었는지 잰다.
8. `chute gc --dry-run`이 실제 상태 폴더에서 지울 것과 크기를 맞게 보이고, 멈춤 로그를 지우지 않는지 본다.
9. 허브가 아닌 PC에서 `chute events --since 1h --json`이 허브의 사건을 돌려주는지, 알림을 놓친 경우가 `notified`로 보이는지 본다.
10. 실제 시험 묶음에서 probe가 다음 batch를 얼마나 늦추는지 잰다.

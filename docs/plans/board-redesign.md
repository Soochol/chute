# 현황판 UI 개편 플랜: 저장소 머리와 네 칸 숫자, 빈 구역 한 줄, 배치 단계 막대, 줄 격자, 면 카드, 반송 줄

> 작성일 2026-11-18. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 "uiux 더 세련되게 변경하는거 claude artifact로 제안해줘"라고 했다. 대상은 chute 현황판이다. 오케스트레이터가 개편안 여섯 가지를 목업과 함께 artifact(https://claude.ai/artifact/FMj58QfxHBTp1Aoikb6TnY)로 보였고, 사용자는 그것을 보고 "모두 넣어줘"라고 했다.

지금 현황판(main `92de770` 기준, `mod/chute/main.js` 1100줄, `board.js` 863줄)은 필요한 정보를 다 보이지만 화면에서 눈이 갈 곳이 고르지 않다.

- 저장소 머리 줄(`repoHead`)은 이름과 상태 한 줄뿐이다. 배치에 몇 개가 있고 몇 개가 기다리는지는 아래 구역을 내려가며 세야 한다.
- 비어 있는 Needs you, Current batch, Queued, Bounced 머리가 늘 흐린 줄(`QUEUED 0`)로 자리를 차지한다(board-times 결정 3).
- 배치는 4px 진행 막대 하나와 `Verifying` 글자로만 보인다. 지금 어느 단계인지 한눈에 보이지 않는다.
- 줄마다 번호, PR, 제목, 표시, 아이콘의 배치가 조금씩 다르다. 표시(`Urgent`, `after #86`, `chute.toml`)는 맨글자 색이라 제목과 섞이고, 제목 줄을 밀어 잘리게 한다. `log` 글자와 `↗`의 크기가 다르다.
- 경고와 사람 차례 카드는 색 테두리다. 여러 개가 쌓이면 시끄럽고, 사람 차례 카드의 버튼은 카드 중간에 있다.
- 반송 줄은 이유가 제목 오른쪽 끝에 붙어 좁은 패널에서 제목을 밀고, 고칠 작업자가 살아 있는지 보이지 않는다.

## 2. 확정된 결정

요청에 적힌 것이다.

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 머리 | 저장소 이름과 데몬 상태 점을 첫 줄에, 그 아래에 배치·대기·반송·오늘 머지 네 칸 숫자. 0인 칸은 흐리게. 데몬 점은 orch와 같은 기준: 큐가 비면 쉬는 중(회색), 큐가 있는데 꺼졌을 때만 빨강, 돌면 초록 |
| 2 | 빈 구역 | 빈 Needs you/Current batch/Queued/Bounced 머리를 늘 보이지 않고 맨 아래 한 줄로 모은다 |
| 3 | 배치 | Building → Verifying → Bisecting → Pushing 네 칸 단계 막대에 현재 단계만 채운다. 경과 시간 옆에 평소 시간. 배치 묶음은 surface 면으로 뗀다 |
| 4 | 줄 격자 | 대기·배치·반송 줄이 순번 · PR · 제목 · 오른쪽 아이콘의 같은 4칸 격자를 쓴다. 표시는 옅은 바탕 칩으로 제목 아래 한 줄. PR 번호와 시간은 고정폭 숫자. `log`와 `↗`는 같은 크기 아이콘 버튼 |
| 5 | 카드 | 경고·사람 차례 카드는 색 테두리 대신 상태색을 옅게 깐 면과 앞머리 기호. 할 일 버튼은 카드 안 마지막 줄 |
| 6 | 반송 줄 | 이유 칩이 제목 아래 첫 자리. 작업자 터미널 생존 점 |
| — | 색과 폭 | 새 색을 만들지 않고 `theme()` 토큰만 쓴다. 옅은 면은 그 토큰에 투명도를 준 것. 패널 너비 360px 그대로 |
| — | `board.js` | `interpret`와 섹션 데이터 모양은 가능하면 바꾸지 않는다. 바꾸면 범위에 적고 `board.test.js`의 기존 시험을 지우거나 약하게 하지 않는다 |
| — | 화면 확인 | 실제 Herdr에서 밝은·어두운 테마를 모두 확인하는 방법을 성공 조건에 넣는다. 못 하면 그 한계를 적는다 |
| — | 저장소 규칙 | `AGENTS.md`를 따르고 chute로 병합한다 |

결정 2는 board-times 결정 3(빈 구역 머리를 흐린 한 줄로 남긴다, 2026-11-17)을 대신한다. 사용자가 개편안의 2번을 보고 "모두 넣어줘"라고 했기 때문이다.

## 3. 스스로 정한 결정과 이유

### 3.1 공통

| 결정 | 이유 |
|---|---|
| 화면 문구는 영어로 쓴다. 제안 목업의 한국어 문구(`배치`, `대기`, `없음: …`)는 아래 영어로 옮긴다 | `AGENTS.md`: `mod/chute/`의 화면 문구는 영어(2026-11-17 사용자 결정). 목업은 생김새의 제안이고 문구 규칙을 바꾸지 않는다 |
| 옅은 면과 칩 바탕은 `board.js`에 새로 내보내는 `tint(color, alpha)`로 만든다. `#rgb`, `#rrggbb`, `#rrggbbaa` 토큰 값을 받아 `#rrggbbaa`를 돌려주고(이미 있는 알파에는 곱한다), 그 밖의 값에는 `null`을 돌려준다. `main.js`는 `null`이면 `t.surface`를 쓴다 | gpui-kit의 `Color`는 `#rgb`·`#rrggbb`·`#rrggbbaa`만 받는다. `opacity()`는 안의 글자까지 흐리게 해서 면에 쓸 수 없다. 순수 함수라 `board.test.js`로 시험할 수 있고, 색 리터럴 없이 토큰에서만 색을 만든다 |
| 투명도 값은 `main.js` 맨 위 상수 두세 개(면, 칩)로 둔다. 밝은·어두운 테마 사진을 보고 0.08~0.18 사이에서 정한다 | 한 곳에서 고치게 한다. 값은 테마마다 달리 보이므로 사진으로 정한다 |
| 고정폭 숫자는 `settings().font_family`(터미널 글꼴)로 그린다. PR 번호, 시간, 네 칸 숫자에 쓴다 | gpui-kit에 tabular-nums 같은 글꼴 기능 설정이 없다. 파일 보기(`loadFile`)가 이미 같은 글꼴을 쓴다 |
| 새 파일(SVG 아이콘 등)을 mod에 더하지 않는다. 아이콘 버튼은 글자 기호를 같은 크기 정사각 칸(20×20, 가운데 정렬)에 넣는다. 로그·보고서 `≡`, PR 열기 `↗`. tooltip은 지금 문구를 지킨다 | `chute install`(`src/cmd/install.rs`)은 `main.js`, `board.js`, `mod.json` 세 파일만 넣는다. 파일을 더하면 본체 설치 코드까지 고쳐야 한다 |
| 작업 둘을 순차로 둔다(5절) | 여섯 가지가 모두 `main.js`의 같은 그리기 함수들을 고친다. 2번 작업은 1번이 만든 `tint`와 고정폭 도우미를 쓰고, 둘 다 `batchView`를 고친다 |

### 3.2 머리와 네 칸 (작업 `board-head`)

| 결정 | 이유 |
|---|---|
| 패널 머리(`headerView`, `summaryView`)는 지금처럼 `chute`, `N repos · N running · N issues`, 새로 고친 시각, 버튼을 둔다. 시각만 고정폭으로 그린다. 저장소 이름·데몬 점·네 칸은 저장소마다의 머리(`repoHead`)에 둔다 | 현황판은 `chute status --json --all`로 여러 저장소를 한 패널에 보인다. 목업은 저장소 하나만 그렸다. 패널 머리의 요약은 탭 막대 버튼(`chute.status`)과 같은 숫자라 지킨다 |
| `repoHead` 첫 줄: 접기 표시 · 데몬 점 · 저장소 이름 · 허브 칩 · (빈칸) · 상태 한 줄. 지금의 상태 점은 데몬 점으로 바뀌고, 배치나 머지 뒤 검사가 돌 때 지금처럼 깜빡인다(`repo.active`). 상태 한 줄의 글자 색은 지금 규칙(`repoHead`의 `text_color`)을 지킨다 | 요청은 "이름과 데몬 상태 점"이다. 점이 둘이면 시끄럽다. 상태 한 줄은 접힌 저장소의 한 줄 요약(board-polish 결정 1)이라 남긴다 |
| 데몬 점의 툴팁: `Daemon running`, `Daemon idle · queue empty`, `Daemon off · N PRs in queue`, `Daemon state unknown` | 점 색만으로 뜻을 전하지 않는다 |
| `board.js`의 저장소 보기에 `daemon` 필드를 더한다: `{ state, tone, text }`. `state`는 `running`(`daemon.running === true`, tone `ok`), `off`(`running === false`이고 `queue`에 항목이 하나라도 있음, tone `error`), `idle`(`running === false`이고 큐가 빔, tone `idle`), `unknown`(daemon 기록이 없거나 `running`이 불리언이 아님, 또는 허브에 닿지 않음, tone `idle`). 큐 항목은 상태와 상관없이 모두 센다 | orch(`orch.py`의 `chute-daemon-down`)가 `len(status["queue"])`가 0보다 크고 데몬이 안 돌 때만 문제로 본다. 같은 기준이다. `idle` tone은 `toneToken`에서 `dim`이 된다(회색) |
| 경고 카드 `Daemon is off`, 상태 한 줄 `Daemon is off`, 머리 요약의 issues 수, 저절로 펼치기 키 `daemon:off`는 지금 그대로 둔다(큐가 비어도 빨강) | 기존 시험(`board.test.js`의 overview issues 시험, `problem keys are the notice keys plus the stopped daemon`, 무작위 시험의 경고 목록)이 큐가 빈 저장소의 꺼진 데몬을 문제로 본다. 이것을 바꾸면 기존 시험을 고쳐야 한다. 6절 결정 1로 사용자에게 묻는다 |
| 네 칸은 `board.js`가 `cells` 필드로 준다: `[{ key: "batch", label: "Batch", value }, { key: "queued", label: "Queued", value }, { key: "bounced", label: "Bounced", value }, { key: "merged", label: "Merged today", value }]`. `batch`는 배치의 PR 수(배치가 없으면 0), `queued`와 `bounced`는 그 구역의 항목 수, `merged`는 `today.merged`(정수가 아니거나 `today`가 없으면 `null`). 허브에 닿지 않는 저장소는 `cells`가 `null`이다 | "오늘 머지"는 status의 `today.merged`에 이미 있다. 지금 `todayOf`는 이 값을 문자열로만 넘겨서 숫자가 따로 필요하다. 필드를 더하는 것이고 있는 필드는 바꾸지 않는다 |
| 네 칸은 같은 너비로 나란히 둔다. 숫자(고정폭, 15px 안팎, 굵게) 위, 이름(11px, `t.dim`) 아래. 값이 0이거나 `null`(`—`로 그림)인 칸은 숫자도 `t.dim`으로 그린다. 저장소가 펼쳐졌을 때만 보인다 | 요청의 "0인 칸은 흐리게"다. 접은 저장소는 한 줄로 줄이는 것이 접기의 뜻이라 칸을 숨긴다 |
| `TODAY` 줄(`todayView`)은 지금대로 둔다 | 오늘 반송 수, 중앙값, 펼친 세부를 보인다. 네 칸의 `Merged today`와 겹치는 것은 머지 수 하나뿐이다 |

### 3.3 빈 구역 한 줄 (작업 `board-head`)

| 결정 | 이유 |
|---|---|
| `board.js`의 `sections`는 지금처럼 빈 네 구역을 담는다(`ALWAYS_SHOWN` 유지, 위 주석만 고친다). `main.js`가 항목이 없는 구역의 머리를 그리지 않고, 저장소 맨 아래(TODAY 줄 다음)에 `Empty: Needs you · Bounced`처럼 한 줄을 흐리게(11px, `t.dim`) 그린다. 구역 순서는 `sections` 순서다. 빈 구역이 없으면 줄도 없다. `emptyHead`는 지운다 | 데이터 모양을 바꾸지 않아 기존 시험(`an empty queue keeps four section heads and no alerts or unknown` 등)이 그대로 통과한다. 고치는 곳이 그리기 하나다 |

### 3.4 배치 (작업 `board-head`)

| 결정 | 이유 |
|---|---|
| `board.js`의 배치 항목에 `stage`(0~3 또는 `null`)를 더한다. status의 `batch.phase`가 `building`·`verifying`·`bisecting`·`pushing`이면 그 순번, 없거나 다른 값이면 `null`. 네 칸 이름은 `PHASE` 표의 값(`Building`, `Verifying`, `Bisecting`, `Pushing`)을 그대로 쓴다 | 지금 `phase`는 이름표로 바뀐 글자라 순번을 알 수 없다. 필드를 더하는 것이다 |
| 단계 막대: 같은 너비 네 칸(높이 4px, 둥근 2px), 칸 아래에 단계 이름(11px). 현재 칸만 채운다. 채우는 색은 `t.working`, 평소보다 오래 걸리면(`longer`) `t.ansi3`, 슬롯을 기다리면(`waitingMachine`) `t.muted`. 나머지 칸은 `t.border`, 이름은 현재만 `t.foreground`이고 나머지는 `t.dim`. `stage`가 `null`이면 아무 칸도 채우지 않는다 | 요청은 "현재 단계만 채움"이다. Bisecting은 실패했을 때만 지나므로 앞 칸을 지나간 것으로 칠하지 않는다 |
| 4px 진행 막대는 지운다. `board.js`의 `progress` 필드는 그대로 둔다(그리는 곳이 없어진다) | 요청이 그 막대를 단계 막대로 바꾼다. 필드를 지우면 기존 시험을 고쳐야 한다 |
| 배치 머리 줄: spinner · `CURRENT BATCH` · (빈칸) · 경과 시간(고정폭) · `/ usually 5m`(11px, `t.dim`). `usual`이 없으면 경과만. 평소보다 오래 걸리면 경과 시간을 `t.ansi3`로 그린다. 아래 단계 글자 줄(`test 3/3`, `Waiting for a slot`)과 `Slower than usual`, `Round`·`Reused` 사실 줄은 지금 뜻을 지킨다 | 요청의 "경과 시간 옆에 평소 시간"이다 |
| 배치 묶음 전체(머리 줄, PR 줄, 단계 막대, 글자 줄)를 `t.surface` 면(둥근 8px, 안쪽 여백 10px)에 둔다 | 요청이다 |

### 3.5 줄 격자 (작업 `board-rows`)

| 결정 | 이유 |
|---|---|
| 대기, 배치 안 PR, 반송, 최근 머지, 알 수 없는 상태 줄이 같은 도우미 하나로 그린다. 칸: 순번(너비 18, 오른쪽 정렬, 순번이 없는 줄도 빈 칸으로 자리를 지킨다) · PR 번호(고정폭, `t.accent`, 굵게, 같은 최소 너비) · 제목(남는 폭, 줄바꿈 허용) · 오른쪽 칸(시간 + 아이콘 버튼 자리 둘). 아이콘 자리는 `≡`, `↗` 순서로 고정이고 없는 아이콘은 빈 자리로 둔다 | 요청의 "같은 4칸 격자"다. 최근 머지 줄은 요청에 없지만 같은 패널에서 번호와 아이콘 세로줄이 어긋나지 않게 함께 맞춘다. 빈 자리를 지켜야 줄마다 `↗`가 같은 세로줄에 선다 |
| 제목은 잘라 말줄임하지 않고 줄바꿈한다 | 표시가 제목 아래로 내려가면 제목이 밀릴 이유가 없다. 제안의 "제목이 잘리지 않고"다 |
| 제목 아래 한 줄(제목 칸에 맞춰 들여 씀): 칩들. 대기 줄은 표시(`Urgent`, `Repair`, `after #N`, `chute.toml`), CI 대기(`Waiting on CI`, 지금 `label` 그대로), 그 밖의 대기 이유(`note`) 순서. 칩은 `tint(톤 색, 칩 알파)` 바탕에 톤 색 글자, 테두리 없음, 11px. 그 아래 submitter 줄은 지금 문구 그대로 같은 들여쓰기 | 요청이다. 칩 문구는 `board.js`가 주는 그대로 써서 데이터를 바꾸지 않는다 |
| 지금 `chip()`(색 테두리)을 쓰는 다른 곳(허브 칩, 에이전트 host 칩, TODAY의 `chronic`, 알 수 없는 상태 칩)도 같은 옅은 바탕 칩으로 바꾼다 | 한 패널에 두 칩 모양을 두지 않는다 |
| 아이콘 버튼(`iconLink`가 만드는 것)은 20×20 정사각, 글자 12px 가운데, 둥근 4px, hover 때 `t.hover` 바탕. 클릭이 줄로 번지지 않는 지금 동작(`stop_propagation`)을 지킨다 | 요청의 "같은 크기 아이콘 버튼"이다 |

### 3.6 카드 (작업 `board-rows`)

| 결정 | 이유 |
|---|---|
| 경고 카드(`alertCard`): 테두리를 지우고 `tint(톤 색, 면 알파)` 면, 둥근 8px. 제목 앞에 기호: tone `error`는 `✕`, `warn`은 `△`. 기호와 제목은 톤 색. 파일 버튼(`Log ›`, `Report ›`) 줄은 지금처럼 마지막 | 요청이다. 기호가 있어 색만으로 구분하지 않는다 |
| 사람 차례 카드(`humanCard`): 테두리를 지우고 `tint(t.blocked, 면 알파)` 면. 질문 앞에 `?` 기호(`t.blocked`). 줄 순서는 질문, PR 줄, 표시 칩, 이유, 승인 불가, 실패 요약, 변경 요약, 겹침, 사실, 사람, 보고서, 실행 오류, 그리고 맨 끝에 버튼 줄(`Approve`, `Drop`, 오른쪽 `View diff ↗`). 버튼의 생김새와 동작은 지금대로 | 요청의 "할 일 버튼은 카드 안 마지막 줄"이다. 실행 오류는 버튼 바로 위에 두어 무엇이 실패했는지 버튼 곁에서 읽힌다 |
| 카드 안 PR 번호도 고정폭으로 그린다 | 결정 4의 고정폭 숫자를 패널 전체에 맞춘다 |

### 3.7 반송 줄 (작업 `board-rows`)

| 결정 | 이유 |
|---|---|
| 첫 줄: 순번 칸(빈 칸) · PR · 제목 · 오른쪽 칸(반송 시각 `ago`, `≡`, `↗`). 둘째 줄 첫 칩: 이유(`headline`, 예 `Verification failed · Combined`, `tint(t.ansi1)` 칩). 그 뒤 같은 줄이나 다음 줄에 작업자: 생존 점(살아 있으면 `t.ansi2`, 아니면 `t.dim`) · 이름(`card.agent.label`, 죽었으면 `· exited`) · `Retries 3/3`. 작업자가 없으면 `No agent`(`t.dim`) | 요청이다. 생존 점은 사람 차례 카드의 `agentChip`과 같은 색 규칙이다 |
| 줄 전체를 누르면 보고서가 열리는 지금 동작을 지킨다 | 지금 쓰는 동작이다 |

## 4. 화면 확인 방법

`herdr-app check mod/chute`는 mod를 한 번 그려 오류만 본다. 생김새는 보지 못한다. 그래서 두 작업 모두 사용자 창과 떨어진 Herdr 창에서 사진을 찍는다.

- 임시 폴더 하나에 `HERDR_APP_HOME`, `HC_SESSION`(고유한 이름), `XDG_CONFIG_HOME`·`XDG_STATE_HOME`·`XDG_DATA_HOME`·`XDG_RUNTIME_DIR`를 모두 둔다. 이 PC에 `/usr/bin/xvfb-run`이 있으니 `xvfb-run -a`로 창을 띄워 사용자의 화면(Wayland `:0`)에 나타나지 않게 한다.
- 그 `HERDR_APP_HOME/mods/chute/`에 작업 브랜치의 `main.js`, `board.js`, `mod.json`을 복사한다. `bin` prop은 임시 폴더의 가짜 `chute` 스크립트를 가리킨다. 가짜는 `status --json --all`에 장면 JSON 배열을 출력한다.
- 장면 셋: (a) `tests/fixtures/status-full.json` 그대로, (b) 배치 하나가 돌고 대기 3, 반송 1인 조용한 저장소(제안 목업의 데이터), (c) 큐가 빈 저장소(빈 구역 한 줄, 데몬 점 회색).
- 모드 패널을 360px 폭으로 보이게 하고(레이아웃 block의 `size`나 `pane.split`), `app.theme`로 `paper`(밝음)와 `graphite`(어두움)를 바꿔 장면마다 `herdr-app shot`으로 찍는다. 찍은 사진은 직접 열어 본다.
- 모든 `herdr-app`·`hc` 명령에 위 환경 변수를 붙인다. 사용자의 창, 테마, `~/.config/herdr-app/`, 사용자 hc 코어에는 아무 명령도 보내지 않는다. 끝나면 띄운 창 프로세스를 끝내고, 자기 `HC_SESSION`의 코어만 멈추고, 임시 폴더를 지운다(사진은 보고서 폴더로 옮긴다).
- 이 방법이 안 되면(창이 Xvfb에서 그려지지 않음, 코어가 격리되지 않음 등) 멈추고 무엇이 막혔는지 보고서에 적는다. 그 경우 사용자 창에 사본을 설치하거나 사용자 테마를 바꾸는 우회는 하지 않는다(6절 결정 2).

이전 현황판 작업 넷(board-en, board-polish, board-times, needs-card)은 사진 없이 끝났다. 이 방법은 이 PC에서 아직 시험되지 않았다.

## 5. 작업

| 작업 | 맡는 것 | 순서 | 방식 | 방식의 이유 |
|---|---|---|---|---|
| `board-head` | 3.1의 `tint`·고정폭 도우미, 3.2 머리와 네 칸, 3.3 빈 구역 한 줄, 3.4 배치 | 먼저 | `basic` | 화면 조정과 `board.js`의 읽기 전용 필드 추가다. 상태 저장, 머지·검증 판정, 잠금, 프로세스, orch 코드를 건드리지 않는다 |
| `board-rows` | 3.5 줄 격자, 3.6 카드, 3.7 반송 줄 | `board-head` 뒤 | `basic` | 화면 조정뿐이다. `board.js`는 바꾸지 않는다 |

순차로 둔 이유: 두 작업 모두 `main.js`의 `batchView`를 다시 짠다(`board-head`는 머리·막대·면, `board-rows`는 그 안의 PR 줄). `board-rows`는 `board-head`가 만든 `tint`와 고정폭 도우미를 쓴다.

두 작업의 공통 범위:

- 고치는 파일: `mod/chute/main.js`, `mod/chute/board.js`(`board-head`만, 필드 추가), `mod/chute/board.test.js`(`board-head`만, 시험 추가).
- 고치지 않는 것: chute 본체(`src/`), `tests/fixtures/`, `mod/chute/mod.json`, `mod/chute-tabs/`, `chute.toml`, 다른 플랜 문서, 사용자의 `~/.config/herdr-app/` 아래 파일.
- `board.test.js`의 기존 줄은 지우거나 바꾸지 않는다. 시험은 더하기만 한다.

## 6. 사용자가 정해야 하는 결정

1. **큐가 빈데 데몬이 꺼진 저장소의 경고.** 이 플랜에서 데몬 점은 회색(쉬는 중)이 되지만, 경고 카드 `Daemon is off`, 빨간 상태 한 줄, 머리 요약의 issues 1, 접은 저장소 저절로 펼치기는 지금처럼 남는다. 그래서 그 저장소는 회색 점과 빨간 경고가 함께 보인다. orch처럼 이것들도 큐가 있을 때만 띄우려면 기존 시험 셋(overview issues 시험의 `daemonOff`, `problem keys are the notice keys plus the stopped daemon`, 무작위 시험의 경고 목록 기대값)의 기대를 바꿔야 한다. 요청이 기존 시험을 약하게 하지 말라고 해서 이 플랜은 하지 않았다. 원하면 작은 작업 하나로 더한다.
2. **화면 확인이 막혔을 때.** 4절의 격리 창이 안 되면 사진 없이 끝난다. 사용자 Herdr에 `place: "none"` 사본을 잠깐 설치하고 사용자 창의 테마를 바꿔 찍는 것을 허용할지 정한다. 허용하지 않으면(이 플랜의 기본) 머지 뒤 `chute install`로 설치한 화면을 사용자가 테마 고르기로 두 테마를 보고 확인한다.
3. **mod 시험이 chute 검증에 없다.** `chute.toml`의 검증은 `cargo fmt`, `clippy`, `cargo test`뿐이라 `node --test mod/chute/`는 chute가 병합 전에 돌리지 않는다. 작업자가 성공 조건으로 돌리지만, 병합 전 합친 결과에서도 돌리려면 `chute.toml`에 단계를 더하는 별도 결정이 필요하다. 이 플랜은 `chute.toml`을 고치지 않는다.

## 7. 범위 밖

- chute 본체(`src/`)와 status JSON. 필요한 값(`daemon.running`, `queue`, `batch.phase`, `today.merged`)은 이미 있다.
- 탭 막대 버튼(`mod/chute-tabs/`)과 `chute.status` 공유 값.
- 파일 보기 창(`fileView`)과 오버레이 머리 버튼.
- 패널 너비, 폴링 간격, 알림과 배지.
- 6절 결정 1의 경고 기준 변경.

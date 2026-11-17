# chute 탭 막대 버튼과 패널 레이아웃 선택 설치 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 Herdr 탭 막대의 `● chute` 버튼과 오른쪽 패널의 chute 현황판을 쓰고 있다. 지금은 이 PC에만 손으로 만든 개인 설정이다. 사용자는 "● chute 버튼 … 개인 설정 아니게 하려면?"이라고 물었고, 오케스트레이터가 낸 두 안 가운데 B(chute가 선택 설치한다)를 골랐다.

이 PC에 손으로 만든 것(2026-11-17 확인, Herdr 0.1.16):

- 탭 막대 `~/.config/herdr-app/mods/tabs-chute/`: 기본 `tabs` mod를 `mod.clone`한 것. 기본 `mods/tabs/main.js`(157줄)와 다른 곳은 여섯 군데다. `herdr` import 뒤의 `CHUTE_TONE`·`chuteDot`·`chuteTip`, `syncPanel`의 `now` 계산과 공간 전환 복원, `panelButton`의 `on` 계산과 클릭, 창 버튼 앞의 `● chute` 버튼이다. 창 버튼 SVG 경로는 `mods/tabs-chute/`로 바뀌어 있다.
- 레이아웃 `~/.config/herdr-app/layouts/main-chute.json`: 기본 `main`을 `layout.clone`하고 id `tabs` 블록의 mod를 `tabs-chute`로, id `panel` 열의 `panel-body` 아래에 `{"id":"chute","type":"block","size":{"weight":1},"min":120,"content":{"mod":"chute"}}`를 더하고 `panel-body`에 `size: {weight: 1}`을 준 것. 나머지는 `main`과 같고 키 순서만 다르다.
- 단축키 `~/.config/herdr-app/keys.json`의 `global`에 `ctrl-shift-g: layout.toggle:panel`, `ctrl-shift-u: layout.toggle:chute`.

Herdr 규칙(`~/.config/herdr-app/AGENTS.md`)에서 이 설계에 걸리는 것:

- Herdr의 기본 레이아웃과 mod는 읽기 전용이다. 고치면 다음 시작에 `<name>-edited`로 밀려나고 Herdr 판으로 돌아간다. 복제본만 쓴다.
- 복제본은 "yours and never updated"다. Herdr가 기본 `tabs`·`main`을 바꿔도 복제본은 따라가지 않는다.
- Herdr는 처음 보는 mod 폴더를 `mod.json`의 `place`대로 지금 레이아웃에 한 번 붙인다(`seen.json`에 이름이 남는다). 탭 막대 복제본은 `place: "none"`이어야 한다.
- `keys.json`은 한 글자라도 고치면 파일 전체가 사용자의 것이 되어, Herdr가 새 기본 단축키를 더 이상 넣어 주지 않는다.
- 레이아웃의 클릭 상태(크기, 보임·숨김)는 `layouts/.state/`에 따로 있고 레이아웃 파일을 다시 써도 남는다.

## 2. 확정된 결정

요청에 적힌 것이다.

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 설치 방식 | `chute install`의 **선택** 옵션. 옵션 없는 `chute install`은 지금처럼 Codex 스킬과 현황판 mod만 쓴다 |
| 2 | 쓰는 것 | chute 이름으로 된 탭 막대 mod와 레이아웃. Herdr 기본 `tabs`·`main`은 고치지 않는다 |
| 3 | 단축키 | 사용자 `keys.json`을 덮어쓰지 않는다 |
| 4 | Herdr 버전 | 복제 원본과 지금 Herdr의 기본 파일이 다르면 경고한다. 갱신 방법은 다시 설치다 |
| 5 | 되돌리기 | `chute uninstall --layout` 또는 같은 뜻의 방법으로 `main`으로 돌아가고 chute가 쓴 파일을 지운다 |
| 6 | 동작 표 | 아래 표대로 움직인다 |
| 7 | 손으로 만든 것 | 이 PC의 `tabs-chute`, `main-chute`는 설치 뒤 오케스트레이터가 정리한다. 이 작업은 사용자 파일을 지우지 않는다 |
| 8 | README | Herdr가 mod용 탭 막대 버튼 API를 주면 이 복제본을 지우고 갈아탄다는 것을 한 줄 적는다 |

동작 표(사용자에게 보인 것). "Files"는 Files·Changes 버튼 하나를 뜻한다.

| 지금 패널 | chute 버튼 | Files 버튼 |
|---|---|---|
| 닫힘 | chute만 열린다 | Files만 열린다 |
| chute와 Files 둘 다 열림 | chute만 닫힌다 | Files만 닫힌다 |
| chute만 열림 | 패널이 닫힌다 | Files가 더해진다(둘 다) |
| Files만 열림 | chute가 더해진다(둘 다) | 패널이 닫힌다 |

## 3. 스스로 정한 결정

| 번호 | 항목 | 결정 | 이유 |
|---|---|---|---|
| A | 이름 | 옵션 `chute install --layout`, 되돌리기 `chute uninstall --layout`. 탭 막대 mod `chute-tabs`, 레이아웃 `chute`(파일 `layouts/chute.json`), 패널 안 블록 id `chute` | 요청의 예를 따른다. 블록 id `chute`는 지금 PC의 단축키 `layout.toggle:chute`와 같다 |
| B | 복제본을 만드는 법 | 저장소에 복제본 전체를 넣지 않는다. 설치할 때 **그 PC의 지금 Herdr 기본 파일**을 읽어 만든다. 레이아웃은 `layouts/main.json`을 JSON으로 읽어 id로 찾은 노드를 고친다. 탭 막대는 `mods/tabs/main.js`에 정확히 한 번 나오는 몇 줄을 바꿔 끼우고, chute가 더하는 코드는 chute 자신의 모듈 `chute.js`에 둔다 | 다시 설치하면 새 Herdr의 기본 파일 위에 다시 만들어지므로 "갱신 방법은 다시 설치"가 그대로 성립한다. 저장소(공개, 라이선스 없음)에 Herdr의 코드를 통째로 싣지 않는다. 바꿔 끼울 줄이 없거나 두 번 나오면(Herdr가 그 부분을 바꿨다) 설치를 거절하고 이유를 알린다. 조용히 틀린 복제본을 쓰지 않는다 |
| C | chute가 더하는 코드 | `mod/chute-tabs/chute.js`. 버튼의 점 색과 tooltip, 그리고 동작 표를 순수 함수로 둔다(지금 패널 상태와 누른 버튼을 받아 실행할 Herdr 명령 목록을 돌려준다). `mod/chute-tabs/chute.test.js`가 node로 동작 표의 여덟 칸을 시험한다 | 동작 표가 시험으로 고정된다. 바꿔 끼우는 줄이 짧아져 Herdr가 바뀌어도 맞출 곳이 적다 |
| D | 지금 복제본과 동작 표의 차이 | 동작 표를 따른다. 지금 `tabs-chute`는 "chute만 열림 → chute 버튼(패널 닫힘) → Files 버튼"에서 chute 블록이 숨겨지지 않아 Files와 chute가 함께 열린다(코드를 따라가 확인한 것이다. 화면으로 보지는 않았다). 표의 "닫힘에서 Files → Files만"과 다르다 | 사용자가 본 것은 표다 |
| E | 공간별 패널 기억 | 기본 `tabs`처럼 공간마다 Files 쪽(`panel-body`의 선택)만 기억한다. chute 블록의 보임은 공간과 상관없다. 기억된 것이 "닫힘"이고 chute가 보이면 `panel-body`만 숨긴다 | 지금 복제본의 `syncPanel` 동작이다. 현황판은 모든 저장소를 보이므로 공간마다 달리 둘 까닭이 없다 |
| F | 창 버튼 SVG | 생성한 `main.js`는 `mods/tabs/win-*.svg`를 그대로 가리킨다. SVG를 복사하지 않는다 | Herdr가 `tabs`와 함께 늘 싣는 파일이다. 쓰는 파일이 줄어든다 |
| G | 버전 기록과 경고 | 생성한 `main.js` 첫 줄 주석에 "chute install --layout이 만들었고 다음 설치에 덮어쓴다"와 원본 `mods/tabs/main.js`의 지문(FNV-1a 64비트, 의존성 없이 구현)을 적는다. `chute install`(옵션이 있든 없든)은 `mods/chute-tabs/`가 있으면 지금 기본 파일로 다시 만들어 본 결과와 설치된 파일을 견준다. 다르면 경고한다. 원본 지문이 다르면 "Herdr가 기본 탭 막대를 바꿨다", 레이아웃이 다르면 "Herdr의 기본 main이 바뀌었거나 chute 레이아웃을 고쳤다"로 나눠 알리고 `chute install --layout`을 다시 실행하라고 한다 | Herdr 고유의 `cloned_from {name, version}`은 쓰지 않는다. chute는 Herdr 버전을 파일만으로 알 수 없고, 레이아웃과 `mod.json`에 Herdr가 모르는 키를 넣으면 거절될 수 있다. JS 주석은 위험이 없다. 다시 만들어 견주는 것은 실행 중인 Herdr 없이 파일만으로 되고 시험할 수 있다 |
| H | 내 것인지 확인 | `layouts/chute.json`은 id `tabs` 블록의 mod가 `chute-tabs`일 때, `mods/chute-tabs/main.js`는 G의 첫 줄 주석이 있을 때만 chute의 것으로 본다. 아닌 파일은 설치가 덮어쓰지 않고 되돌리기가 지우지 않는다. 이유를 출력한다 | Herdr 규칙의 "이름이 겹치면 덮어쓰지 않는다"를 따른다. 사용자가 같은 이름으로 만든 파일을 지우지 않는다 |
| I | chute의 것인 파일의 덮어쓰기 | 설치는 내 것인 파일을 지금 만든 내용으로 덮어쓴다(손으로 고친 것도). `mods/chute/`를 지금 덮어쓰는 것과 같다 | README에 "chute가 만드는 파일이니 고치지 않는다"고 적는다 |
| J | 레이아웃으로 바꾸기 | 설치는 바꾸지 않고 안내한다. `herdr-app do layout.switch '{"name":"chute"}'` 또는 `ctrl-shift-l` | Herdr는 새 레이아웃 파일을 1초 안에 읽는다. 쓰자마자 바꾸면 아직 모르는 이름일 수 있다. 사용자 창을 언제 바꿀지는 사용자가 정한다 |
| K | 단축키 | `keys.json`을 쓰지 않고 안내만 한다. 설치 출력에 `"layout:chute": {"ctrl-shift-g": "layout.toggle:panel", "ctrl-shift-u": "layout.toggle:chute"}` 구역을 보여 준다 | 한 글자라도 고치면 Herdr가 새 기본 단축키를 더 이상 넣어 주지 않는다. `layout:chute` 구역은 chute 레이아웃에서만 들어 다른 레이아웃과 부딪히지 않는다. 두 키는 Herdr 기본 단축키에 없다 |
| L | 되돌리기의 순서 | `chute uninstall --layout`은 먼저 `herdr-app status`로 보이는 레이아웃을 읽고, `chute`이면 `herdr-app do layout.switch '{"name":"main"}'`을 실행한다. 그다음 내 것인 파일을 지운다: `layouts/chute.json`, `mods/chute-tabs/`의 chute 파일과 Herdr가 넣는 `gpui-kit.d.ts`·`herdr.d.ts`·`jsconfig.json`. 폴더가 비면 폴더도 지운다. 다른 파일이 남으면 폴더와 그 파일을 두고 알린다. Herdr가 꺼져 있어 `herdr-app`이 실패하면 파일은 지우고 "다음 시작에 chute 레이아웃이 없으면 ctrl-shift-l로 main을 고른다"고 알린다 | 보이는 레이아웃의 파일을 먼저 지우면 Herdr가 어떻게 할지 확인하지 못했다. `layouts/.state/`, `keys.json`, `seen.json`, 현황판 `mods/chute/`는 건드리지 않는다(Herdr의 것이거나 이 옵션이 쓴 것이 아니다) |
| M | `herdr-app` 실행 | `src/ext.rs`의 `Tool`에 `herdr-app`(환경 변수 `CHUTE_HERDR_APP`)을 더해 `Ext`로 부른다. 시험은 가짜를 쓴다 | 저장소 규칙이 외부 프로그램을 `Ext`로 부르고 가짜로 시험하게 한다. 실제 Herdr 창에 닿는 시험을 만들지 않는다 |
| N | `chute uninstall` | 새 하위 명령. 지금은 `--layout`만 있고 반드시 줘야 한다. 없으면 "지금은 --layout만 되돌린다"며 거절한다 | 요청의 이름을 따른다. 스킬과 현황판 mod의 제거는 범위 밖이라 빈 `chute uninstall`이 무엇을 지울지 정하지 않는다 |
| O | 작업 수 | 작업 하나(PR 하나) | 사용자의 주간 사용량이 90%대라 작업 수를 줄인다. Rust 설치·되돌리기, JS 한 모듈, README가 한 기능이고 서로를 써서 나누면 순차가 된다 |

작업 `board-layout`의 방식은 `poteto`다. 사용자의 Herdr 설정 폴더에 파일을 쓰고 지우며, 외부 프로그램(`herdr-app`)을 새로 부르기 때문이다.

## 4. 범위 밖

- Herdr에 mod용 탭 막대 버튼 API를 요청하는 것(안 A).
- 이 PC의 `tabs-chute`, `main-chute`, `keys.json`의 두 줄을 정리하는 것(오케스트레이터가 설치 뒤 한다).
- `chute install`이 레이아웃을 스스로 바꾸는 것, `keys.json`에 키를 더하는 것.
- 스킬과 현황판 mod(`mods/chute/`)를 지우는 `chute uninstall`.
- 현황판 mod의 화면과 status JSON. 탭 막대는 현황판이 이미 쓰는 shared `"chute.status"`를 읽기만 한다.
- 사용자 창에서 레이아웃을 바꿔 찍는 확인. 작업자는 임시 `HERDR_APP_HOME`에서 만든 복제본을 `herdr-app check`로만 확인한다.

## 5. 사용자가 정해야 하는 결정

- J: 설치가 레이아웃을 바로 바꾸지 않고 안내만 한다. 바로 바꾸기를 바라면 알려 준다.
- K: 단축키는 안내만 한다. 지금 이 PC의 `global` 두 줄은 그대로 두면 chute 레이아웃에서도 그대로 든다.
- D: 지금 쓰는 복제본과 한 곳이 다르게 움직인다(chute만 열린 상태에서 패널을 닫고 Files를 누르면 Files만 열린다). 지금 동작이 맞다면 알려 준다.

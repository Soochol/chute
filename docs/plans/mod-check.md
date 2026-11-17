# chute 검사에 현황판 시험 추가 플랜

> 작성일 2026-11-18. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 미뤄 둔 일 #15와 #16을 보고 "이거 진행 필요?"라고 물었고, 오케스트레이터가 #16은 하고 #15는 선택으로 basic 작업 하나에 묶자고 추천하자 "ㅇㅇ"라고 답했다.

- **#16 (주)**: 2026-11-18 현황판 PR #90과 #91이 batch에서 mod 시험을 한 번도 돌리지 않은 채 병합됐다. 지금 `chute.toml`의 `[[verify]]`는 `fmt`, `clippy`, `test`(`cargo test`)뿐이라 `mod/` 아래의 Node 시험은 어느 검증에서도 돌지 않는다.
- **#15 (덤)**: 배치에 `started_ms`가 없으면 현황판의 CURRENT BATCH 머리줄에 경과 시간 없이 `/ usually 5m 45s`만 나온다.

main `fd59ef7`에서 확인한 사실:

- `mod/` 아래에는 시험이 있는 mod가 둘 있다. `mod/chute/board.test.js`(154개)와 `mod/chute-tabs/chute.test.js`(18개)다. 두 폴더의 `package.json`은 `main`이 시험 파일을 가리켜 `node --test mod/chute/`처럼 폴더를 줘도 돈다.
- 둘 다 지금 통과한다. 이 PC에서 둘을 함께 돌리면 약 8초 걸린다. 거의 전부가 `random malformed status never breaks the board`(시드를 고정한 무작위 시험, 약 7.5초)다. 무작위 시험의 시드는 `20261115`로 고정돼 있어 흔들리지 않는다.
- 최근 chute batch 10개는 39~139초 걸렸다(`chute status --json`의 `recent`). 8초를 더하면 batch가 대략 10~20% 길어진다.
- 검증 명령은 데몬의 환경을 물려받아 슬롯 폴더에서 `sh -c`로 돈다(`src/verify.rs`). 도는 두 데몬 프로세스의 `PATH`에 `/home/dev/.local/bin`(→ nvm의 node v24.13.1)과 `/home/dev/.nvm/versions/node/v24.13.1/bin`이 있다. 이 저장소의 허브는 이 PC(`hpg1a`)이고 batch는 허브에서 돈다. `preflight = "never"`라 작업자 PC의 preflight는 검사를 돌리지 않는다.
- 검증은 PR의 설정이 아니라 기본 브랜치의 `chute.toml`로 돈다(README "신뢰 모드"). 이 플랜의 구현 PR이 더하는 단계는 그 PR이 머지된 뒤 다음 batch부터 돈다. 구현 PR 자신의 batch에서는 돌지 않는다.
- 머리줄의 `/ usually`는 `mod/chute/main.js`가 `batch.usual`이 있으면 붙인다. `batch.usual`은 `mod/chute/board.js`의 `batchOf`가 정하는데, 경과 시간(`elapsedS`)이 없어도 `usualS`만 있으면 채운다. 진행 막대와 `longer`는 이미 둘이 모두 있을 때만 켠다(`timed`).

## 2. 확정된 결정

- `chute.toml`에 mod 시험을 도는 검증 단계를 더한다(#16).
- `/ usually`는 경과 시간이 있을 때만 붙인다. 시험을 하나 더한다(#15).
- 작업은 basic 방식 하나로 한다(요청).

## 3. 스스로 정한 결정과 이유

| 결정 | 이유 |
|---|---|
| 단계 이름은 `mod`, 명령은 `node --test mod/*/*.test.js`, `timeout = 300` | 요청의 `node --test mod/chute/`는 `mod/chute-tabs/`의 시험 18개를 빠뜨린다. 그 시험도 한 번도 batch에서 돈 적이 없다. glob은 `sh`가 펼치므로 나중에 생기는 mod의 시험도 따로 손대지 않아도 돈다. 맞는 파일이 하나도 없으면 `sh`가 글자 그대로 넘기고 node가 실패하므로 조용히 건너뛰지 않는다. 300초는 지금 8초의 서른 배가 넘어 부하에서도 넉넉하다 |
| `failed = '✖ (.+) \(\d[\d.]*ms\)'`를 넣는다 | 반송 보고서에 로그 끝 대신 실패한 시험 이름이 들어간다. node의 spec 출력은 실패를 본문과 끝의 `failing tests:` 목록에 두 번 적지만 chute가 이름의 중복을 없앤다(`src/verify.rs` `failed_tests`). `✖ failing tests:` 줄은 `(…ms)`가 없어 맞지 않는다. 이름에 괄호가 든 시험(`a (b) fails`)도 맞는 것을 손으로 확인했다. `probe`는 넣지 않는다(mod 단계에 흔들림이 없다) |
| 단계 순서는 `fmt`, `mod`, `clippy`, `test` | 단계는 적은 순서대로 돌고 하나라도 실패하면 batch가 실패한다. 8초짜리 단계를 수 분짜리 cargo 단계 앞에 두면 mod를 깨뜨린 PR이 빨리 반송된다 |
| `paths`를 두지 않는다(늘 돈다) | `board.test.js`는 `tests/fixtures/status-full.json`을 읽으므로 mod 밖의 변경도 이 시험을 깰 수 있다. 8초는 아껴도 얻는 것이 작고, `paths`를 잘못 좁히면 이번 일과 같은 구멍이 다시 생긴다 |
| node가 없으면 단계가 실패하게 둔다(건너뛰지 않는다) | 검사를 조용히 건너뛰는 것이 이번에 고치는 문제 그 자체다. node가 없으면 `sh`가 127로 끝나 로그에 `node: not found`가 남고, 기준 검증도 실패하므로 큐가 "기준이 깨졌다"로 서서 바로 드러난다. 지금 batch가 도는 허브의 데몬 `PATH`에는 node가 있다. `env_check`는 더하지 않는다. 실패 로그에 원인이 이미 보인다 |
| `/ usually` 수정은 `mod/chute/board.js`에서 한다 | `main.js`는 Herdr를 import해 node로 시험할 수 없다. `batchOf`의 `usual`을 경과 시간이 있을 때만 채우면 `main.js`의 `.when(Boolean(batch.usual), …)`가 그대로 맞고 `board.test.js`로 시험할 수 있다. `usual`은 `main.js`의 그 자리에서만 쓴다 |
| 검사 목록을 적은 문서를 맞춘다 | `AGENTS.md`의 `## 검사`가 "저장소 루트의 `chute.toml`에도 같은 명령이 있다"고 적는다. `chute.toml` 머리 주석도 "AGENTS.md의 검사와 같다"다. README의 개발 안내 한 줄(`cargo test`로 시험한다)에도 mod 시험을 더한다. 다른 문서에는 이 저장소의 검사 목록이 없다(`docs/testing.md`는 Rust 시험 기준이다) |

## 4. 작업

| 작업 ID | 내용 | 방식 | 방식의 이유 |
|---|---|---|---|
| `mod-check` | `chute.toml`에 `mod` 단계, `AGENTS.md`·README 검사 목록, `board.js`의 `usual` 조건과 시험 하나 | `basic` | 설정, 문서, 화면 문구 조건 한 줄과 시험 하나다. 검증 판정 코드(`src/verify.rs` 등)는 건드리지 않는다 |

작업이 하나라 병렬·순차 관계는 없다. 고칠 파일이 다섯이지만 각각 몇 줄이고 시험은 하나라 profile은 `minor-fix`다.

## 5. 범위 밖

- `random malformed status never breaks the board`를 빠르게 하는 일. 8초는 받아들일 만하다.
- CI(GitHub Actions) 추가. 이 저장소에는 워크플로가 없고 검증은 chute batch가 맡는다.
- `src/` 아래의 Rust 코드. 검증 실행기, 설정 해석은 바꾸지 않는다.
- `mod/chute/main.js`의 머리줄 배치나 색.

## 6. 사용자가 정해야 하는 결정

- 없음. 이 플랜은 검증을 더할 뿐 약하게 하는 것이 없다.
- 알아 둘 것: batch가 8초쯤 길어진다. 그리고 새 단계는 구현 PR이 머지된 다음 batch부터 돈다. 구현 PR의 mod 시험은 작업자가 손으로 돌린 결과로만 확인된다.

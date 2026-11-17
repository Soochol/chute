# chute 시험 작성 기준과 기계 검사 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

기준: `origin/main` 3f37163.

## 1. 사용자가 풀려는 문제

사용자는 2026-11-17에 시험 작성 기준을 vallythevally e2e에 먼저 두고, chute Rust 시험은 그다음 별도 플랜으로 하기로 승인했다. vallythevally 쪽은 끝났다(PR #456~#461: `docs/e2e-tests.md`, `scripts/e2e_rules.py`의 정적 규칙과 허용 목록, 시간 예산 보고). chute에도 같은 모양(문서 기준, 기계 검사, 기존 위반의 허용 목록, 새 위반만 막기)을 맞춘다.

오늘 chute에서 고친 흔들림은 모두 시험을 쓰는 방식에서 나왔다.

| 작업 | 흔들린 까닭 | 고친 방법 |
|---|---|---|
| flaky-two (#73) | 데몬 시작, gc 슬롯 | 원인에서 고침 |
| flaky-ext (#77, `docs/plans/flaky-ext.md`) | 시간 초과 시험이 벽시계 기한과 경주했다. 다른 시험 스레드의 fork가 fd 사본을 쥐었다 | `Ext`에 시계 주입(`src/clock.rs`의 `System`, `Frozen`), `/proc/self/fd` 단언 |
| flaky-rest (#80, #81, `docs/plans/flaky-rest.md`) | env 시험 300ms 취소·한도와 경주, 데몬 정지 파일의 2초 나이 규칙, `gc.json`을 쓰기 전의 틈, `post.pid`를 쓰는 도중의 빈 읽기 | 주입 시계, 시작 표시, 순서 판정, 마지막 관찰을 기다림, 임시 파일 뒤 이름 바꾸기 |

남은 미해결은 gc 시험 J(원인 모름)와 `chute down` 경쟁(`~/.claude/orch/reports/flaky-daemon/flaky-daemon.md`)이다.

기준을 문서로만 두면 다음 작업자가 읽지 않았을 때 막을 수 없다. 그래서 자동으로 확인할 수 있는 규칙은 검사로 만들고, 지금 있는 시험이 걸리는 곳은 허용 목록에 넣어 새 위반만 막는다.

## 2. 근거로 쓴 측정 (`3f37163`)

시험 코드는 `tests/**/*.rs`(`tests/support/mod.rs` 제외), `src/feedback/tests.rs`, `src/**/*.rs`에서 `#[cfg(test)]`가 붙은 인라인 `mod … {`부터 파일 끝까지로 쟀다. 정규식으로 센 값이라 작업자가 검사를 만들면서 정확한 값을 다시 센다.

| 무엇 | 곳 | 파일 | 많은 곳 |
|---|---|---|---|
| `thread::sleep(` | 27 | 11 | `src/gitops.rs` 7, `tests/locks.rs` 4 |
| `Instant::now(` | 49 | 13 | `tests/verify.rs` 9, `tests/engine.rs` 8, `src/ext.rs` 8 |
| `Duration::from_millis`·`from_micros`·`from_nanos`·`from_secs_f32/f64` (`advance(`의 인수 제외) | 43 | 13 | `tests/verify.rs` 10, `src/gitops.rs` 7 |
| `#[ignore` | 2 | 1 | `src/gitops.rs`. 둘 다 `"run as a child process by <시험>"`이고 그 시험이 같은 파일에 있다 |
| `set_var`·`remove_var`·`set_current_dir` | 0 | 0 | |
| `SystemTime::now(` | 8 | 6 | 모두 파일 시각을 며칠 전(8일, 40일)으로 정하는 데 쓴다 |
| 시험 코드의 `Command`의 `.spawn()` | 11 | 9 | `tests/preflight.rs` 3 |
| 제품 코드(`src/clock.rs` 밖)의 `Instant::now(` | 17 | 6 | `src/cmd/daemon.rs` 5, `src/verify.rs` 5, `src/engine.rs` 3 |

`wait_until` 같은 폴링 함수가 시험 파일마다 따로 있다(`tests/daemon.rs` 30초, `tests/locks.rs` 10초, `tests/post_verify.rs` 60초, `tests/verify.rs`의 `wait_for` 10초, `src/env.rs` 30초, `src/ext.rs`의 `wait_for`). 위 sleep과 `Instant::now`의 상당수가 이 함수들 안에 있다.

열린 PR은 없다(작성 시점 `gh pr list`).

## 3. 확정된 결정

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 순서 | vallythevally e2e 다음에 chute Rust 시험을 별도 플랜으로 한다 |
| 2 | 모양 | 문서 기준과 기계 검사를 함께 둔다. 기존 위반은 허용 목록에 두고 새 위반만 막는다 |
| 3 | 문서 기준의 내용 | 요청이 정한 일곱 가지: 시간 판정 코드는 시계를 주입받고 시험은 멈춘 시계로 돈다, 벽시계 sleep·짧은 기한과 경주하지 않는다, 결과는 관찰할 수 있는 상태로 확인한다, 같은 바이너리의 다른 시험과 독립이다, 외부 프로세스는 자기 프로세스 그룹과 함께 정리한다, 재시도·sleep 늘리기·`#[ignore]`로 덮지 않는다, 흔들림은 재현·원인·부하 반복 통과 횟수로 고친다 |
| 4 | 기존 시험 | 대량으로 고치지 않는다. 필요하면 후속 플랜으로 제안한다 |

## 4. 스스로 정한 결정과 이유

| 번호 | 항목 | 결정 | 이유 |
|---|---|---|---|
| A | 검사를 돌리는 곳 | 새 통합 시험 `tests/test_rules.rs`로 만들어 `cargo test` 안에서 돈다. `chute.toml`과 `clippy.toml`은 고치지 않는다 | `cargo test`는 chute batch의 `test` 단계라 모든 PR의 머지 전에 돈다. 언어나 도구를 새로 들이지 않고, 이미 의존성인 `regex`, `serde_json`을 쓴다. 실패하면 `chute.toml`의 `failed` 정규식이 시험 이름을 뽑아 반송 보고서가 규칙 이름을 보여 준다. 따로 `[[verify]]` 단계를 두면 `cargo test`와 별개로 한 번 더 빌드할 이유가 없고, 작업자가 PR 전에 돌리는 세 검사에도 이미 들어 있다. clippy의 `disallowed-methods`는 경로를 정확히 풀지만 제품 코드와 시험 코드를 가르지 못하고, 파일별 개수 허용 목록이 없어 기존 위반마다 `#[expect]`를 붙여야 한다. 짧은 기한과 `#[ignore]`의 이유 형식도 볼 수 없다 |
| B | 규칙 | 시험 코드에 다섯, 제품 코드에 하나, `tests/locks.rs`에 하나. 아래 표 | 모두 정규식으로 읽을 수 있고 지금 저장소에서 오탐이 없는 것만 골랐다 |
| C | 짧은 기한을 가리는 법 | 1초 미만 단위로 쓴 `Duration`(`from_millis`, `from_micros`, `from_nanos`, `from_secs_f32/f64`)을 짧은 기한으로 본다. 멈춘 시계를 옮기는 `advance(`의 인수는 뺀다. `from_secs`는 문서와 리뷰에 맡긴다 | 지금 시험의 1초 미만 값은 폴링 간격(10~25ms)과 벽시계 한도(200~600ms)다. 둘 다 오늘의 흔들림과 같은 꼴이다. `from_secs`는 설정 값 비교, 멈춘 시계의 긴 한도(3600초), 멈춤 상한(30~60초)에 정당하게 쓰여 정규식으로 가를 수 없다 |
| D | 정당한 길 | `tests/support/mod.rs`(단위 시험의 `crate::testkit`과 같은 파일)에 두 함수를 둔다. `wait_until(what, condition)`은 관찰할 수 있는 상태를 기다리고 60초 멈춤 상한에서 `what`으로 실패한다. `fixed_wait(ms, reason)`은 "정해진 시간 안에 일어나지 않는다"를 볼 때만 쓰는 실제 대기이고, 이유가 비면 실패한다. 이 파일은 시간 규칙에서 뺀다 | vallythevally의 `fixedWait(page, ms, reason)`과 같다. 이유를 주석이 아니라 인수로 받아 검사가 raw sleep만 찾으면 된다. 폴링 함수가 하나로 모이면 상한을 한 곳에서 정한다. 60초는 부하에서 3~5배 느려지는 측정(vallythevally)과 오늘 관찰한 정지 파일 나이 최대 2.3초를 넉넉히 덮는 멈춤 상한이고, 판정에는 끼지 않는다 |
| E | `#[ignore]` | `#[ignore = "run as a child process by <시험 이름>"]`이고 같은 파일에 `fn <시험 이름>`이 있을 때만 허용한다. 그 밖의 `#[ignore`는 위반이다. 허용 목록은 0으로 시작한다 | 지금 둘은 다른 시험이 자식 프로세스로 띄우는 진입점이다. 흔들리는 시험을 덮는 `#[ignore]`와 형식으로 가를 수 있다 |
| F | `SystemTime::now` | 기계 검사에 넣지 않고 문서에 "파일 시각은 판정 경계에서 며칠 떨어지게 정한다"고 적는다 | 지금 8곳은 모두 gc 같은 제품 판정이 벽시계로 파일 나이를 읽기 때문에 쓴다. 제품에 벽시계 주입이 없는 동안 막으면 새 gc 시험을 쓸 길이 없다. 여유가 며칠이라 경주하지 않는다 |
| G | 제품 코드의 `Instant::now` | `src/clock.rs` 밖의 제품 코드에서 막는다. 허용 목록 17곳 | 오늘 고친 흔들림 셋(flaky-ext의 Ext 기한, flaky-rest의 env 한도와 정지 파일 나이)이 모두 제품이 시각을 직접 읽어서 시험이 멈출 수 없던 곳이다. 새 시간 판정이 처음부터 `Clock`을 받게 한다. 사용자 결정 1 |
| H | 시험이 띄우는 프로세스 | `tests/support/mod.rs`에 `spawn(command) -> Spawned`를 둔다. 자기 프로세스 그룹(`process_group(0)`)으로 띄우고, `Drop`에서 그 그룹에 SIGKILL을 보낸 뒤 기다린다. 시험 코드에서 `Command`의 `.spawn()`을 직접 부르지 않는다. 허용 목록 11곳 | 지금 `tests/daemon.rs`의 `Daemon`은 자식만 죽여 데몬이 띄운 검사 프로세스가 남을 수 있다. 정리를 한 함수에 모으면 새 시험이 그룹 정리를 빠뜨리지 못한다. 사용자 결정 2 |
| I | `tests/locks.rs` | 이 파일에 프로세스를 띄우는 이름(`Command`, `Sandbox`, `TestRepo`, `spawn(` 중 `thread::spawn` 밖)이 나오면 위반이다. 허용 목록 0 | `AGENTS.md`의 규칙(잠금 시험 바이너리는 프로세스를 띄우지 않는다)을 기계로 지킨다. flaky-ext가 겪은 fd 사본 문제다 |
| J | 허용 목록 | 새 파일 `tests/test-rules-baseline.json`에 규칙별·파일별 개수를 적는다. 양쪽으로 맞춘다: 개수가 늘면 위반 줄마다 고칠 방법과 함께 실패하고, 줄면 허용 목록을 낮추라고 실패한다 | vallythevally와 같다. 줄 번호가 아니라 개수라 다른 줄을 고쳐도 걸리지 않는다. 줄어든 자리가 다시 차지 않는다. 기존 위반을 고친 PR은 같은 PR에서 그 숫자를 낮춘다 |
| K | 부하 반복 도구 | `scripts/stress`를 둔다. 바쁜 루프 N개를 띄우고 시험 바이너리를 동시에 M개씩 R라운드 돌려 "몇 회 중 몇 회 실패"와 실패한 시험 이름별 횟수를 낸다 | 오늘 flaky 작업들이 저마다 같은 부하(hog 40개, 동시 16개, 8라운드)를 손으로 만들었다. 도구가 있으면 문서가 명령 하나를 가리키고 보고서의 숫자를 서로 비교할 수 있다 |
| L | 문서 | 새 `docs/testing.md`. `AGENTS.md`의 `## 시험`에 "시험을 쓰거나 고치기 전에 읽는다"는 한 줄을 더한다 | `AGENTS.md`는 저장소 규칙만 짧게 둔다. 사례와 예시가 들어간 기준은 따로 둔다 |
| M | 작업 수와 순서 | 셋. `test-rules`와 `test-stress`는 병렬, `test-doc`은 둘 뒤 | 문서가 두 작업이 만든 이름(`wait_until`, `fixed_wait`, `spawn`, `test-rules-baseline.json`, `scripts/stress`)을 적으므로 먼저 머지되면 없는 도구를 안내한다. 두 도구 작업은 겹치는 파일이 없다 |

### 규칙 표

| 규칙 | 범위 | 찾는 것 | 허용 목록 시작값 | 고칠 방법 |
|---|---|---|---|---|
| `sleep` | 시험 코드 | `thread::sleep(`, `use std::thread::sleep`이나 `use std::thread::{…sleep…}`로 들여온 `sleep(` | 27 | `wait_until`로 상태를 기다린다. 시간 판정은 `Clock::frozen()`과 `advance`. "안 일어남"만 `fixed_wait` |
| `instant-now` | 시험 코드 | `Instant::now(` | 49 | 같음 |
| `short-duration` | 시험 코드 | `Duration::from_millis`·`from_micros`·`from_nanos`·`from_secs_f32`·`from_secs_f64`의 호출. `advance(` 바로 안의 인수는 뺀다 | 43 | 멈춘 시계와 긴 한도를 쓰고 관찰 뒤 시계를 옮긴다 |
| `ignore` | 시험 코드 | 결정 E의 형식이 아닌 `#[ignore` | 0 | 흔들림은 원인을 고친다 |
| `process-env` | 시험 코드 | `set_var(`, `remove_var(`, `set_current_dir(` | 0 | `Ext::from_vars`, `Home::from_vars`, `Sandbox::command`, `Command::current_dir` |
| `spawn` | 시험 코드 | `.spawn()`(빈 괄호. `thread::spawn(…)`은 걸리지 않는다) | 11 | `spawn(command)`으로 띄운다 |
| `locks-no-process` | `tests/locks.rs` | 결정 I의 이름 | 0 | 프로세스가 필요한 시험은 다른 시험 파일에 둔다 |
| `product-instant-now` | 제품 코드(`src/clock.rs` 제외) | `Instant::now(` | 17 | `Clock`을 받아 `clock.now()`를 읽는다 |

`tests/support/mod.rs`는 모든 시험 코드 규칙에서 뺀다(결정 D, H의 함수가 있는 곳). 범위와 경계는 지시서 `test-rules`에 자세히 적었다.

### 작업과 방식

| 작업 | 고치는 곳 | 프로필 | 방식 | 이유 | 순서 |
|---|---|---|---|---|---|
| `test-rules` | 새 `tests/test_rules.rs`, 새 `tests/test-rules-baseline.json`, `tests/support/mod.rs`에 `wait_until`·`fixed_wait`·`spawn`, 그 함수들의 시험(`tests/testkit.rs`) | `implementer` | `poteto` | 모든 PR의 머지 전 검증 판정(`cargo test`)에 실패 조건을 더한다. 시험용 프로세스 그룹 정리 코드가 들어간다 | 병렬 |
| `test-stress` | 새 `scripts/stress` | `minor-fix` | `poteto` | 파일 하나지만 바쁜 루프와 시험 프로세스를 띄우고 정리한다(프로세스 조건) | 병렬 |
| `test-doc` | 새 `docs/testing.md`, `AGENTS.md`의 `## 시험`에 한 줄 | `minor-fix` | `basic` | 문서만 바꾼다 | `test-rules`, `test-stress` 뒤 |

`test-doc`을 순차로 둔 이유는 결정 M이다. `test-rules`가 먼저 머지된 뒤 다른 작업이 시험에 sleep을 더하면 그 PR의 `cargo test`가 실패하고 고칠 방법을 출력한다. 의도한 동작이다. 허용 목록 파일은 기존 위반을 고치는 PR마다 고치게 되므로, 같은 파일의 숫자를 고친 PR 둘이 같은 batch에 들면 충돌로 반송될 수 있다. 파일별로 한 줄씩 정렬해 두어 충돌을 줄인다.

## 5. 범위 밖

- 기존 시험 고치기: 따로 있는 폴링 함수 여섯을 `wait_until`로 옮기기, `tests/verify.rs` 등의 벽시계 한도를 멈춘 시계로 바꾸기, 제품 코드 17곳의 `Instant::now`를 `Clock`으로 옮기기, `.spawn()` 11곳을 `spawn`으로 옮기기. 사용자 결정 3의 후속 플랜으로 제안한다.
- 아직 원인을 모르는 흔들림(gc 시험 J, `chute down` 경쟁).
- 벽시계(`SystemTime`)를 제품에 주입하기(결정 F).
- `from_secs`의 짧은 값, 가짜 도구 스크립트 안의 `sleep N`, 시험이 중간 상태를 끝으로 보는 것의 기계 검사. 정규식으로 정당한 쓰임과 가를 수 없어 문서와 리뷰에 맡긴다.
- 허용 목록을 늘리는 PR을 기계로 막기. 늘린 숫자는 diff에 보이고 실패 메시지와 문서가 늘리지 말라고 적는다. vallythevally와 같다.
- `chute.toml`, `clippy.toml`, 시험 시간 한도, orch 저장소의 `test_deploy` 대기 시간 초과.

## 6. 사용자가 정해야 하는 결정

| 번호 | 질문 | 플래너의 권고 |
|---|---|---|
| 1 | 제품 코드의 `Instant::now`도 기계 검사로 막을지 (결정 G) | 막는다. 새 시간 판정 코드가 처음부터 `Clock`을 받게 한다. 기존 17곳은 허용 목록이라 지금 고칠 곳은 없다. 빼면 규칙 표의 `product-instant-now` 한 줄만 빠진다 |
| 2 | 시험이 띄우는 프로세스를 `spawn` 함수로만 띄우게 할지 (결정 H) | 그렇게 한다. 프로세스 그룹 정리를 한 곳에 모은다. 빼면 문서 기준으로만 남는다 |
| 3 | 기존 위반을 줄이는 후속 플랜을 둘지 | 둔다. 먼저 폴링 함수 여섯을 `wait_until`로 옮기고(sleep과 `Instant::now`가 함께 준다), 다음에 `tests/verify.rs`의 벽시계 한도를 멈춘 시계로 바꾼다. 줄어든 만큼 허용 목록이 낮아진다 |

승인 때 다른 답이 없으면 지시서는 세 권고를 따른다.

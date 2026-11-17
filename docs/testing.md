# 시험 작성 기준

chute의 Rust 시험을 쓰거나 고치기 전에 읽는다. 자동으로 확인할 수 있는 규칙은 [기계 검사](#기계-검사)가 막고, 나머지는 이 문서와 리뷰가 맡는다. 시험이 사용자의 환경에 닿지 않게 하는 규칙(`CHUTE_HOME`, 가짜 `gh`·`hc` 등)은 [`AGENTS.md`](../AGENTS.md)의 `## 시험`에 있다.

이 기준은 2026-11-17에 고친 흔들림들(`docs/plans/flaky-ext.md`, `docs/plans/flaky-rest.md`, flaky-daemon)에서 나왔다. 모두 시험을 쓰는 방식 때문에 생겼다. 설계 배경은 [`docs/plans/test-standard.md`](plans/test-standard.md)에 있다.

## 시간에 따라 판정하는 코드는 시계를 주입받는다

기한이나 나이로 판정하는 코드는 `Instant::now()`를 직접 읽지 않고 [`src/clock.rs`](../src/clock.rs)의 `Clock`을 받는다. 시험은 `Clock::frozen()`으로 시계를 멈추고 `advance`로 옮긴다.

- 한도는 길게 둔다(예: 1시간). 제품이 시계를 무시하면 시험이 한도 안에서 끝나지 않아 실패한다.
- 한도를 건 일이 시작된 것을 관찰한 **뒤에** 시계를 한도 너머로 한 번 옮긴다.
- 제품은 기한을 프로세스를 띄우기 **전에** 읽는다. 띄운 뒤에 읽으면 시험이 시계를 옮긴 시점과 기한을 읽는 시점이 경주한다.

오늘의 예: `Ext`의 시간 초과 시험이 `Duration::from_millis(200)` 같은 벽시계 한도와 경주해 부하에서 흔들렸다(flaky-ext). `Ext::with_clock`으로 시계를 넣고, 시험이 `clock.advance`로 한도를 넘기게 했다. 처음 고칠 때는 제품이 기한을 읽기 전에 시험이 시계를 옮기는 결함이 있었고(flaky-ext의 결함 3), flaky-rest의 결정 D가 "기한은 띄우기 전에 읽는다"로 정했다. 그 모양이 `src/env.rs`의 `a_check_that_outlasts_the_limit_is_cut_and_the_next_one_still_runs`다. 한도는 `Duration::from_secs(3600)`이고, 첫 검사 로그에 `start`가 보인 뒤 `clock.advance(Duration::from_secs(2 * 3600))`를 한 번 부른다.

새 시간 판정 코드가 처음부터 `Clock`을 받게 하려고 제품 코드의 `Instant::now(`도 기계 검사가 막는다(`product-instant-now`).

## 벽시계 sleep과 짧은 기한과 경주하지 않는다

시험이 기다리는 것은 시간이 아니라 상태다. `tests/support/mod.rs`(단위 시험은 `crate::testkit`)의 `wait_until(what, condition)`으로 관찰할 수 있는 상태를 기다린다. 이 함수의 60초 상한은 멈춤을 잡는 것이지 판정이 아니다. 상한을 넘으면 `what`을 대며 실패한다.

"정해진 시간 안에 일어나지 않는다"를 보려는 마음이 들면 먼저 뒤에 오는 사건이나 순서로 바꿀 수 있는지 본다. 데몬이 시작할 때 정지 파일을 지우는 규칙은 "2초보다 오래된 것"이어서 시험이 바깥 프로세스의 시작 시각과 경주했다. flaky-rest의 결정 F가 이것을 "데몬이 잠금을 잡기 전부터 있던 것"이라는 순서 판정으로 바꿨다. 시험은 시각을 재지 않고 파일이 잠금 전에 있었는지 뒤에 생겼는지만 만든다(`src/cmd/daemon.rs`의 `a_stop_file_that_appeared_after_the_lock_is_kept`).

실제 시간이 꼭 필요할 때만 `fixed_wait(ms, 이유)`를 쓴다. 이유는 주석이 아니라 인수로 받고, 비어 있으면 시험이 실패한다. 그래서 기계 검사는 날 `thread::sleep`만 찾으면 된다.

## 결과는 관찰할 수 있는 상태로 확인한다

파일 내용, 프로세스 상태, `/proc`처럼 밖에서 볼 수 있는 것으로 확인한다. 확인할 때 세 가지를 지킨다.

- **중간 상태를 끝으로 보지 않는다.** 마지막에 생기는 관찰을 기다린다. `tests/daemon.rs`의 `the_daemon_runs_gc_when_it_starts_and_leaves_the_records`는 `gc.json`을 기다린다. gc가 일을 끝낸 뒤에 쓰는 파일이다. 그보다 앞선 표시(데몬이 떴다, 로그가 생겼다)를 기다리면 gc가 끝나기 전에 단언해 흔들렸다(flaky-daemon, 부하 384회 중 3회 실패).
- **시험이 읽는 파일은 임시 파일에 쓴 뒤 이름을 바꾼다.** 가짜 도구가 `post.pid`를 쓰는 도중에 시험이 읽으면 빈 문자열이 읽혀 `ParseIntError { kind: Empty }`로 실패했다(flaky-daemon, 384회 중 1회). `tests/daemon.rs`의 가짜 스크립트는 `echo $$ > post.pid.tmp`로 쓰고 `mv`로 이름을 바꾼다.
- **프로세스가 fd를 닫았는지는 `/proc/self/fd`로 본다.** `src/ext.rs`의 `pipes_close_after_a_timeout_even_for_an_escaped_grandchild`가 `held_pipes()`로 열린 파이프를 읽어 시간 초과 뒤 파이프가 닫혔는지 확인한다. "프로세스가 끝났다"만으로는 파이프 읽기 끝이 닫혔다는 뜻이 아니다.

## 같은 바이너리의 다른 시험과 독립이다

같은 시험 바이너리의 시험은 스레드로 동시에 돈다. 다른 시험에 기대거나 영향을 주지 않는다.

- **fork한 자식이 fd 사본을 쥔다.** 다른 시험 스레드가 프로세스를 띄우면 fork한 자식이 exec하기 전까지 그 순간 열려 있던 모든 fd를 함께 쥔다. 잠금의 의미를 보는 시험은 [`tests/locks.rs`](../tests/locks.rs)에 두고 그 바이너리에서는 프로세스를 띄우지 않는다. `/proc/self/fd` 단언이 흔들린 것(flaky-ext)도 같은 원인이다.
- **전역 상태를 쓰지 않는다.** 시험끼리 공유하는 `static`이나 고정 경로를 쓰지 않는다. 시험마다 `Sandbox::new()`로 새 임시 폴더를 만든다.
- **프로세스 환경 변수를 바꾸지 않는다.** `std::env::set_var`와 `remove_var`는 금지다. 환경은 `Ext::from_vars`, `Home::from_vars`, `Sandbox::command`로 넘긴다.
- **현재 디렉터리를 바꾸지 않는다.** `set_current_dir` 대신 `Command::current_dir`로 자식에게만 정한다.

## 시험이 띄우는 외부 프로세스는 `spawn`으로 띄운다

`tests/support/mod.rs`의 `spawn(command)`는 자식을 자기 프로세스 그룹(`process_group(0)`)으로 띄운다. 반환한 `Spawned`가 버려질 때 그 그룹 전체에 SIGKILL을 보내고 기다린다. 시험이 실패해 일찍 끝나도 자식이 띄운 손자(데몬이 띄운 검사 프로세스 등)까지 남지 않는다. 시험 코드에서 `Command`의 `.spawn()`을 직접 부르지 않는다. `Child`처럼 `Deref`로 쓸 수 있고 출력이 필요하면 `wait_with_output()`을 부른다.

## 재시도, sleep 늘리기, 한도 늘리기, `#[ignore]`로 덮지 않는다

흔들리는 시험을 재시도하거나, `sleep`을 늘리거나, 한도를 늘리거나, `#[ignore]`를 붙이면 흔들림이 사라지는 것이 아니라 가려진다. 원인을 고친다. 원인을 못 찾았으면 못 찾았다고 보고서에 적는다(flaky-daemon은 gc 시험의 나머지 실패를 512회 돌려도 못 잡았고, 미해결로 남겼다).

`#[ignore]`는 다른 시험이 자식 프로세스로 띄우는 진입점에만 쓴다. 이유를 `#[ignore = "run as a child process by <시험 이름>"]` 형식으로 적고, 그 시험을 같은 파일에 둔다(`src/gitops.rs`의 둘이 이렇다).

## 흔들림을 고치는 순서

1. **재현해 고치기 전 실패율을 잰다.** [`scripts/stress`](../scripts/stress)로 부하를 건 채 반복한다.
   ```sh
   scripts/stress --hogs 40 --jobs 16 --rounds 8 --test daemon -- --exact <시험 이름>
   ```
   `--hogs`는 바쁜 루프 수, `--jobs`는 바이너리마다 동시에 돌리는 수, `--rounds`는 라운드 수다. `--` 앞의 나머지 인수는 `cargo test --no-run`에, `--` 뒤는 시험 바이너리에 그대로 간다. 총 횟수는 라운드 × 동시 수 × 바이너리 수다. 도구는 "몇 회 중 몇 회 실패"와 실패한 시험 이름별 횟수를 낸다. 종료 코드는 0이 전부 통과, 1이 한 번이라도 실패, 2가 인수 오류나 빌드 실패다. 옵션은 `scripts/stress --help`를 본다.
2. **원인을 찾는다.** 진단(stderr를 파일로 남기기 등)은 커밋하지 않는다.
3. **고친다.**
4. **같은 부하에서 연속 통과 횟수를 잰다.** 1번과 같은 인수로 돌린다.
5. **제품을 임시로 망가뜨려 고친 시험이 여전히 결함을 잡는지 본다.** 시험이 시간에서 풀려나면서 아무것도 보지 않게 되는 것을 막는다. 망가뜨린 것은 커밋하지 않는다.
6. **네 숫자와 결과를 PR 본문이나 보고서에 남긴다.** 고치기 전 실패율, 고친 뒤 통과 횟수, 결함 검출 결과, 부하 조건이다.

flaky-daemon 보고서의 표가 예다.

| 시험 | 고치기 전 | 고친 뒤 |
|---|---|---|
| 정지 요청 | 16/128 실패 | 0/128 |
| gc (부하를 더 올림: hog 80, 동시 32, 12라운드) | 3/384 실패 | 0/384 |
| post 검사 (같은 부하) | 1/384 실패 | 0/384 |

부하와 함께 적는다(hog 40개, 동시 16개, 8라운드 = 128회). 부하가 다르면 숫자를 서로 비교할 수 없다.

## `SystemTime::now`로 파일 시각을 정할 때

기계 검사 밖이다. 제품의 gc 같은 판정이 벽시계로 파일 나이를 읽으므로 시험이 `SystemTime::now()`로 파일 시각을 정해야 한다. 판정 경계에서 며칠 떨어지게 정한다. `tests/daemon.rs`가 gc 대상 폴더의 시각을 `Duration::from_secs(8 * 24 * 60 * 60)` 전으로 둔 것처럼 한다. 경계 근처로 정하면 시험이 도는 동안 시각이 지나 경주한다.

## 기계 검사

[`tests/test_rules.rs`](../tests/test_rules.rs)가 `cargo test`의 일부로 돈다. `chute.toml`의 `test` 단계가 `cargo test`이므로 모든 PR이 머지 전에 chute batch에서 이 검사를 통과해야 한다. 시험 코드는 `tests/**/*.rs`(`tests/support/mod.rs` 제외), `src/feedback/tests.rs`, `src/**/*.rs`의 `#[cfg(test)]` 인라인 모듈이다. 제품 코드는 그 나머지다.

| 규칙 | 찾는 것 | 시험 이름(`cargo test --test test_rules`) | 고칠 방법 |
|---|---|---|---|
| `sleep` | 시험 코드의 `thread::sleep(`와 들여온 `sleep(` | `test_code_does_not_sleep` | `wait_until`로 상태를 기다린다. 시간 판정은 `Clock::frozen()`과 `advance`. "안 일어남"만 `fixed_wait` |
| `instant-now` | 시험 코드의 `Instant::now(` | `test_code_does_not_read_instant_now` | 같음 |
| `short-duration` | 시험 코드의 `Duration::from_millis`·`from_micros`·`from_nanos`·`from_secs_f32`·`from_secs_f64`. 멈춘 시계를 옮기는 `advance(`의 첫 인수는 뺀다 | `test_code_has_no_short_durations` | 멈춘 시계와 긴 한도를 쓰고, 관찰한 뒤 시계를 한도 너머로 옮긴다 |
| `ignore` | 위 형식이 아닌 `#[ignore` | `test_code_has_no_unexplained_ignore` | 원인을 고친다. 자식 프로세스 진입점만 `#[ignore = "run as a child process by <시험 이름>"]` |
| `process-env` | `set_var(`, `remove_var(`, `set_current_dir(` | `test_code_does_not_change_the_process_environment` | `Ext::from_vars`, `Home::from_vars`, `Sandbox::command`, `Command::current_dir` |
| `spawn` | 시험 코드의 `.spawn()`(빈 괄호) | `test_code_spawns_children_through_the_helper` | `tests/support/mod.rs`의 `spawn(command)` |
| `locks-no-process` | `tests/locks.rs`에서 프로세스를 띄우는 이름 | `locks_tests_start_no_process` | 프로세스가 필요한 시험은 다른 시험 파일에 둔다 |
| `product-instant-now` | `src/clock.rs` 밖 제품 코드의 `Instant::now(` | `product_code_reads_time_through_the_clock` | `Clock`을 받아 `clock.now()`를 읽는다 |

### 허용 목록

지금 있는 시험과 제품이 걸리는 곳은 [`tests/test-rules-baseline.json`](../tests/test-rules-baseline.json)에 규칙 이름별·파일별 개수로 적혀 있다. 이 목록은 새 위반만 막으려는 것이다. 양쪽으로 맞춘다.

- 개수가 늘면 위반한 줄마다 고칠 방법과 함께 실패한다(`<파일>:<줄> [<규칙>] 허용 N곳, 실제 M곳. …`).
- 개수가 줄어도 실패한다(`<파일> [<규칙>] 허용 목록을 낮춘다(허용 N곳, 실제 M곳)`). 줄어든 자리가 다시 차지 않게 하려는 것이다. 기존 위반을 고친 PR은 같은 PR에서 그 숫자를 낮춘다.
- 목록은 줄 번호가 아니라 개수라서 다른 줄을 고쳐도 걸리지 않는다.
- **목록을 늘려서 새 위반을 통과시키지 않는다.** 새 시험은 처음부터 위 규칙을 지킨다.

### 기계가 보지 않는 것

정규식으로 정당한 쓰임과 가를 수 없어 이 문서와 리뷰가 맡는다.

- `Duration::from_secs`의 짧은 값. 설정 값 비교, 멈춘 시계의 긴 한도(3600초), 멈춤 상한(30~60초)에도 정당하게 쓰여서다. 벽시계 한도로 `from_secs(1)`을 쓰지 않는다.
- 가짜 도구 스크립트 안의 `sleep N`.
- 시험이 중간 상태를 끝으로 보는 것.
- `SystemTime::now`로 정한 파일 시각이 판정 경계에 가까운지.

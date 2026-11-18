# chute 검증 단계의 CPU 사용 상한 플랜

> 작성일 2026-11-18. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

이 PC는 `nproc` 32다. 2026-11-18 load average가 15분 평균 160까지 올랐다(코어의 약 5배). chute는 PC 전체에서 검증을 최대 3개(`machine.toml`이 없어 `max_verify` 기본 3) 동시에 돌린다. 그런데 이 저장소 `chute.toml`의 `clippy`·`test` 단계는 빌드 jobs와 시험 스레드에 상한이 없어 각각 32로 돈다. 여러 저장소의 batch와 작업자 빌드가 겹치면 과부하가 된다. 사용자는 clippy·test 단계가 빌드 jobs 8, 시험 스레드 8을 넘지 않기를 원한다.

main `5333907`에서 확인한 사실:

- 검증 명령은 데몬의 환경을 물려받아 슬롯 폴더에서 `sh -c`로 돈다(`src/verify.rs`의 `attempt`). 명령 앞에 플래그나 환경 변수를 적으면 그대로 먹는다.
- `chute.toml` 머리 주석은 "AGENTS.md의 검사와 같다"이고, `AGENTS.md`의 `## 검사`는 "저장소 루트의 `chute.toml`에도 같은 명령이 있다"고 적는다. 둘이 서로를 가리키는 대응은 이 한 쌍뿐이다. README 42줄의 "`cargo test`로 시험한다"와 `docs/testing.md` 83줄의 "`test` 단계가 `cargo test`이므로"는 명령을 그대로 옮긴 것이 아니라 무엇을 돌리는지 말하는 글이라 바뀐 뒤에도 맞는다.
- 저장소 안의 시험은 이 저장소 루트의 `chute.toml`이나 `AGENTS.md`를 읽지 않는다(`tests/`의 `chute.toml` 읽기는 모두 시험용 저장소의 것이다).
- 시험 가운데 시험 바이너리를 다시 띄우는 둘(`src/gitops.rs`의 hook 시험, `tests/testkit.rs`의 sandbox 시험)은 자식에 `--test-threads=1`을 직접 준다.
- `reuse_verified`의 지문에 단계의 `run`이 들어간다. `run`을 바꾸면 머지 뒤 첫 batch는 옛 통과 기록을 다시 쓰지 않고 검증을 한 번 돈다.
- 검증은 PR의 설정이 아니라 기본 브랜치의 `chute.toml`로 돈다. 바뀐 상한은 구현 PR이 머지된 다음 batch부터 쓰인다.

플래너가 이 PC에서 잰 값(cargo 1.96.0, load average 1~8일 때, 빌드 캐시는 단계마다 같은 조건):

| 단계 | 지금(jobs·스레드 32) | 상한 8 |
|---|---|---|
| `cargo clippy --all-targets -- -D warnings` 빈 캐시 | 9.6초 | 9.7초 |
| `cargo test --no-run` 빈 캐시 | 13.0초 | 14.3초 |
| `cargo test` 시험 실행(빌드 끝난 뒤) | 36.3초, 38.5초 | 47.1초, 47.5초 |

- 상한 8로 두 번, 지금 방식으로 두 번 돌렸고 네 번 모두 1382개 통과, 2개 무시(`#[ignore]`), 실패 0이다. 시간 의존이나 병렬 가정으로 깨지는 시험은 보이지 않았다.
- test 단계는 약 10초(약 25%) 길어진다. 가장 오래 걸리는 `tests/daemon.rs`(15개, 약 11초)는 그대로이고, `src/lib.rs` 단위 시험(655개)이 5.1초→7.8초, `tests/engine.rs`가 3.5초→5.0초, `tests/verify.rs`가 2.5초→4.1초로 늘었다. 시험 실행의 sys 시간은 93초→59초로 줄었다.
- 두 방식 모두 로그의 `test <이름> ... ok` 줄이 1382개로 시험 수와 같다. libtest는 스레드가 둘 이상이면 시험 하나를 한 줄에 적으므로 `failed = 'test (\S+) \.\.\. FAILED'`는 그대로 맞는다.
- 바꾼 `chute.toml`의 사본을 `chute config-check --file`로 읽으면 "설정이 올바르다"와 단계 4개(`fmt` 300초, `mod` 300초, `clippy` 1800초, `test` 1800초)가 나온다.

## 2. 확정된 결정

- `chute.toml`의 `clippy`·`test` 단계를 빌드 jobs 8, `cargo test` 시험 스레드 8 이하로 묶는다(요청).
- 단계와 시험 목록은 그대로 두고 병렬도만 줄인다(요청).
- "AGENTS.md의 검사와 같다" 대응을 맞춘다(요청).
- PR 하나, 작은 작업이다(요청).

## 3. 스스로 정한 결정과 이유

| 결정 | 이유 |
|---|---|
| 명령은 `cargo clippy -j 8 --all-targets -- -D warnings`와 `cargo test -j 8 -- --test-threads=8` | 상한이 명령 줄에 보여 `chute.toml`과 반송 보고서의 명령만 보고 알 수 있다. cargo의 `-j`는 `CARGO_BUILD_JOBS`보다 앞서므로 데몬 환경에 다른 값이 있어도 8을 넘지 않는다. `RUST_TEST_THREADS`를 쓰면 그 환경 변수가 시험이 띄우는 `chute`, `git`, 가짜 `gh`/`hc`와 그 아래 검증 명령에까지 물려 내려가지만, `--test-threads`는 cargo가 시험 바이너리에만 넘긴다 |
| 값은 8 | 요청한 값이다. `max_verify` 3개가 한꺼번에 돌아도 빌드 jobs 합이 24로 코어 32 안에 들고, 작업자 빌드에 자리가 남는다. 측정에서 clippy와 빌드는 거의 같고 시험 실행만 약 10초 늘었다 |
| `timeout`은 그대로 1800초 | 상한 8에서 test 단계가 빌드 포함 약 1분이다. 부하가 큰 때 몇 배로 늘어도 30분에 한참 못 미친다. 늘리지도 줄이지도 않는다 |
| `failed` 정규식은 그대로 | 위 측정대로 스레드 8에서도 시험마다 한 줄 `test <이름> ... <결과>` 형식이다 |
| `AGENTS.md`의 `## 검사` 명령 블록을 `chute.toml`과 똑같은 명령으로 바꾼다 | 두 파일이 "같은 명령"이라고 서로 가리키므로 한쪽만 바꾸면 틀린 글이 된다. 작업자가 PR 전에 이 명령을 그대로 돌리므로 작업자 빌드도 같은 상한을 지켜 과부하가 덜하다. 머리 주석과 "같은 명령이 있다" 문장은 그대로 맞으니 고치지 않는다 |
| `fmt`·`mod` 단계는 고치지 않는다 | `cargo fmt --check`는 컴파일하지 않는다. `node --test mod/*/*.test.js`는 시험 파일이 둘뿐이라 동시에 많아야 둘이다 |
| README, `docs/testing.md`는 고치지 않는다 | 1절대로 두 곳은 명령을 옮겨 적은 것이 아니고 바뀐 뒤에도 맞는다 |

## 4. 작업

| 작업 ID | 내용 | 방식 | 방식의 이유 |
|---|---|---|---|
| `verify-threads` | `chute.toml`의 `clippy`·`test` `run` 두 줄과 `AGENTS.md`의 `## 검사` 명령 블록 두 줄 | `basic` | 설정과 문서 네 줄이다. 검증 판정 코드(`src/verify.rs` 등)는 건드리지 않고 단계와 시험 목록, `failed`, `timeout`도 그대로다 |

작업이 하나라 병렬·순차 관계는 없다. 파일 둘에 각각 두 줄이라 profile은 `minor-fix`다.

## 5. 범위 밖

- `machine.toml`의 `max_verify` 조정. PC의 로컬 설정이라 저장소에 두지 않는다.
- 다른 저장소의 `chute.toml`.
- chute가 검증 명령에 jobs나 스레드 상한을 스스로 주는 기능(`src/verify.rs`). 이번에는 이 저장소의 설정만 고친다.
- `scripts/stress`의 기본 동시 수.
- 오래 걸리는 시험(`tests/daemon.rs`)을 빠르게 하는 일.

## 6. 사용자가 정해야 하는 결정

- 없음. 이 플랜은 검증을 약하게 하지 않는다. 단계(`fmt`, `mod`, `clippy`, `test`), 각 단계의 시험 목록, `failed`, `timeout`, `paths`(없음)는 그대로이고 빌드와 시험의 병렬도만 줄인다.
- 알아 둘 것: batch의 test 단계가 약 10초 길어진다. 상한은 구현 PR이 머지된 다음 batch부터 쓰이고, `run`이 바뀌어 머지 뒤 첫 batch는 옛 통과 기록을 다시 쓰지 않는다.

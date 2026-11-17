# 흔들리는 시험 둘을 고치는 플랜

> 작성일 2026-11-17. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

chute 완성도 평가에서 "다음으로 흔들리는 시험 둘을 고친다"는 추천에 사용자가 "응 진행해줘"라고 답했다. 두 시험은 전체 실행 때 가끔 실패하고 단독으로 돌리면 통과한다. 흔들리는 시험은 chute batch에서 엉뚱한 PR을 반송하거나 `flaky` 기록을 늘려 머지를 늦춘다.

| 시험 | 관찰 |
|---|---|
| `tests/daemon.rs`의 `a_second_daemon_ends_at_once_and_down_stops_the_first_after_its_batch` | preflight-policy 작업 보고서(83줄): 부하를 준 동시 6회 가운데 1회, `tests/daemon.rs:74`(`lab.store.daemon().unwrap().unwrap()`)에서 실패했다. 데몬 잠금은 잡혔는데 `daemon.json`이 아직 없는 순간이다 |
| `src/gc.rs`의 `surplus_slots_go_with_their_worktree_registration_and_the_rest_stay` | board-times 작업 보고서(9줄): 전체 `cargo test --lib`에서 한 번 실패했고 단독 재실행은 통과했다. 어느 단언에서 실패했는지는 기록되지 않았다 |

## 2. 확정된 결정

| 번호 | 항목 | 결정 |
|---|---|---|
| 1 | 고치는 방법 | 재현으로 원인을 찾아 원인을 고친다. 재시도, sleep 늘리기, 시험 끄기(`#[ignore]`)로 덮지 않는다 |
| 2 | 제품 코드 | 원인이 제품 코드의 경쟁이면 제품 코드를 고친다 |
| 3 | 근거 | 고친 뒤 부하를 준 상태에서 반복 실행해 흔들림이 사라진 근거를 보고서에 남긴다 |
| 4 | 작업 수 | 최소로 한다. 사용자 주간 사용량이 90%대다 |

## 3. 다른 흔들린 시험 (목록)

요청대로 chute의 흔들림 장부와 batch 기록을 봤다(2026-11-17, main `b259340`).

- `chute flaky --repo /home/dev/code/chute`: "최근 30일에 흔들린 시험이 없다".
- batch 기록(`~/.local/state/chute/repos/Soochol__chute/batches/`, 32개)에서 반송은 둘이다. 둘 다 같은 시험이 두 번 모두 실패한 고정 실패이고 흔들림이 아니다.
  - `b20261117-101606`(#65): `tests/cli.rs`의 `status_shows_queue_items_from_the_state_folder`
  - `b20261116-192546`(#43): `tests/docs.rs`의 `readme_gives_the_first_run_check_and_the_manual_open_command`
  - 반송되지 않은 `b20261116-190238`의 `cache_dir_is_not_set_for_a_run_without_one`도 네 번 모두 실패했다.
- preflight 기록(17개)의 실패도 위 두 시험의 고정 실패뿐이다.
- 작업 보고서와 오케스트레이터 메모가 부하에서 흔들렸다고 적은 시험 셋(`tests/ci_gate.rs`의 `only_the_ci_waiters_whose_interval_has_passed_are_read_again`, `tests/daemon.rs`의 `a_daemon_killed_before_it_told_the_agent_tells_it_after_the_restart`, `src/cmd/guard.rs`의 `a_command_with_thousands_of_heredoc_openers_finishes`)은 2026-11-16 커밋 `600fb41`("부하에 흔들리던 시험 셋이 시간에 기대지 않게 한다")에서 이미 고쳤다. 그 뒤 새 관찰은 없다.

그래서 이 플랜은 두 시험만 고친다.

## 4. 스스로 정한 결정

| 번호 | 항목 | 결정 | 이유 |
|---|---|---|---|
| A | 작업 수 | 하나(`flaky-two`). 두 시험을 한 PR에서 고친다 | 결정 4. 두 수정은 파일이 겹치지 않지만 둘 다 작고, 나누면 작업자 세션과 batch가 하나씩 더 든다 |
| B | 데몬 시험의 원인 가설 | `chute daemon`은 `try_lock_daemon`으로 잠금을 잡은 뒤 첫 `doing()` 호출(`serve` 안)에서야 `daemon.json`을 쓴다. 그 사이 신호 처리기 설치, 감시·post 스레드 시작, `engine_warning`이 있다. 이 사이에 `daemon_running()`은 참인데 `daemon()`은 `None`(또는 죽은 앞 데몬이 남긴 옛 pid)이다 | `src/cmd/daemon.rs`의 `run`과 `doing`, `src/store.rs`의 `daemon_running`을 읽었다. 보고서의 실패 줄과 맞는다. 작업자가 재현으로 확인한다 |
| C | 데몬 시험의 고칠 곳 | 제품 코드다. 잠금을 잡은 바로 뒤 다른 일보다 먼저 자기 `DaemonInfo`(pid와 시작 중임을 알리는 `doing`)를 쓴다. 잠금과 쓰기 사이의 짧은 틈은 남으므로, 시험은 "데몬이 떴다"를 `daemon_running()`이 아니라 `daemon.json`에 자기 pid가 보이는 것으로 기다린다 | 같은 틈이 사용자에게도 보인다. `chute status`는 잠금이 잡혀 있으면 `daemon.json`을 읽는다(`src/cmd/status.rs` 341~342줄). 앞 데몬이 죽어 `daemon.json`이 남았으면 새 데몬이 첫 `doing`을 쓸 때까지 죽은 pid를 보인다. 먼저 쓰면 그 창이 거의 없어진다. 남는 틈을 없애려면 잠금 파일에 정보를 넣는 식의 큰 변경이 드는데, 요청 범위를 넘는다 |
| D | gc 시험의 원인 가설 | 시험이 `lock_slot(...).open()`으로 슬롯 넷을 만들고 바로 잠금을 놓는다. 같은 `cargo test --lib` 바이너리의 다른 시험 스레드가 그 사이 fork한 자식은 exec하기 전까지 잠금 파일의 사본을 쥔다. 그러면 `sweep`의 `lock_surplus_preflight_slots`가 그 슬롯을 "누가 쥐고 있다"로 보고 건너뛰어 지운 슬롯 수 단언이 틀린다 | `AGENTS.md`와 `tests/locks.rs` 머리 주석이 적은 바로 그 현상이다. 단위 시험 바이너리에서는 다른 시험이 `git`과 가짜 `gh`/`hc`를 계속 띄운다. 작업자가 재현으로 확인한다 |
| E | gc 시험의 고칠 곳 | 시험이다. 시험이 이 프로세스에서 잠금 파일을 열었다 닫는 일 없이 슬롯 worktree를 만든다(예: `git worktree add`를 슬롯 경로에 직접 실행하는 준비 함수). 잠금의 의미는 이미 `tests/locks.rs`의 `gc_offers_only_surplus_preflight_slots_nobody_holds_and_keeps_them_locked`가 본다 | 제품에서는 같은 현상이 "이번 gc에서 그 슬롯을 건너뛰고 다음 시간에 지운다"로만 나타나 해가 없다. 실제 gc는 데몬 안에서 한 시간마다 돌고, 그 데몬은 preflight 슬롯을 쥐지 않는다. 그래서 제품 잠금 방식을 바꾸지 않는다. 재현이 다른 원인을 가리키면 작업자가 그 원인을 고치고 보고서에 적는다 |
| F | 흔들림이 사라진 근거 | 부하를 준 상태(예: 다른 `cargo test`나 `stress`를 함께 돌림)에서 고치기 전에 실패를 재현한 횟수와 고친 뒤 연속 통과 횟수를 보고서에 적는다. 데몬 시험은 `cargo test --test daemon` 반복, gc 시험은 `cargo test --lib` 전체 반복이다 | 결정 3. gc 시험은 다른 시험 스레드가 있어야 흔들리므로 단독 반복은 근거가 되지 않는다 |
| G | 고치기 전 재현을 못 한 경우 | 가설을 코드로 강제해(예: 잠금과 첫 쓰기 사이, 또는 잠금을 놓은 뒤에 일부러 틈을 넓히는 임시 변경) 같은 실패가 나는지 보이고, 그 임시 변경은 커밋하지 않는다 | 부하 재현은 기기 상태에 따라 드물다. 원인을 보였다는 근거가 필요하다 |

### 작업과 방식

| 작업 | 프로필 | 방식 | 이유 |
|---|---|---|---|
| `flaky-two` | `minor-fix` | `poteto` | 파일 셋과 시험 둘이라 `minor-fix`다. 데몬 시작 순서(프로세스, 잠금, 상태 파일)를 바꾸므로 `basic` 조건에 걸린다 |

순차로 둔 작업은 없다(작업이 하나다).

## 5. 범위 밖

- `daemon_running()`과 `daemon.json`을 한 번에 원자적으로 읽게 하는 설계 변경(잠금 파일에 정보를 넣는 등).
- 슬롯 잠금 방식(`flock`) 변경과 자식 프로세스의 잠금 상속 대책.
- 위 3절의 고정 실패 시험들(각 PR에서 이미 고쳐 머지됐다)과 `600fb41`에서 고친 세 시험.
- `chute status --json`의 필드 이름과 뜻. `daemon.doing`에 새 문구 값이 하나 생길 수 있으나 필드는 그대로다.

## 6. 사용자가 정해야 하는 결정

없다. 결정 C(데몬 시작 때 `daemon.json`을 먼저 씀)와 E(gc 시험만 고침)는 위 이유로 정했다. 다르게 하려면 승인 때 알려 준다.

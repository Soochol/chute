# chute 실환경 시험 묶음

가짜 `gh`와 `hc`로 도는 `cargo test`는 GitHub와 hc 터미널이 실제로 하는 일을 보지 못한다. 이 묶음은 시험 전용 private 저장소 `Soochol/chute-sandbox`에서 실제 `git`, `gh`, `hc`, `chute` 바이너리로 시나리오를 돌린다. 네트워크가 필요하고 몇 분 걸리므로 기본 `cargo test`에는 들어가지 않는다. chute를 고칠 때마다 손으로 돌린다.

## 돌리는 법

```sh
cargo build                       # 시험할 chute를 target/debug/chute에 만든다
scripts/livetest/run.sh           # 시나리오 전부
scripts/livetest/run.sh 02 07     # 이름이 02, 07로 시작하는 시나리오만
scripts/livetest/scenarios/05-flaky.sh   # 한 시나리오를 직접
```

필요한 것은 `git`, 로그인한 `gh`(`Soochol/chute-sandbox`에 쓰기 권한), 떠 있는 hc 코어, `jq`다. 환경 변수는 다음과 같다.

| 변수 | 뜻 |
|---|---|
| `CHUTE_BIN` | 시험할 chute 실행 파일. 기본값은 `target/debug/chute`이고 없으면 `cargo build`를 한다 |
| `LIVETEST_REPO` | 시험 저장소. 이름이 `/chute-sandbox`로 끝나야 한다. 다른 저장소에는 강제 push와 PR 닫기를 하지 않는다 |
| `LIVETEST_TIMEOUT` | 시나리오 하나의 제한 시간(초). 기본 600 |

`run.sh`는 시나리오마다 통과, 실패, 시간 초과, 환경 오류를 표로 보이고 끝에 남은 것(터미널, 열린 PR, 브랜치, 임시 폴더, 시험 데몬)을 확인한다. 종료 코드 0은 모두 통과하고 남은 것도 없다는 뜻이다. 시나리오별 로그는 `/tmp/chute-livetest-logs.*`에 남는다.

## 격리

- 시나리오마다 임시 폴더 `/tmp/chute-livetest.*`를 만든다. 거기에 `CHUTE_HOME`(chute 상태), 샌드박스 clone, 시험 표식이 들어가고 끝나면 지운다. 사용자의 `~/.local/state/chute`와 다른 저장소의 데몬은 건드리지 않는다.
- hc 터미널은 호출한 셸의 환경 변수를 물려받지 않는다. 그래서 chute가 `hc run -- chute daemon`으로 띄우는 데몬은 `CHUTE_HOME`을 잃고 사용자의 실제 상태 폴더를 읽는다. 시나리오가 `PATH` 맨 앞에 두는 `hc` 껍데기가 이 호출 하나만 `env CHUTE_HOME=… chute daemon`으로 바꿔 준다. 나머지 `hc` 호출은 그대로 실제 hc로 간다.
- 알림을 받는 터미널(`agent`는 반송을 받는 작업자, `orch`는 submit한 터미널)은 `hc run`으로 띄운 `cat` 터미널이다. 알림이 도착했는지는 `hc read`로 본다. 오케스트레이터 터미널로는 아무것도 보내지 않는다.
- 시나리오는 시작할 때와 끝날 때 샌드박스를 초기 상태로 되돌린다. main을 시드 커밋으로 강제 push하고, 열린 PR을 닫고, main 말고 모든 브랜치를 지운다.

## 샌드박스 검증 단계

`sandbox/`가 시드다. `chute.toml`은 `require_head = true`, `respawn = []`이고 검증 단계는 `sh check.sh` 하나다(제한 시간 12초). `check.sh`는 `cases/` 아래 파일마다 내용으로 결과를 정한다.

| 내용 | 결과 |
|---|---|
| `ok` | 통과 |
| `fail` | `FAIL <이름>`을 찍고 실패 |
| `flaky` | 이 이름으로 처음 도는 때만 실패. 표식은 `$LIVETEST_STATE`에 둔다 |
| `hang` | 제한 시간을 넘겨 멈춘다 |
| `slow:<n>` | n초 뒤 통과(batch가 도는 동안 끼어들 틈을 만든다) |
| `excl:<태그>` | 혼자서는 통과하고 같은 태그가 둘이면 실패(조합 실패) |

시나리오는 PR마다 `cases/<이름>.txt`를 더하는 식으로 결과를 정한다. 같은 줄 충돌은 `app.txt`로 만든다.

## 시나리오

| 파일 | 보는 것 |
|---|---|
| `01-batch-merge` | PR 3개가 한 batch로 머지되고 GitHub에서 모두 MERGED |
| `02-bisect` | 3개 중 깨진 1개만 반송되고 나머지는 머지 |
| `03-conflict` | 같은 줄을 고친 두 PR 가운데 뒤의 PR이 충돌로 반송 |
| `04-combo` | 각자 통과하고 합치면 실패하는 PR이 조합 실패(`mode=combo`)로 반송 |
| `05-flaky` | 흔들리는 단계가 다시 돌려 통과하고 `chute flaky` 장부에 남음 |
| `06-timeout` | 단계 시간 초과 반송, 멈춘 프로세스가 남지 않음 |
| `07-head-moved` | submit 뒤 push하면 `head_changed` 반송, 예산을 쓰지 않음, 다시 submit하면 머지 |
| `08-daemon-kill` | batch 검증 도중 데몬을 `kill -9`하고 다시 띄워도 큐와 main이 맞게 이어짐 |
| `09-unreachable` | 반송받을 터미널이 끝났거나 지워졌을 때 사람 차례(`unreachable`)와 submitter 알림 |
| `10-after-priority` | `--after`는 대상이 머지될 때까지 batch에 넣지 않고 `--priority`는 먼저 넣음 |
| `11-revert` | `chute revert`는 revert PR이 아니라 main에 revert 커밋을 쌓음 |
| `12-pr-closed` | batch 도중 닫힌 PR은 머지되지 않고 나머지는 머지 |
| `13-main-broken` | 깨진 main에서 큐가 멈추고 `--repair`로 풀림 |

## 시나리오 더하기

`scenarios/NN-이름.sh`를 만들고 `lib.sh`를 읽어 들인다. `sc_setup`이 샌드박스와 터미널을 준비하고 종료 때 정리를 걸어 둔다. `mk_pr`, `submit`, `wait_for`, `check`, `ev_has`, `term_text`를 쓰고 `sc_done`으로 끝낸다. 단언이 하나라도 실패하면 종료 코드 1이다. chute의 결함이 나오면 시나리오에서 우회하지 않는다. 실패한 채로 두고 재현 방법을 보고서에 적는다.

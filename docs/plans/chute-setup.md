# `chute init --setup` 플랜

> 작성일 2026-11-18. 플랜 에이전트가 쓴 설계다. 승인 시점의 기록이며 구현 중 바뀐 것은 반영하지 않는다.

## 1. 사용자가 풀려는 문제

사용자는 다른 저장소(psa-stm32-firmware)에 chute를 처음 붙이려다 README "처음 쓰기"가 여러 단계로 나뉜 것을 겪었다. `chute init`(`src/cmd/init.rs`)은 `chute.toml` 초안을 쓰고 상태 폴더에 등록하고 지침 파일에 넣을 한 줄을 출력할 뿐이다. chute는 `<원격>/<기본 브랜치>`에 올라간 `chute.toml`만 읽으므로 첫 `chute.toml` PR은 큐로 넣을 수 없고 사람이 따로 머지해야 한다. 사용자는 이 과정을 명령 하나로 끝내고 싶어 한다.

```sh
chute init --setup --verify "pio run -e stm32_debug" --verify "python3 tests/test_gps_i2c3.py"
```

main `6289027`에서 확인한 사실:

- `chute init`은 작업 폴더에 `chute.toml`이 있으면 그대로 두고, `<원격>/<기본 브랜치>:chute.toml`이 있으면 초안을 쓰지 않고, 둘 다 없으면 `config::draft(guess_verify(..))`를 쓴다. 짐작은 `Cargo.toml`→`cargo test`, `package.json`→`npm test`, `go.mod`→`go test ./...`이고 없으면 `FALLBACK_VERIFY`(`exit 1`로 끝나는 안내 명령)다. 그 뒤 등록하고 `AGENTS_LINE`을 출력한다.
- `chute config-check`(`src/cmd/config_check.rs`)는 `Config::parse`만 한다. `FALLBACK_VERIFY`는 올바른 설정이라 통과한다.
- 큐의 검증은 `verify::run(config, store, slot, log_dir, &Options{..})`(`src/verify.rs`)이다. batch(`src/engine.rs`)와 preflight(`src/cmd/preflight.rs`)가 같은 함수를 쓰고, 단계별 `timeout`, `failed` 패턴, 실패 시 한 번 더 돌기(`rerun_on_exit_failure`), `serialize` 잠금, PC 단위 동시 검증 상한(`pc_limit`), 프로세스 그룹 정리가 여기 있다. 검증 폴더는 `Gitops`의 슬롯(`batch`, `preflight-<k>`, `revert`, `post`)이다.
- chute의 머지는 `gh pr merge`가 아니다. 슬롯에서 `<기본 브랜치>`와 PR HEAD SHA를 `merge --no-ff`(GitHub 머지 메시지)로 합쳐 검증하고, 그 커밋을 `git push <원격> <커밋>:refs/heads/<기본 브랜치>`로 올린다(`Slot::merge`, `Slot::push`). 강제 push가 아니라 기본 브랜치가 그 사이 움직였으면 거절되고 `PushOutcome::MainMoved`가 된다. GitHub는 PR HEAD가 기본 브랜치에 들어오면 PR을 머지됨으로 표시한다. PR HEAD는 `refs/pull/<n>/head`를 가져와 등록한 SHA와 비교한다(`Slot::fetch_pr_head`).
- 같은 트리의 통과 기록은 `state.json`의 `verified_trees`(트리 SHA + `reuse::fingerprint` 설정 지문 + 건너뛴 단계)에 남고 `reuse_hours` 안에서 다시 쓴다. `by`는 `preflight`·`batch` 둘뿐이고 `chute status`·`chute events`가 그 값을 화면에 낸다.
- 원격 선택(`repo::discover`)은 작업 폴더 `chute.toml`의 `remote` 키를 읽는다. 원격이 여럿이고 키가 없으면 키를 적으라고 멈춘다.
- `chute init`의 시험은 `tests/cli.rs`의 `init_*` 일곱 개다. `init_leaves_agents_md_alone`이 "chute는 AGENTS.md를 고치지 않는다"를 고정한다.
- `docs/plans/chute-v1.md`는 "템플릿과 `AGENTS.md`는 고치지 않고 넣을 한 줄을 출력한다", "`chute init`은 사람의 일"로 정했다. README "처음 쓰기" 4번과 `skill/manual.md` "에이전트가 하지 않는 것"이 같은 뜻을 적는다.

## 2. 확정된 결정

요청서와 오케스트레이터가 덧붙인 수정(사용자 승인)에서 온 것이다. 덧붙인 수정이 원래 요청보다 우선한다.

- `chute init --setup`이 다섯 단계를 한 실행에서 한다. 1) 검증 명령 받기, 2) 설정 검사, 3) 실제 검증, 4) setup PR, 5) chute가 직접 머지.
- 검증 명령은 `--verify <명령>`(여러 번)으로 받아 `[[verify]]`를 채운다. `--verify`가 없으면 TTY 여부와 관계없이 초안을 쓰고 "`--verify`를 주거나 초안의 `[[verify]]`를 고친 뒤 다시 실행한다"고 안내하고 멈춘다. 고친 초안이 있으면 `--verify` 없이도 그 초안으로 간다. `$EDITOR`는 열지 않는다.
- 검증 명령 짐작은 지금의 세 가지에서 늘리지 않는다(사용자 결정: 무엇을 검사할지는 사용자가 정한다).
- 2단계는 config-check를 자동으로 돌리고, 초안의 `FALLBACK_VERIFY`가 남아 있으면 멈춘다.
- 3단계는 chute의 기존 검증 실행부(`verify::run`)와 슬롯을 그대로 쓴다. 새 실행 코드를 만들지 않는다. setup에서 통과하면 큐에서도 같은 조건으로 통과하게 하려는 것이다.
- 4단계 diff 확인과 5단계 직접 머지는 TTY에서 사람이 확인해야 진행한다. TTY가 아니면 `--yes`가 있어야만 4·5단계로 가고, 없으면 diff를 출력하고 "`--yes`로 다시 실행한다"고 안내하고 멈춘다.
- 지침 파일은 `--instructions <파일>`로 고를 수 있다. 출력에 어느 파일인지 밝힌다.
- 5단계 머지 조건(모두 만족할 때만): 기본 브랜치에 아직 `chute.toml`이 없다. PR이 `chute.toml`과 지침 한 줄만 바꾼다. 3단계를 통과했다. 3단계 뒤 기본 브랜치가 바뀌지 않았다(바뀌었으면 다시 검증한다). 확인한 PR HEAD로만 머지한다.
- 재실행은 멱등이다. 끝난 단계는 건너뛰고, 열린 setup PR이 있으면 새로 만들지 않고 그 PR로 계속한다.
- setup은 한 PC에서 한 번 한다. 기본 브랜치에 이미 `chute.toml`이 있으면 `--setup`은 지금의 `chute init`처럼 등록만 하고 그 사실을 알린다. 다른 PC는 그 뒤 `chute init`(또는 `--setup`)으로 등록만 한다.
- 옵션 없는 `chute init`의 동작은 바꾸지 않는다.
- `chute init --setup`은 사람 명령이다.
- 범위 밖: 짐작 확대, `chute install` 자동 실행, `chute up` 자동 실행.

## 3. 스스로 정한 결정과 이유

### 흐름

한 번의 실행은 다음 순서로 간다. 각 단계는 "이미 끝났는지"를 저장소와 GitHub의 지금 상태에서 다시 판정하므로, 어느 단계에서 멈췄든 같은 명령을 다시 실행하면 이어 간다.

| 순서 | 하는 일 | 끝났다고 보는 조건(재실행 때 건너뜀) | 멈추는 경우 |
|---|---|---|---|
| 0 | `repo::discover`로 저장소를 찾고 등록한다(`chute init`과 같은 출력). 기본 브랜치를 가져온다 | 등록은 늘 한다(멱등) | `<원격>/<기본 브랜치>`에 `chute.toml`이 있으면 "기본 브랜치에 chute.toml이 이미 있어 등록만 했다"를 출력하고 종료 코드 0으로 끝난다 |
| 1 | 초안 준비 | 작업 폴더에 `chute.toml`이 있다 | 아래 "검증 명령 입력" |
| 2 | config-check와 `FALLBACK_VERIFY` 검사 | 없음(빠르다) | 설정 오류, `run`이 `FALLBACK_VERIFY`인 단계 |
| 3 | 슬롯에 setup 커밋을 만들고 그 트리를 검증한다 | 같은 기본 브랜치 SHA, 같은 트리, 같은 설정 지문의 통과 기록이 있다 | 검증 실패 |
| 4 | diff를 보이고 확인받는다. setup 브랜치를 push하고 PR을 만든다 | 열린 setup PR이 있고 그 HEAD의 내용이 지금 만들 내용과 같다 | TTY에서 거절, TTY가 아닌데 `--yes`가 없음 |
| 5 | 머지 조건을 다시 확인하고 기본 브랜치에 push한다 | 기본 브랜치에 `chute.toml`이 생겼다(0번에서 끝난다) | 조건 위반 |

종료 코드는 기존 규칙을 따른다. 사람이 무언가를 해야 하는 멈춤(초안 확인, 설정 오류, 검증 실패, 확인 거절, `--yes` 없음, 머지 조건 위반)은 `REFUSED`(1), 실행할 수 없는 오류(git·gh 실패 등)는 `CANNOT_RUN`(2)이다.

### 결정

| 결정 | 이유 |
|---|---|
| **5단계 머지는 큐와 같은 방식으로 한다.** 슬롯에서 `merge --no-ff <기본 브랜치> <setup 커밋 SHA>`로 머지 커밋을 만들고, 그 트리가 검증한 트리와 같을 때만 `Slot::push`로 기본 브랜치에 올린다. `gh pr merge --match-head-commit`은 쓰지 않는다 | 요청은 "`--match-head-commit` 상당"이다. `gh pr merge`는 GitHub가 그 순간의 기본 브랜치로 머지 커밋을 새로 만들어, 확인과 머지 사이에 기본 브랜치가 움직이면 검증하지 않은 트리가 들어간다. 큐의 push는 fast-forward만 되므로 "검증 뒤 기본 브랜치가 바뀌지 않았다"를 git이 원자적으로 보장하고, 들어가는 트리가 검증한 트리와 정확히 같다. PR HEAD는 `refs/pull/<n>/head`를 가져와 확인한 SHA와 비교한다(`Slot::fetch_pr_head`). 머지 메시지도 큐와 같아 기록이 한 모양이다. guard 훅이 막는 `gh pr merge`를 chute 자신도 쓰지 않게 된다 |
| push가 `MainMoved`면 처음(0번)부터 다시 판정한다. 한 실행에서 세 번까지 하고, 넘으면 "기본 브랜치가 계속 바뀐다. 잠시 뒤 다시 실행한다"로 멈춘다 | 기본 브랜치가 움직였으면 그 사이 `chute.toml`이 들어왔을 수도 있고 트리가 달라져 다시 검증해야 한다. 0번부터 다시 보면 조건 확인을 한 곳에서 한다. 무한히 돌지 않게 상한을 둔다 |
| setup 커밋은 사용자의 작업 폴더가 아니라 preflight 슬롯(`lock_preflight_slot`)에서 만든다. 부모는 방금 가져온 기본 브랜치 끝이고, 내용은 작업 폴더의 `chute.toml`과, 기본 브랜치의 지침 파일에 `AGENTS_LINE` 한 줄을 붙인 것이다 | 사용자의 브랜치, index, 지침 파일을 건드리지 않는다. preflight 슬롯은 에이전트의 preflight와 같은 검증 폴더라 "슬롯 또는 같은 방식"을 그대로 지키고, 새 슬롯 종류를 만들지 않아 gc·`SlotName::parse`를 고칠 필요가 없다. 기본 브랜치에 `chute.toml`이 없으니 슬롯 수 상한은 새 설정의 `max_parallel`을 쓴다 |
| 3단계는 setup 커밋의 트리, 곧 머지로 들어갈 트리 그대로를 검증한다(지침 한 줄 포함) | 요청의 "기본 브랜치 + 새 `chute.toml`"에 지침 한 줄이 더해진다. 지침 파일을 읽는 시험(이 저장소의 `tests/docs.rs` 같은)이 있을 수 있어, 들어갈 트리와 검증한 트리를 같게 둔다 |
| 3단계 `Options`는 preflight·batch와 같은 값(`cache_dir`, `on_step`, `on_machine_wait`, 나머지는 `Options::default()`)이고 `changed`만 `None`이라 `paths`가 있는 단계도 모두 돈다 | setup의 목적은 틀린 명령을 미리 잡는 것이다. `paths`로 건너뛰면 그 단계의 명령은 확인되지 않은 채 큐에 들어간다. 시간 제한, `failed`, 재시도, 잠금은 같은 함수라 같다 |
| `[[post_verify]]`는 돌지 않는다. config-check가 모양만 본다 | 머지 뒤 검사는 batch를 멈추지 않는다(실패하면 `post_failed` 알림). setup을 그만큼 늦출 이유가 작다. `env_check`도 기록하지 않는다. 통과 기록을 다른 PC와 나누지 않기 때문이다 |
| 3단계 통과 기록은 상태 폴더의 `setup.json`에 남긴다. 필드는 기본 브랜치 SHA, 트리 SHA, `reuse::fingerprint`, 시각이다. 임시 파일에 쓴 뒤 이름을 바꾼다. 재실행은 세 값이 모두 같고 새 설정의 `reuse_verified`가 켜져 있고 `reuse_hours` 안일 때만 다시 쓴다 | 긴 검증(펌웨어 빌드 등)을 PR 만들기나 네트워크 실패 때문에 다시 돌리지 않는다. `state.json`의 `verified_trees`에 넣으면 `VerifiedBy`에 새 값이 생겨 `chute status`·`chute events`·현황판의 출력 뜻이 바뀌고, setup 머지는 batch를 거치지 않아 큐가 그 기록을 쓸 일도 없다. 그래서 상태 JSON을 바꾸지 않는 별도 파일로 둔다. 유효 기간은 큐의 재사용 규칙과 같게 한다 |
| 검증 로그는 gc가 이미 지우는 로그 폴더(preflight 로그와 같은 자리)에 쓴다 | 새 폴더를 만들면 gc가 모른다. 실패하면 단계 이름, 로그 경로, 로그 끝을 출력한다 |
| **검증 명령 입력**: `--verify`가 있고 작업 폴더에 `chute.toml`이 없으면 그 명령들로 초안을 쓴다. 하나면 단계 이름은 `test`(지금 초안과 같다), 여럿이면 `test-1`, `test-2`, … 순서는 준 순서다. 나머지 키는 `config::draft`와 같다 | 지금 초안과 모양을 맞춘다. 이름은 겹치면 안 되므로 번호를 붙인다 |
| `--verify`가 있는데 작업 폴더에 `chute.toml`이 이미 있으면, 그 `[[verify]]`의 `run` 목록이 `--verify` 목록과 순서까지 같을 때만 그대로 간다. 다르면 파일을 고치지 않고 "초안의 `[[verify]]`가 `--verify`와 다르다. 초안을 고치거나 지운 뒤 다시 실행한다"로 멈춘다 | 사용자가 손본 초안을 몰래 덮어쓰지 않는다. 같은 명령으로 다시 실행하면 그대로 이어 가므로 멱등이다 |
| `--verify`가 없으면: 작업 폴더에 `chute.toml`이 없을 때는 지금처럼 초안을 쓰고(짐작 포함) 멈춘다. 이번 실행 전부터 있던 `chute.toml`이면 그 초안으로 간다 | "고친 초안이면 간다"를 판정할 방법은 "이 실행이 쓴 것이 아니다"뿐이다. 짐작한 `cargo test`가 맞더라도 사용자가 한 번 보고 다시 실행하게 한다(사용자 결정: 무엇을 검사할지는 사용자가 정한다) |
| 2단계는 `config_check`의 검사 함수를 그대로 부르고(`pub(crate)`로 연다), 그 위에 `run`이 `FALLBACK_VERIFY`와 같은 단계를 오류로 더한다. 오류 문구는 단계 이름과 "검사 명령으로 바꾼다"를 담는다 | "config-check를 자동 실행"을 문자 그대로 지킨다. `FALLBACK_VERIFY`는 일반 설정에서는 올바른 값이라 config-check 자체는 바꾸지 않는다 |
| **지침 파일 고르기**: `--instructions <파일>`이 있으면 그 파일. 없으면 기본 브랜치에 `AGENTS.md`가 있으면 `AGENTS.md`, 없고 `CLAUDE.md`가 있으면 `CLAUDE.md`, 둘 다 없으면 새 `AGENTS.md`. TTY여도 묻지 않는다. 확인 단계와 결과 출력에 "지침 파일: <파일>"을 적는다 | 요청의 기본값(`AGENTS.md`)을 따르되, `CLAUDE.md`만 쓰는 저장소에 쓰지 않는 `AGENTS.md`를 새로 만들지 않는다. TTY와 아닐 때 같은 규칙이면 시험할 수 있고 놀랄 일이 없다. 다른 파일은 `--instructions`로 고르고, 확인 단계에서 파일 이름을 보고 거절할 수 있다 |
| `--instructions`는 저장소 안의 상대 경로만 받는다. 절대 경로, `..`가 드는 경로, `chute.toml`, 기본 브랜치에서 심볼릭 링크인 파일은 거절한다 | 저장소 밖에 쓰지 않는다. `CLAUDE.md -> AGENTS.md` 같은 링크에 내용을 쓰면 링크가 일반 파일로 바뀌므로 대상 파일을 고르라고 안내한다 |
| 지침 파일 내용 = 기본 브랜치의 내용(끝에 줄바꿈이 없으면 붙인다) + `AGENTS_LINE` + 줄바꿈. 파일이 없으면 `AGENTS_LINE` + 줄바꿈. 같은 줄이 이미 있으면 지침 파일은 바꾸지 않는다(PR은 `chute.toml`만 바꾼다) | 내용을 한 함수로 정해 두면 5단계의 "지침 한 줄만" 확인이 같은 함수의 결과와 PR HEAD의 파일을 바이트로 비교하는 일이 된다 |
| setup 브랜치 이름은 `chute/setup`. 커밋 메시지와 PR 제목은 `chute: 머지 큐 설정 추가`, 본문은 `chute init --setup`이 만들었다는 것과 검증한 단계 목록. 작성자는 그 저장소의 git 설정을 따른다 | 대상 저장소의 커밋 규칙은 알 수 없어 접두어 없는 한국어 한 줄로 둔다. 브랜치 이름이 정해져 있어야 재실행이 PR을 찾는다 |
| **열린 PR 찾기**: `gh pr list --head chute/setup --base <기본 브랜치> --state open`으로 찾는다. 있고 그 HEAD의 `chute.toml`과 지침 파일이 지금 만들 내용과 같으면 그 HEAD를 setup 커밋으로 쓰고 브랜치를 다시 push하지 않는다. 내용이 다르면 새 setup 커밋을 만들어 확인받은 뒤 `--force-with-lease=refs/heads/chute/setup:<그 HEAD>`로 push하고 같은 PR을 쓴다. 열린 PR이 없으면 같은 lease 규칙으로 push하고 `gh pr create`한다 | 재실행에서 PR을 새로 만들지 않는다(요청). 초안을 고친 뒤의 재실행도 같은 PR에 이어진다. lease는 그 사이 다른 실행이 올린 커밋을 덮지 않게 한다. 닫힌(머지 안 된) PR은 열린 PR이 아니라 새 PR을 만든다 |
| **5단계 조건 확인은 push 직전에 한 함수에서 한다.** 기본 브랜치를 다시 가져와서: ① 그 끝에 `chute.toml`이 없다, ② `gh pr view`로 PR이 열려 있고 base가 기본 브랜치이고 head가 같은 저장소의 `chute/setup`이다, ③ `refs/pull/<n>/head`가 확인한 setup 커밋 SHA와 같다, ④ 기본 브랜치와 그 커밋의 merge-base 대비 바뀐 파일이 `chute.toml`(새 파일)과 지침 파일뿐이고 각 내용이 위 함수의 결과와 같다, ⑤ 머지 커밋의 트리가 `setup.json`의 통과 트리와 같고 기본 브랜치 SHA도 같다. ①이 깨지면 0번의 "등록만"으로 끝나고, ⑤가 깨지면 다시 검증하고, 나머지는 머지하지 않고 이유를 출력하고 멈춘다 | 이 경로는 chute가 큐를 거치지 않고 기본 브랜치에 쓰는 유일한 경로다. 조건마다 시험을 하나씩 둬 고정한다 |
| 확인은 한 번이다. 3단계가 끝난 뒤(또는 통과 기록을 찾은 뒤) diff(`git diff <기본 브랜치> <setup 커밋>`), 지침 파일 이름, PR 번호(있으면), "setup 브랜치를 push하고 PR을 만들어 기본 브랜치에 바로 머지한다"를 보이고 `[y/N]`을 묻는다. TTY 판정은 stdin과 stdout이 모두 터미널일 때이고, 판정과 답 읽기는 시험이 주입할 수 있게 한다 | PR 만들기와 머지를 따로 물으면 PR만 열린 채 멈추는 상태가 생긴다. 사람이 한 번 보고 한 번 동의한다. TTY는 시험에서 만들 수 없어 판정을 주입한다 |
| 머지에 성공하면 작업 폴더의 `chute.toml`이 머지된 것과 바이트까지 같을 때 지운다. 다르면 남기고 그 사실을 알린다. 그리고 "`git pull`로 받는다"와 다음 할 일(`chute install`, 첫 `chute submit`이 데몬을 띄운다)을 출력한다 | 추적되지 않은 같은 이름의 파일이 남으면 `git pull`·`git checkout`이 "untracked working tree files would be overwritten"으로 멈춘다. 내용이 같으면 지워도 잃는 것이 없다 |
| 원격이 여럿인 저장소는 지금처럼 `repo::discover`가 정한다. 키가 필요하면 멈추고, 사용자가 초안에 `remote = "..."`를 적고 다시 실행하면 그 키가 `chute.toml`에 그대로 커밋돼 큐도 같은 원격을 쓴다 | 원격 선택 규칙을 setup만 다르게 두지 않는다. 초안의 키가 그대로 기본 브랜치의 설정이 된다 |
| `--setup`은 허브로 넘기지 않는다. 그 PC에서 돈다 | 기본 브랜치에 `chute.toml`이 없으면 큐도 허브도 아직 쓰이지 않는다. 확정된 결정대로 한 PC에서 한 번 하고, 다른 PC는 등록만 한다 |
| `--verify`, `--instructions`, `--yes`는 `--setup`과 함께만 받는다(clap `requires`) | 옵션 없는 `chute init`의 동작을 바꾸지 않는다 |
| 지침 파일 원칙을 바꾼다: "chute는 지침 파일을 몰래 고치지 않는다. `chute init --setup`만 diff를 보여 동의(TTY의 `y` 또는 `--yes`)를 받은 뒤 setup PR에 한 줄을 넣는다." README "처음 쓰기"와 `skill/manual.md`의 해당 문장을 고친다. 옵션 없는 `chute init`의 "chute는 AGENTS.md를 고치지 않는다" 출력은 그대로 둔다 | 요청이 이 원칙을 바꾼다. 옵션 없는 `init`은 여전히 고치지 않으므로 그 출력과 `init_leaves_agents_md_alone`은 맞다 |
| `docs/plans/chute-v1.md`는 고치지 않는다. 이 플랜이 원칙의 바뀐 부분을 기록한다 | 요청은 "설계 문서의 관련 문장도 고친다"고 했지만 `AGENTS.md`가 "플랜 문서는 고치지 않는다"고 정한다. 설계 문서는 `docs/plans/`의 플랜뿐이라 저장소 규칙을 따르고 이 차이를 적는다 |
| `skill/manual.md`에서 `chute init --setup`은 사람 명령으로 둔다. "에이전트는 사용자가 명시적으로 시킬 때만 `--yes`와 함께 실행한다"를 한 줄 적는다. guard 훅은 바꾸지 않는다 | TTY가 아닐 때 `--yes`로 가는 길은 사용자가 에이전트에게 맡기는 경우를 위한 것이다(오케스트레이터 수정 1). 막지 않되 스스로 판단해 쓰지 않게 한다. 막을지는 6절에서 사용자가 정할 수 있다 |
| 두 setup이 동시에 돌 때를 위한 별도 잠금은 두지 않는다 | 슬롯 잠금, 브랜치 lease, fast-forward push가 서로 덮는 것을 이미 막는다. 진 쪽은 오류나 `MainMoved`로 멈추고 다시 실행하면 0번에서 "등록만"으로 끝난다 |

### 작업 나누기

| 작업 ID | 내용 | 방식 | 방식의 이유 |
|---|---|---|---|
| `setup-verify` | `--setup`(숨김), `--verify`, `--instructions`. 0~3번 단계: 등록, 초안, config-check와 `FALLBACK_VERIFY`, 슬롯의 setup 커밋, `verify::run`, `setup.json`. 끝에 diff를 출력하고 멈춘다 | `poteto` | 검증 판정을 쓰는 코드, 상태 파일(`setup.json`), 슬롯 잠금, `gitops`·`config`·`cmd`에 걸친 새 설계다 |
| `setup-merge` | 4~5번 단계: 확인(TTY 주입, `--yes`), setup 브랜치 push와 PR 만들기·다시 쓰기, 머지 조건 확인, 기본 브랜치 push와 `MainMoved` 재시도, 작업 폴더 정리, `--setup` 숨김 해제. README "처음 쓰기"와 `skill/manual.md` | `poteto` | main에 무엇이 들어가는지 정하는 코드(머지, push)이고 큐를 거치지 않는 유일한 머지 경로다 |

`setup-merge`는 `setup-verify` 뒤에 순차로 둔다(`after`). 같은 함수(`--setup` 실행부)의 끝을 다시 짜고, 3단계가 만든 setup 커밋과 `setup.json`을 그대로 쓰기 때문이다. 앞 작업이 머지되는 동안 `--setup`은 도움말에 숨겨 반쪽 기능이 사용자에게 보이지 않게 한다. 바깥으로 나가는 일(push, PR, 머지)은 모두 뒤 작업에 있어 앞 작업만 머지돼도 원격에는 아무것도 쓰지 않는다.

## 4. 범위 밖

- 검증 명령 짐작 확대, `$EDITOR`로 초안 열기.
- `chute install`, `chute up` 자동 실행.
- 기본 브랜치에 이미 `chute.toml`이 있는 저장소의 설정 변경. 그때는 보통의 PR로 큐에 넣는다.
- `[[post_verify]]` 단계를 setup에서 돌리는 일.
- guard 훅이 `chute init --setup`을 막는 일(6절).
- 빈 저장소(기본 브랜치에 커밋이 없는 원격), GitHub가 아닌 원격.
- `chute status`·현황판에 setup 진행을 보이는 일. 상태 JSON은 바꾸지 않는다.

## 5. 위험

- GitHub가 PR을 머지됨으로 표시하는 것은 HEAD 커밋이 기본 브랜치에 들어온 것을 GitHub가 알아챘을 때다. 큐의 머지가 이미 이 방식이라 새로운 위험은 아니다.
- 대상 저장소에 기본 브랜치 보호 규칙(PR 필수, 리뷰 필수)이 있으면 `Slot::push`가 거절된다. 큐도 같은 조건에서 머지하지 못하므로 setup이 먼저 드러내는 셈이다. 거절 문구를 그대로 보인다.

## 6. 사용자가 정해야 하는 결정

- **검증을 약하게 하는 작업은 없다.** setup은 `paths`로 건너뛰지 않고 모든 `[[verify]]` 단계를 돌아 큐의 batch보다 넓게 검사한다. 머지도 `gh pr merge`보다 엄격한 큐 방식이다(3절 첫 결정). 다만 `[[post_verify]]`는 setup에서 돌지 않는다.
- **에이전트의 `--setup` 실행을 막을지.** 이 플랜은 막지 않고 설명서에 "사용자가 시킬 때만"을 적는다. 막기를 원하면 guard 훅에 `chute init --setup … --yes`를 더하는 작은 후속 작업이 된다.
- **5단계 머지를 `gh pr merge --match-head-commit`이 아니라 큐와 같은 push로 하는 것**(3절 첫 결정). 요청의 "상당"을 이렇게 읽었다. GitHub의 머지 버튼과 같은 기록(GitHub가 만든 머지 커밋)이 꼭 필요하면 알려 준다.

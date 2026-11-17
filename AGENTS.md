# chute 저장소 규칙

chute는 herdr 에이전트의 PR을 묶어 로컬에서 검증하고 기본 브랜치에 머지하는 머지 큐다. 설계는 `docs/plans/chute-v1.md`에 있다. 플랜 문서는 고치지 않는다. 플랜과 다르게 해야 하면 PR과 보고서에 적는다.

- 이 저장소의 PR은 chute로 머지한다. PR을 올리기 전에 `chute --skill`을 실행해 나온 절차를 따르고, `gh pr merge`나 기본 브랜치로의 직접 push는 하지 않는다.

## 검사

PR을 올리기 전에 네 명령이 모두 통과해야 한다. 저장소 루트의 `chute.toml`에도 같은 명령이 있다.

```sh
cargo fmt --check
node --test mod/*/*.test.js
cargo clippy --all-targets -- -D warnings
cargo test
```

## 코드 규칙

- `git`, `gh`, `hc`는 `src/ext.rs`의 `Ext`를 거쳐서만 실행한다. 다른 곳에서 `std::process::Command`로 이 셋을 부르지 않는다.
- 큐 항목의 상태(`ItemState`)를 바꾸는 코드는 `src/queue.rs`에만 둔다. 다른 모듈은 `queue`의 함수를 부른다.
- `state.json`은 `RepoStore::update`로만 고친다. 잠금을 잡고 임시 파일에 쓴 뒤 이름을 바꾼다.
- 명령마다 인자와 실행 함수는 `src/cmd/<명령>.rs`에 둔다. `src/cli.rs`에는 하위 명령 목록만 둔다.
- 상태 JSON(`chute status --json`)의 필드 이름이나 뜻을 바꾸면 `tests/fixtures/status-full.json`과 `mod/chute/`를 같은 PR에서 고친다. 필드를 더하는 것은 되지만 있는 필드의 이름과 뜻은 바꾸지 않는다.
- 사용자에게 보이는 글(오류와 출력 메시지, 도움말, 설명서, 보고서)은 한국어로 쓴다. 코드, 식별자, 코드 주석은 영어로 쓴다.
- `mod/chute/`의 화면 문구는 영어로 쓴다(2026-11-17 사용자 결정).

## 시험

시험은 사용자의 환경에 닿지 않는다.

시험을 쓰거나 고치기 전에 `docs/testing.md`를 읽는다.

- `CHUTE_HOME`과 `HOME`은 임시 폴더다. 단위 시험은 `crate::testkit`, 통합 시험은 `mod support;`(`tests/support/mod.rs`)의 `Sandbox`를 쓴다.
- `gh`와 `hc`는 `Sandbox::fake`가 만드는 가짜다. 원격은 `TestRepo`가 만드는 로컬 bare 저장소다.
- 실제 hc 코어, Herdr 창, 사용자의 홈 폴더, GitHub에 쓰지 않는다.
- 시험에서 프로세스 환경 변수를 바꾸지 않는다(`std::env::set_var` 금지). 환경은 `Ext::from_vars`, `Home::from_vars`, `Sandbox::command`로 넘긴다.
- 잠금의 의미를 보는 시험은 `tests/locks.rs`에 둔다. 그 시험 바이너리에서는 프로세스를 띄우지 않는다. 다른 시험 스레드가 fork한 자식은 exec하기 전까지 열린 잠금 파일을 함께 쥐기 때문이다.
- 명령의 시험은 그 명령 파일(`src/cmd/<명령>.rs`)에 둔다. 여러 명령을 한 목록에 모은 시험을 만들지 않는다. 뒤 작업들이 같은 줄을 고치게 된다.

## 커밋

메시지는 `<type>: <한국어 요약>` 형식이다. 예: `chore: 저장소 초기화`, `feat: 큐 상태 전이 추가`. type은 `feat`, `fix`, `docs`, `refactor`, `test`, `chore` 가운데 하나다.

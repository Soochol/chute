# chute

chute는 herdr 에이전트들이 올리는 PR을 받아 묶음(batch)으로 로컬에서 검증하고, 통과한 것을 기본 브랜치에 머지하는 머지 큐다. 실패하거나 충돌한 PR은 원인과 함께 그 PR을 만든 에이전트에게 돌려보내고, 사람이 풀어야 하는 것만 사람에게 남긴다. 설계는 [`docs/plans/chute-v1.md`](docs/plans/chute-v1.md)에 있다.

## 빌드

```sh
cargo build --release
cargo test
```

실행 파일은 `target/release/chute`다.

#!/bin/sh
if command -v chute >/dev/null 2>&1; then
  exec chute guard
fi
top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
[ -f "$top/chute.toml" ] || exit 0
echo "chute: 실행 파일이 PATH에 없어 기본 브랜치 push와 gh pr merge를 막지 못한다. 'cargo install --git https://github.com/Soochol/chute'로 설치한다." >&2
exit 1

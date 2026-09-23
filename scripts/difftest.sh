#!/usr/bin/env bash
#
# 진짜 Redis(정답지)와 우리 구현의 응답을 한 줄씩 비교합니다.
#
#   ./scripts/difftest.sh                  # tests/cases/ 전체 실행
#   ./scripts/difftest.sh 01               # 이름에 01 이 들어간 케이스만 (01-ping.txt)
#   ./scripts/difftest.sh ping             # 이름에 ping 이 들어간 케이스만
#   ./scripts/difftest.sh tests/cases/01-ping.txt   # 경로를 직접 줘도 됩니다
#
# 케이스 파일 규칙
#   - 한 줄에 커맨드 하나. 인자는 공백으로 구분 (따옴표는 지원하지 않습니다)
#   - `#` 으로 시작하는 줄과 빈 줄은 무시
#   - `SLEEP 0.5` 는 양쪽 서버에 보내지 않고 그냥 대기합니다 (TTL 테스트용)

set -uo pipefail

REF_PORT="${REF_PORT:-6379}"
OUR_PORT="${OUR_PORT:-6380}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

ref_pid=""
our_pid=""

cleanup() {
  [ -n "$our_pid" ] && kill "$our_pid" 2>/dev/null
  [ -n "$ref_pid" ] && kill "$ref_pid" 2>/dev/null
  return 0
}
trap cleanup EXIT

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "❌ '$1' 이(가) 없습니다. brew install redis 를 실행하세요."; exit 1; }
}
need redis-cli
# redis-server 는 우리가 직접 정답지를 띄워야 할 때만 필요합니다.
# 이미 떠 있는 Redis(예: CI의 서비스 컨테이너)를 쓸 때는 redis-cli 만 있으면 됩니다.

wait_for_port() {
  local port="$1" name="$2" i
  for i in $(seq 1 60); do
    if redis-cli -p "$port" PING >/dev/null 2>&1; then return 0; fi
    sleep 0.25
  done
  echo "❌ $name 서버(포트 $port)가 뜨지 않았습니다."
  return 1
}

# --- 정답지 Redis 준비 ---------------------------------------------------
if redis-cli -p "$REF_PORT" PING >/dev/null 2>&1; then
  echo "▶ 이미 떠 있는 Redis를 정답지로 사용합니다 (포트 $REF_PORT)"
else
  need redis-server
  echo "▶ 정답지 Redis를 띄웁니다 (포트 $REF_PORT)"
  redis-server --port "$REF_PORT" --save '' --appendonly no --daemonize no >/dev/null 2>&1 &
  ref_pid=$!
  wait_for_port "$REF_PORT" "정답지 Redis" || exit 1
fi

# --- 우리 서버 준비 ------------------------------------------------------
echo "▶ 우리 서버를 빌드합니다"
if ! cargo build --manifest-path "$ROOT/Cargo.toml" 2>&1 | tail -5; then
  echo "❌ 빌드 실패"
  exit 1
fi

echo "▶ 우리 서버를 띄웁니다 (포트 $OUR_PORT)"
"$ROOT/target/debug/redis-clone" --port "$OUR_PORT" >/dev/null 2>&1 &
our_pid=$!
wait_for_port "$OUR_PORT" "우리" || {
  echo "   힌트: cargo run -- --port $OUR_PORT 가 동작해야 합니다. (CLAUDE.md의 '서버 규약' 참고)"
  exit 1
}

# --- 비교 실행 -----------------------------------------------------------
# 인자는 파일 경로여도 되고, 케이스 이름의 일부여도 됩니다. (예: 01, ping, 01-ping)
resolve_case() {
  local given="$1" matches=()
  if [ -f "$given" ]; then
    echo "$given"
    return 0
  fi
  for candidate in "$ROOT"/tests/cases/*"$given"*.txt; do
    [ -f "$candidate" ] && matches+=("$candidate")
  done
  if [ ${#matches[@]} -eq 0 ]; then
    echo "❌ '$given' 에 맞는 케이스 파일이 없습니다. 있는 것:" >&2
    for candidate in "$ROOT"/tests/cases/*.txt; do
      [ -f "$candidate" ] && echo "   - $(basename "$candidate")" >&2
    done
    return 1
  fi
  printf '%s\n' "${matches[@]}"
}

files=()
if [ $# -eq 0 ]; then
  files=("$ROOT"/tests/cases/*.txt)
else
  for given in "$@"; do
    while IFS= read -r resolved; do
      files+=("$resolved")
    done < <(resolve_case "$given") || exit 1
  done
fi

if [ ${#files[@]} -eq 0 ]; then
  echo "❌ 실행할 케이스 파일이 없습니다."
  exit 1
fi

pass=0
fail=0

for file in "${files[@]}"; do
  [ -f "$file" ] || continue
  echo ""
  echo "=== $(basename "$file") ==="

  redis-cli -p "$REF_PORT" FLUSHALL >/dev/null 2>&1
  redis-cli -p "$OUR_PORT" FLUSHALL >/dev/null 2>&1

  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      ''|'#'*) continue ;;
      'SLEEP '*) sleep "${line#SLEEP }"; continue ;;
    esac

    # shellcheck disable=SC2086
    expected="$(redis-cli -p "$REF_PORT" $line 2>&1)"
    # shellcheck disable=SC2086
    actual="$(redis-cli -p "$OUR_PORT" $line 2>&1)"

    if [ "$expected" = "$actual" ]; then
      pass=$((pass + 1))
      echo "  ✅ $line"
    else
      fail=$((fail + 1))
      echo "  ❌ $line"
      echo "       기대: $(echo "$expected" | tr '\n' '|')"
      echo "       실제: $(echo "$actual" | tr '\n' '|')"
    fi
  done < "$file"
done

echo ""
echo "------------------------------"
echo "통과 $pass / 실패 $fail"
[ "$fail" -eq 0 ] || exit 1

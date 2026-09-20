# redis-clone

Rust로 Redis를 바닥부터 만들어보는 3인 스터디 저장소입니다.
Rust를 몰라도 AI와 함께 구현하면서, **설계 결정과 그 근거를 남기는 것**을 목표로 합니다.

## 원칙 3개

1. **테스트가 먼저 있어야 합니다.** Rust를 모르면 코드만 보고 맞는지 판단할 수 없습니다.
   진짜 Redis를 정답지로 두고 응답을 비교합니다. (`scripts/difftest.sh`)
2. **한 세션에 기능 하나만.** "Redis 만들어줘" 한 방이면 아무것도 남지 않습니다.
3. **Rust는 코드에 등장한 것만 그때그때 배웁니다.** 사전 문법 공부는 하지 않습니다.

## 세션 진행 루프 (2~3시간)

| 순서 | 내용 | 시간 |
| --- | --- | --- |
| 1 | 이번에 만들 커맨드 스펙을 redis.io에서 같이 읽기 | 10분 |
| 2 | `tests/cases/`에 비교 테스트 케이스 먼저 추가 | 20분 |
| 3 | 한 명이 운전하면서 AI로 구현 (운전자는 매 세션 교대) | 60분 |
| 4 | **코드 읽기 — "이 줄 왜 있음?" 질문 최소 3개** | 30분 |
| 5 | 이번에 나온 Rust 개념 1~2개 정리해서 PR 본문에 기록 | 15분 |

4번을 빼면 스터디가 아니라 구경이 됩니다.

## 로드맵

| 세션 | 만들 것 | 백엔드에서 배우는 것 | 같이 배울 Rust 개념 |
| --- | --- | --- | --- |
| 1 | TCP 서버 + `PING` | accept 루프, 커넥션 단위 처리 | `Result`와 `?`, `loop`, 소유권 첫 만남 |
| 2 | RESP 파서 + `ECHO` + 파이프라인 | 길이 기반 프로토콜, 파이프라이닝 | `enum` + `match`, `&[u8]`와 `Vec<u8>` |
| 3 | `SET`/`GET`/`DEL`/`EXISTS` | 공유 저장소, cache-aside 패턴 | `HashMap`, `Arc<Mutex>`, `.clone()`의 의미 |
| 4 | TTL — `EXPIRE`/`TTL`/`PERSIST`, `SET NX EX` | 만료 전략(lazy vs active), 분산 락 | `Instant`/`Duration`, 백그라운드 태스크 |
| 5 | `INCR`/`DECR`/`INCRBY` | 원자성, rate limiter, 카운터 | 정수 파싱, `checked_add` |
| 6 | List — `LPUSH`/`LPOP`/`LRANGE` + `WRONGTYPE` | 작업 큐의 자료구조 | 데이터를 담는 `enum`, `VecDeque` |
| 7 | `BLPOP`/`BRPOP` (블로킹) | 클라이언트를 재우고 깨우기, 폴링과의 차이 | 채널, `tokio::select!`, `Notify` |
| 8 | Hash — `HSET`/`HGETALL`/`HINCRBY` | 세션 저장, 부분 갱신 | 중첩 자료구조의 소유권 |
| 9 | `MULTI`/`EXEC`/`DISCARD`/`WATCH` | 낙관적 락(CAS), 롤백이 없는 이유 | 커넥션별 상태, 키 버전 추적 |
| 10 | `KEYS` vs `SCAN` + `TYPE`/`DBSIZE` | O(N) 명령이 서버를 멈추는 이유, 커서 순회 | 반복자, 패턴 매칭 직접 구현 |

Set / Sorted Set / Pub/Sub / 영속성은 10세션을 끝낸 뒤 확장으로 다룹니다.
세션별 상세(실무 사용 예, 설계 토론 항목, 완료 기준)는 [docs/backend-roadmap.md](docs/backend-roadmap.md)에 있습니다.

세션마다 GitHub Issue가 하나씩 있습니다. 설계 토론은 그 Issue에서 합니다.

## 시작하기

```bash
# 준비물
brew install redis          # 정답지로 쓸 진짜 Redis
rustup update               # Rust 1.98 이상

# 실행
cargo run -- --port 6380
redis-cli -p 6380 PING

# 진짜 Redis와 응답 비교
./scripts/difftest.sh
```

> 초기 상태에서는 `difftest.sh`가 실패합니다. **이걸 초록색으로 만드는 게 세션 1의 목표입니다.**

## 협업 규칙

- `main` 직접 커밋 금지. 브랜치 → PR → **다른 두 명이 리뷰**한 뒤 머지합니다.
- 브랜치 이름: `<이름>/s03-set-get`
- 리뷰어는 승인하기 전에 **"이 줄 왜 있음?" 질문을 최소 2개** 답니다.
  모르는 코드를 승인하면 그 세션은 날아간 겁니다.
- 설계 갈림길에서 나온 결정은 해당 세션 Issue에 기록합니다.

## 리뷰 체크리스트 (Rust 초보용)

AI가 Rust에서 자주 저지르는 것들입니다. 코드를 읽을 때 이것부터 확인하세요.

- [ ] `.clone()`이 늘었다면, 없으면 왜 안 되는지 설명할 수 있나?
- [ ] `unwrap()` / `expect()`가 프로덕션 코드에 있나? (입력 하나로 서버가 죽습니다)
- [ ] 소켓에서 데이터가 **쪼개져서** 들어오는 경우를 처리하나? (`read` 한 번으로 끝내지 않았나)
- [ ] `Mutex` 락을 잡은 채로 `.await`하지 않나?
- [ ] `Cargo.toml`에 몰래 추가된 의존성이 있나? (RESP 파서 crate는 금지)
- [ ] 시키지 않은 커맨드를 같이 구현하지 않았나?

## 서버가 죽지 않는지 확인하는 법

```bash
# 쓰레기 바이트를 던져도 서버는 살아있어야 합니다
printf 'garbage\r\n' | nc localhost 6380
redis-cli -p 6380 PING        # 여전히 PONG 이어야 함

# 명령을 쪼개서 보내도 처리되어야 합니다
exec 3<>/dev/tcp/localhost/6380; printf '*1\r\n$4\r\n' >&3; sleep 1; printf 'PING\r\n' >&3; head -c 7 <&3
```

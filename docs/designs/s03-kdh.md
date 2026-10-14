# S03 SET / GET / DEL / EXISTS — kdh

- 구현 브랜치: `kdh/s03-set-get` (`kdh/s02-echo` 위에서 시작)
- 걸린 시간: <!-- 직접 채움 -->

## 1. 무엇을 만들었나

커맨드 4개보다 **여러 커넥션이 저장소 하나를 같이 쓰는 것**이 본론입니다.

| 항목 | 내용 |
| --- | --- |
| `SET key value` | 저장하고 `+OK`. 있던 값은 덮어씁니다. 뒤에 인자가 더 붙으면 `ERR syntax error` (옵션은 S04) |
| `GET key` | 있으면 Bulk String, 없으면 Null Bulk String(`$-1`) |
| `DEL key [key ...]` | 실제로 지운 개수. `DEL k k` → `:1` |
| `EXISTS key [key ...]` | 있는 키의 개수. 같은 키를 여러 번 쓰면 그만큼 셉니다. `EXISTS k k` → `:2` |
| 공유 저장소 | 커넥션 A가 `SET`한 값을 커넥션 B가 바로 `GET`으로 봅니다 |

- 스펙: https://redis.io/docs/latest/commands/set/ · https://redis.io/docs/latest/commands/get/ · https://redis.io/docs/latest/commands/del/ · https://redis.io/docs/latest/commands/exists/
- 정답지 Redis: 8.10.2

## 2. 설계 선택

| 갈림길 | 고른 것 | 이유 | 버린 것 |
| --- | --- | --- | --- |
| 동시성 모델 (토론 주제) | `Arc<Mutex<HashMap>>` | 코드가 제일 적고, 로드맵이 S03 Rust 개념으로 찍은 `Arc`/`Mutex`/`.clone()`이 그대로 나옵니다. S01에서 "S7에서 막혀 보는 경험을 먼저 한다"고 고른 것과도 같은 방향입니다 | **키 해시 샤딩**: `DEL a b`처럼 키가 다른 샤드로 갈리면 락을 여러 개, 정해진 순서로 잡아야 합니다(아니면 데드락). S09 `EXEC`, S10 `SCAN`에서도 샤드 경계 비용이 생깁니다. 이득(락 경합 감소)은 스터디 부하에서 체감되지 않습니다<br>**actor + 채널**: S07이 제일 자연스럽고 진짜 Redis 구조와 같지만, 저장소 전담 스레드 + 요청마다 응답 채널로 지금 코드가 제일 많습니다 |
| 키/값 타입 (토론 주제) | `Vec<u8>` | 파서가 이미 `Vec<u8>`을 넘겨줘서 변환이 없습니다. Redis 값은 바이너리 세이프라 UTF-8이 아닌 바이트도 그대로 들어가야 합니다 | `String`: 변환 실패를 처리해야 하고, UTF-8이 아닌 값은 Redis와 다르게 동작합니다 |
| `SET k v` 뒤에 남는 인자 | `ERR syntax error` | 옵션이 하나도 없는 지금은 "모르는 옵션"으로 보는 게 Redis와 같은 의미입니다. S04는 그중 일부를 "아는 옵션"으로 바꾸기만 하면 됩니다 | `wrong number of arguments`: Redis와 문구가 다릅니다<br>무시하고 `OK`: `EX 10`을 조용히 버려서 TTL이 안 걸린 걸 모르게 됩니다 |
| 락이 poison 됐을 때 | `PoisonError::into_inner`로 계속 씀 | 락 안에서 하는 일이 `insert`/`remove` 한 번이라 반쯤 바뀐 상태가 남지 않습니다 | `-ERR` 응답: 한 번 poison 되면 그 뒤 모든 커넥션이 저장소를 영영 못 써서 서버가 죽은 것과 같습니다 |
| 커맨드 구조 | `enum Command` + `parse` / `execute` | S02 문서에 "6개가 되면 간다"고 적어 둔 조건입니다. 인자 개수 검사(`parse`)를 락 밖에서 끝내고, `execute`는 실패하지 않습니다. 기능과 섞지 않으려고 리팩터링 커밋을 따로 두었습니다 | 이름별 `if` 체인 유지: 커맨드마다 개수 검사와 저장소 접근이 한 덩어리로 섞입니다 |
| 커맨드 에러 타입 | 직접 만든 `CommandError` enum | S02 `ProtocolError`와 같은 방식입니다. 에러 종류가 응답 문구와 1:1입니다 | 응답 바이트(`Vec<u8>`)를 바로 돌려주기: 더 짧지만 S02와 방식이 갈립니다 |
| 인자를 넘기는 방식 | `build_reply`가 명령을 값(`Vec<Vec<u8>>`)으로 받음 | `SET`이 키와 값을 **복사 없이** 저장소로 옮길 수 있습니다 | 빌림(`&[Vec<u8>]`): 저장할 때 `.clone()`이 필요합니다 |
| `GET` 응답을 만드는 위치 | 락을 잡은 채 바로 응답 바이트로 씀 | 값을 저장소 밖으로 꺼내려면 한 번 복사(clone)해야 합니다. 어차피 응답 바이트로 한 번은 복사하므로 그 한 번으로 끝냅니다 | 값을 clone해서 꺼낸 뒤 락 밖에서 응답 만들기 |
| 락 잡는 범위 | 커맨드 하나 동안만. 소켓 쓰기는 락 밖 | 느린 클라이언트 하나가 락을 붙잡고 있지 않습니다. `DEL a b c`는 락 한 번 안에서 끝나서 원자적입니다 | 커넥션 단위나 read 한 번 단위로 잡기 |

**뒤집을 조건**
- S07 `BLPOP`: 락을 잡은 채로 기다릴 수 없습니다. `Condvar`나 대기자 목록이 필요하고, 그게 꼬이면 actor + 채널로 바꿉니다
- `redis-benchmark`에서 락 경합이 병목으로 보이면 샤딩을 다시 봅니다
- 큰 값 `GET`: 락을 잡은 채 복사하므로 값이 아주 크면 그동안 다른 커넥션이 기다립니다
- S04 active expiration: 백그라운드 스레드가 같은 락을 잡으므로, 한 번에 오래 훑지 않게 나눠야 합니다
- S05/S06: 값 타입이 여러 개가 되면 `HashMap<Vec<u8>, Value>`로 감쌉니다 (바이트는 그대로)

## 3. AI를 어떻게 썼나

- **첫 프롬프트:** S03 브리핑을 먼저 받은 뒤, "S02 기반으로 S03 다 진행되도록 구현해 달라"고 했습니다
- **AI가 정한 것:** 토론 갈림길 4개(동시성, 키/값 타입, SET 추가 인자, poison)는 AI가 추천과 근거를 낸 것을 제가 그대로 골랐습니다. 커맨드 에러 타입, 인자 넘기는 방식, `GET` 응답 위치, 락 범위는 AI가 구현하면서 정했습니다
- **AI 제안 중 거절한 것:** <!-- 직접 채움 -->
- **AI가 틀린 것 / 스펙과 달랐던 것:** <!-- 직접 채움 -->
- **이해하지 못한 채 넘어간 코드:** <!-- 직접 채움. 예: `<[Vec<u8>; 2]>::try_from`, `MutexGuard`의 수명 -->
- **AI 없이 결정한 것:** <!-- 직접 채움 -->

## 4. 내가 추가한 테스트 케이스

`tests/cases/03-set-get.txt`

```
SET name hello
GET name
set name world
GET name
GET nope
GET NAME
SET 한글 값
GET 한글
EXISTS name
EXISTS nope
EXISTS name name nope
DEL name
GET name
DEL name
SET a 1
SET b 2
DEL a b c
EXISTS a b
SET k v
DEL k k
SET
SET k
GET
GET k extra
DEL
EXISTS
SET k v foo
PING
ECHO hello
```

- **줄마다 새 커넥션:** difftest는 줄마다 `redis-cli`를 새로 띄웁니다. 그래서 `SET` 다음 줄의 `GET`이 맞으면 커넥션끼리 저장소를 같이 쓴다는 뜻이고, 완료 기준이 자동으로 검사됩니다
- **`EXISTS name name nope`, `DEL k k`:** 같은 키를 여러 번 쓰면 EXISTS는 그만큼 세고 DEL은 한 번만 셉니다
- **`SET k v foo`:** 인자가 남으면 개수 에러가 아니라 syntax error입니다
- **`GET NAME`, `SET 한글 값`:** 키는 대소문자를 가리고, 바이트 그대로 저장됩니다
- **넣었다가 깨진 것:** 구현 전 S02 서버로 돌리면 2 / 29만 통과했습니다 (PING, ECHO)

**difftest가 못 잡는 것 — 단위 테스트와 raw 소켓으로 따로 확인했습니다**

difftest는 바이트가 아니라 `redis-cli`가 찍은 문자열을 비교합니다. 파이프로 받으면 타입 표시가 빠집니다.

| 응답 | redis-cli 출력 |
| --- | --- |
| `$-1` (없는 키) | 빈 줄 |
| `$0\r\n\r\n` (빈 문자열) | 빈 줄 |
| `:1` (정수) | `1` |
| `$1\r\n1\r\n` (문자열 "1") | `1` |

그래서 완료 기준인 Null Bulk String을 difftest로는 확인할 수 없습니다. `src/command.rs` 단위 테스트에 바이트를 박아 두었습니다.

## 5. 막혔던 곳

- **difftest 초록불이 바이트 일치를 보장하지 않음:** 위 표 참고. 브리핑 때 실측으로 확인했습니다
- <!-- 직접 채움 -->

## 6. 새로 배운 Rust 개념

- `Arc`: 여러 스레드가 같이 소유하는 포인터. `Arc::clone`은 안의 HashMap을 복사하지 않고 "가리키는 쪽 개수"만 올립니다
- `Mutex` / `MutexGuard`: `lock()`이 돌려준 guard가 스코프를 벗어나면 락이 저절로 풀립니다. 패닉한 스레드가 잡고 있었으면 `lock()`이 `Err`(poison)를 돌려줍니다
- `<[T; N]>::try_from(vec)`: Vec 길이가 N일 때만 배열로 바꿔 원소를 옮깁니다. 실패하면 원래 Vec을 돌려줍니다

## 7. 검증 결과

```
$ cargo fmt --check                          # 통과
$ cargo clippy --all-targets -- -D warnings  # 통과
$ cargo test                                 # 21 passed
$ ./scripts/difftest.sh                      # 통과 49 / 실패 0
raw 소켓 비교 (진짜 Redis 8.10.2)            # 7개 중 6개 같음. 다른 1개는 의도한 차이 (아래)
redis-benchmark -t set,get -n 100000 -c 50   # SET 22.3만 / GET 22.2만 rps (release), 끝난 뒤 PING 응답
```

raw 소켓으로 확인한 것

- 커넥션 둘을 **동시에 열어 둔 채로** A가 `SET` → B가 `GET` → B가 `DEL` → A가 `GET`(`$-1`)
- 한 번에 묶어 보낸 바이트: nil과 빈 문자열, 정수, `\xff\x00k`처럼 `\r\n`이 든 바이너리 키/값, 인라인 `SET`/`GET`
- 커넥션 20개가 동시에 서로 다른 키 4000개를 `SET` → `EXISTS`가 `:4000` (빠진 쓰기 없음)
- **다른 1개:** `SET k v EX 10`이 Redis는 `+OK`, 우리는 `-ERR syntax error`. 옵션은 S04 범위라 의도한 차이입니다

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- **남의 케이스로 내 서버를 돌려본 결과:**

## 9. 알려진 차이와 한계

- `SET` 옵션(`EX`/`PX`/`NX`/`XX`/`GET`/`KEEPTTL`)이 전부 syntax error입니다 (S04)
- `FLUSHALL`이 없습니다. difftest는 파일마다 `FLUSHALL`을 보내지만 우리 서버는 무시하므로, 키를 만드는 케이스 파일이 늘면 앞 파일의 키가 뒤 파일에 남습니다
- 큰 값의 `GET`은 복사하는 동안 락을 잡고 있습니다
- S02 문서 9번의 한계(관대한 숫자 파싱, 모르는 커맨드 문구의 CRLF 등)는 그대로입니다

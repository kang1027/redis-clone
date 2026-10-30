# S05 INCR / DECR / INCRBY — kdh

- 구현 브랜치: `kdh/s05-incr` (main `8ed9a83` 위에서 시작). main에 `6d0d167`로 반영했습니다
- 걸린 시간: 구현과 검증 약 3분 (10/30 23:12 요청 → 23:15 push). 그 전에 브리핑과 진짜 Redis 실측에 20분쯤 썼습니다 (22:06~22:27)

## 1. 무엇을 만들었나

커맨드 3개보다 **읽기, 더하기, 쓰기가 왜 서버 안에서 한 번에 끝나야 하는가**가 본론입니다.

| 항목 | 내용 |
| --- | --- |
| `INCR key` | 1을 더합니다. 없는 키는 0에서 시작합니다 |
| `DECR key` | 1을 뺍니다. 없는 키는 0에서 시작합니다 |
| `INCRBY key n` | `n`을 더합니다. 음수도 받습니다. `INCRBY z 0`도 키를 만듭니다 |
| 숫자가 아닐 때 | `ERR value is not an integer or out of range`. 값과 TTL은 그대로이고, 없던 키도 안 생깁니다 |
| 넘칠 때 | `ERR increment or decrement would overflow`. 문구가 위와 다르고, 값은 그대로입니다 |
| TTL | 값만 바꾸므로 그대로입니다. 만료된 키는 없는 키로 보고 0에서 시작하며 TTL도 없습니다 |

- 스펙: https://redis.io/docs/latest/commands/incr/ · https://redis.io/docs/latest/commands/decr/ · https://redis.io/docs/latest/commands/incrby/
- 정답지 Redis: 8.10.2

**빼기로 한 것:** `DECRBY`, `INCRBYFLOAT`(로드맵의 3개에 없음), `WRONGTYPE`(다른 타입이 S06에 생김), `APPEND`, `STRLEN`, `OBJECT ENCODING`(비교 실측에만 썼음), `MULTI`(S09)

## 2. 설계 선택

| 갈림길 | 고른 것 | 이유 | 버린 것 |
| --- | --- | --- | --- |
| 값을 어떻게 저장할지 (토론 주제) | `Vec<u8>` 유지, `INCR` 때마다 파싱하고 다시 바이트로 씀 | `SET k 010` 뒤 `GET`이 `010`이어야 합니다. 바이트를 그대로 두면 이런 경우를 신경 쓸 필요가 없습니다. 고치는 곳도 `INCR` 갈래 하나입니다 | **`enum Value { Str, Int }`**(진짜 Redis 쪽): `SET`이 정수인지 판별하고 `GET`이 다시 글자로 만들어야 합니다. 진짜 Redis도 `OBJECT ENCODING`이 `10`은 `int`, `010`은 `embstr`입니다. 응답 바이트는 같아서 메모리 최적화에 가깝습니다 |
| 실패를 `execute`에서 어떻게 다룰지 | `execute`는 그대로 `Vec<u8>`을 돌려주고, `Result`를 돌려주는 `incr_by` helper를 둠 | 저장된 값을 봐야 아는 에러는 처음입니다. 이번에 실패하는 건 `INCR` 계열뿐이라 한 갈래만 바뀝니다 | **`execute`가 `Result`를 돌려줌**: S06에는 편해지지만 지금은 모든 갈래를 고치는 리팩터링입니다 |
| `INCR`, `DECR`을 어떻게 만들지 | `Command::IncrBy { key, delta }` 하나로 묶음 (`delta`가 1, -1) | 진짜 Redis도 같은 함수로 처리합니다. 계산과 에러가 한 곳에 있습니다 | 커맨드마다 갈래: 같은 코드가 세 번 나옵니다 |
| `INCRBY` 인자를 언제 볼지 | `parse`에서, 락 잡기 전에 | 진짜 Redis도 값보다 인자를 먼저 봅니다. S06에서 `INCRBY 리스트키 abc`는 `ERR`, `INCRBY 리스트키 1`은 `WRONGTYPE`이라 순서가 그대로 맞습니다 | `execute`에서 보기: 락을 잡은 채로 인자 에러가 납니다 |
| 정수 판별 | S04의 `parse_integer` 재사용 | `+10`, `010`, `-0`을 거부하는 규칙이 값에도 똑같이 적용됩니다 (실측 확인) | `str::parse` 그대로: `+10`을 받아 줍니다 |

위쪽 2개는 AI 추천을 그대로 받았고, 아래쪽 3개는 AI가 구현하면서 정했습니다. 반박 환영입니다.

**뒤집을 조건**
- 값 저장: S06에서 어차피 `enum Value`가 생기니 그때 다시 봅니다. `INCR` 벤치에서 파싱 비용이 실제로 보여도 뒤집습니다. 지금은 진짜 Redis와 차이가 작습니다 (7번)
- `execute`의 실패: S06에서 `WRONGTYPE`을 내는 커맨드가 여러 개가 되면 `Result`로 바꿉니다. 이때는 리팩터링 커밋을 따로 둡니다
- S09: `INCR`과 `EXPIRE` 사이에 앱이 죽는 문제는 `MULTI`에서 다시 나옵니다

## 3. AI를 어떻게 썼나

- **첫 프롬프트:** "s05 내용정리좀"으로 브리핑부터 받았습니다. AI는 진짜 Redis 8.10.2에 직접 보내 동작을 뽑고, 앱에서 `GET` → `+1` → `SET`을 할 때 값을 얼마나 잃는지도 쟀습니다. 그다음 "S05 가벼운거같은데 바로 구현해서 올려라잉"이라고 했습니다
- **AI가 정한 것:** 갈림길 2개에 추천을 냈고, 따로 반박하지 않아 추천대로 갔습니다. 2번 표의 아래쪽 3개도 AI가 정했습니다
- **AI 제안 중 거절한 것:** 없습니다
- **AI가 틀린 것:** 이번 구현에서는 고친 것이 없습니다. 대신 원자성 테스트가 진짜 잡는지 확인했습니다. 복사본에서 락을 둘로 쪼갰더니 8000이 나와야 할 값이 3126이 되며 테스트가 깨졌습니다
- **이해하지 못한 채 넘어간 코드:** 아직 코드를 직접 읽지 않았습니다. 발표 전에 따라가 볼 곳입니다
  - `incr_by`의 `let Some(entry) = entries.live(...) else { entries.insert(...) }`: `else` 안에서 `entries`를 또 빌려도 되는 이유
  - `incr_by(&mut entries, ...)`: `MutexGuard`를 `&mut Entries` 자리에 넘겨도 되는 이유
  - 테스트의 `std::thread::scope`: S03의 `Arc` 없이 `&db`를 스레드에 빌려줄 수 있는 이유
- **AI 없이 결정한 것:** S05 바로 진행, PR 없이 main에 올리기

## 4. 내가 추가한 테스트 케이스

`tests/cases/05-incr.txt` (주석 뺀 것)

```
INCR n
INCR n
GET n
DECR d
INCRBY b 5
INCRBY b -7
GET b
INCRBY z 0
EXISTS z
SET k 10
INCR k
DECR k
INCRBY k 100
GET k
incr k
iNcRbY k 2
SET k abc
INCR k
DECR k
INCRBY k 1
GET k
SET k 1.5
INCR k
SET k +10
INCR k
GET k
SET k 010
INCR k
GET k
SET k -0
INCR k
SET k 0x10
INCR k
SET k 1e3
INCR k
SET k 9223372036854775808
INCR k
SET k 9223372036854775807
INCR k
GET k
SET k -9223372036854775808
DECR k
GET k
INCR k
SET k 1
INCRBY k 9223372036854775807
GET k
SET k -1
INCRBY k -9223372036854775808
INCRBY k 9223372036854775807
SET k 0
INCRBY k -9223372036854775808
GET k
INCRBY k -1
SET k 1
INCRBY k abc
INCRBY k 1.5
INCRBY k +1
INCRBY k 010
INCRBY k -0
INCRBY k 9223372036854775808
INCRBY k -9223372036854775809
GET k
INCRBY nope abc
EXISTS nope
INCR
INCR a b
DECR
DECR a b
INCRBY
INCRBY a
INCRBY a 1 2
INCRBY a abc 2
SET t 1 EX 100
INCR t
TTL t
DECR t
TTL t
INCRBY t 5
TTL t
SET t abc EX 100
INCR t
TTL t
SET t 1 PX 100
SLEEP 0.3
INCR t
TTL t
INCR rl
EXPIRE rl 60
INCR rl
TTL rl
PING
ECHO hello
GET t
DBSIZE
DEL n d b z k t rl
DBSIZE
```

- **`INCRBY z 0` → `EXISTS z`:** 아무것도 안 더해도 키가 생기는지
- **`SET k 010` → `INCR k` → `GET k`:** 정수처럼 보여도 거부하고, 값도 안 바뀌는지. 값 저장 방식을 고른 근거입니다
- **`INCRBY k -9223372036854775808`:** 값이 `-1`이면 넘치고 `0`이면 안 넘칩니다. 최소값 끝을 찌릅니다
- **`INCRBY a abc 2`:** 숫자가 틀려도 개수 에러가 먼저입니다
- **`SET t abc EX 100` → `INCR t` → `TTL t`:** 실패해도 TTL이 살아 있는지
- **`SET t 1 PX 100` → `SLEEP 0.3` → `INCR t` → `TTL t`:** 만료된 키의 값과 TTL이 둘 다 사라지는지
- **`INCR rl` → `EXPIRE rl 60` → `INCR rl` → `TTL rl`:** 로드맵의 rate limiter 그대로입니다
- **마지막 DBSIZE 2번:** 실패한 명령이 키를 남기지 않았는지. FLUSHALL이 없어서 끝에 `DEL`도 넣었습니다
- **넣었다가 깨진 것:** 구현 전에는 05 케이스가 통과 33 / 실패 63이었습니다. 구현 뒤에 깨진 케이스는 없습니다

**difftest가 못 잡는 것은 단위 테스트에 바이트로 박아 두었습니다**
- 정수 `:2`와 문자열 `$1\r\n2`는 redis-cli에서 똑같이 `2`로 보입니다
- 줄을 공백으로 쪼개서 보내므로 빈 값, `" 10"`, UTF-8이 아닌 값은 케이스 파일에 못 넣습니다
- 원자성: difftest는 한 번에 하나씩만 보냅니다. 스레드 8개가 1000번씩 `INCR`해서 8000인지 봅니다

## 5. 막혔던 곳

없었습니다. S04의 `Entry`, `live`, `parse_integer`, `CommandError::NotInteger`가 그대로 들어맞았습니다. 값 옆에 TTL을 둔 덕분에 `entry.value`만 바꾸면 TTL이 유지됩니다.

그나마 생각할 곳은 `src/command.rs`의 "`execute`는 실패하지 않습니다" 주석이었습니다. S03부터 지킨 규칙이 저장된 값을 봐야 하는 에러에서 처음 깨져서, 주석과 구조를 같이 고쳤습니다.

## 6. 새로 배운 Rust 개념

- `checked_add`: 넘치면 패닉하거나 값이 돌아가는 대신 `None`을 돌려줍니다. `ok_or`로 바로 에러로 바꿉니다
- `Result`를 돌려주는 helper와 `?`: 실패하는 곳마다 `match`를 쓰지 않고 첫 에러에서 바로 빠져나옵니다
- `to_string().into_bytes()`: 정수를 글자로 바꾸고, `String`을 복사 없이 `Vec<u8>`로 넘깁니다

## 7. 검증 결과

```
$ cargo fmt --check                          # 통과
$ cargo clippy --all-targets -- -D warnings  # 통과
$ cargo test                                 # 38 passed
$ ./scripts/difftest.sh                      # 통과 220 / 실패 0
raw 소켓 비교 (진짜 Redis 8.10.2)            # 119개 모두 같음
redis-benchmark -t incr -n 100000 -c 50      # 우리 23.8만 / Redis 25.6만 rps (release), 끝난 뒤 PING 응답
```

- **바이트 비교:** 정수 응답, 정수가 아닌 값 13종, 오버플로 경계, 인자 에러, 개수 에러, TTL 유지, 만료 뒤 `INCR`
- **금지 규칙:** 프로덕션 코드에 `unwrap()`, `expect(`, `unsafe`, `.clone()` 0개
- **lost update:** 카운터를 1만 번 올린 결과입니다 (release, 한 번 잰 값이라 돌릴 때마다 달라집니다)

| 클라이언트 × 반복 | 앱에서 `GET` → `+1` → `SET` (우리 / Redis) | `INCR` (우리 / Redis) |
| --- | --- | --- |
| 2 × 5000 | 5066 / 5211 | 10000 / 10000 |
| 10 × 1000 | 1728 / 1752 | 10000 / 10000 |
| 50 × 200 | 435 / 451 | 10000 / 10000 |

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

10/30 기준으로 한호택 포크에 `hht/s05-incr`, `hht/s05-design`이 있고, kms 포크에는 아직 없습니다. 미리 보면 내 선택이 흔들려서 아직 안 읽었습니다.

- 나와 제일 다른 지점:
- 물어볼 것:
- **남의 케이스로 내 서버를 돌려본 결과:**

## 9. 알려진 차이와 한계

- `WRONGTYPE`이 없습니다. 값 타입이 하나뿐이라 이번에는 나올 일이 없습니다
- `DECRBY`, `INCRBYFLOAT`이 없습니다
- `OBJECT ENCODING`이 없고, 값은 정수여도 바이트로 저장합니다. `INCR`마다 파싱하고 할당합니다
- rate limiter에서 `INCR`과 `EXPIRE` 사이에 앱이 죽으면 TTL 없는 키가 남습니다. 서버 구현이 아니라 쓰는 쪽 문제이고, S09 `MULTI`에서 다시 봅니다

# S04 TTL — EXPIRE / TTL / PERSIST / SET 옵션 — kdh

- 구현 브랜치: `kdh/s04-ttl` (main `73a0152` 위에서 시작)
- 걸린 시간: 구현 약 10분 (10/18 16:45 요청 → 16:55 커밋 4개), 검증 약 5분. 그 전에 브리핑과 진짜 Redis 실측에 40분쯤 썼습니다 (15:59~16:40)

## 1. 무엇을 만들었나

커맨드 5개보다 **키에 수명을 붙이고, 수명이 다한 키를 언제 지우는가**가 본론입니다.

| 항목 | 내용 |
| --- | --- |
| `SET key value [NX \| XX] [EX s \| PX ms]` | `EX`/`PX`로 TTL을 겁니다. `NX`는 없을 때만, `XX`는 있을 때만 쓰고, 못 쓰면 nil(`$-1`). 옵션 없이 SET하면 걸려 있던 TTL이 지워집니다 |
| `EXPIRE key seconds` | 있는 키면 `:1`, 없으면 `:0`. 0이나 음수면 TTL 대신 키를 바로 지웁니다 |
| `TTL key` | 남은 초를 **반올림**해서 돌려줍니다. TTL 없는 키는 `-1`, 없는 키는 `-2` |
| `PERSIST key` | TTL을 지웠으면 `:1`, 지울 게 없었으면 `:0` |
| `DBSIZE` | 키 개수. 만료됐지만 아직 안 지워진 키도 셉니다 (S10에서 당겨옴) |
| 만료된 키 지우기 | 읽을 때 지우고(lazy), 100ms마다 전체를 훑어서도 지웁니다(active) |

- 스펙: https://redis.io/docs/latest/commands/set/ · https://redis.io/docs/latest/commands/expire/ · https://redis.io/docs/latest/commands/ttl/ · https://redis.io/docs/latest/commands/persist/ · https://redis.io/docs/latest/commands/dbsize/
- 정답지 Redis: 8.10.2

## 2. 설계 선택

| 갈림길 | 고른 것 | 이유 | 버린 것 |
| --- | --- | --- | --- |
| 만료 시각을 어디 둘지 (토론 주제) | 값 옆: `Entry { value, expires_at: Option<Instant> }` | 키를 지우면 TTL도 같이 사라져서 둘이 어긋날 일이 구조적으로 없습니다 | **별도 맵**(진짜 Redis 방식): DEL이나 덮어쓰기 때마다 두 맵을 같이 지워야 합니다. 하나 빼먹으면 지웠다 다시 SET한 키가 옛날 TTL로 사라집니다. 대신 active가 TTL 있는 키만 볼 수 있습니다 |
| active expiration을 이번에 할지 (토론 주제) | lazy + 100ms마다 전체를 `retain`으로 훑기 | 완료 기준(DBSIZE에서도 결국 사라짐)이 요구합니다. 우리 규모에서는 전체를 훑어도 충분했습니다 (7번 실측) | **lazy만**: 아무도 안 읽는 키가 메모리에 계속 남습니다. 진짜 Redis에서 active를 끄고 재 보면 1초 뒤에도 DBSIZE가 그대로입니다<br>**Redis식 샘플링**(20개 뽑고 25% 넘게 만료면 반복): std HashMap에는 무작위로 뽑는 기능이 없어서 자료구조가 더 필요합니다 |
| 시간에 따라 바뀌는 동작을 어떻게 테스트할지 (토론 주제) | `build_reply`가 `now: Instant`를 인자로 받음 | 테스트에서 `start + 200초`를 넘기면 sleep 없이 "200초 뒤"가 됩니다. 단위 테스트 31개가 0.00초에 끝납니다 | 진짜 시간 + sleep: 느리고, 타이밍 때문에 가끔 깨집니다 |
| DBSIZE 범위 | S04로 당겨옴. 03 케이스 끝에 `DEL 한글` 추가 | 완료 기준에 DBSIZE가 적혀 있고, active 동작을 진짜 Redis와 비교할 방법이 이것뿐입니다. S10에는 KEYS/SCAN/TYPE이 남습니다 | 안 만들기: active를 단위 테스트로만 봐야 합니다 |
| 만료 확인을 빼먹지 않게 | `Entries`를 struct로 감싸 맵을 숨기고, 키 읽기는 `live()` 하나로만 | 키를 읽는 커맨드(GET, EXISTS, DEL, TTL, PERSIST, EXPIRE, SET NX/XX)가 만료 확인을 빼먹을 수 없습니다. 맵을 직접 만지면 컴파일이 안 됩니다 | `HashMap`을 그대로 두고 커맨드마다 만료 확인: 하나만 빼먹어도 만료된 키가 보입니다 |
| 숫자와 시각을 계산하는 곳 | `parse`에서 `Instant`까지 계산 | S03처럼 `execute`는 실패하지 않습니다. 에러는 전부 락을 잡기 전에 납니다 | `execute`에서 계산: 락을 잡은 채로 에러가 날 수 있습니다 |
| 정수 파싱 | `parse` 뒤에 다시 글자로 바꿔 입력과 같은지 비교 | 진짜 Redis처럼 `+10`, `010`, `-0`을 거부합니다 | `str::parse` 그대로: `+10`을 받아 줍니다 (S02 `resp.rs`의 길이 파싱이 이 상태) |
| `EXPIRE k 0` 이하 | 만료 시각을 `now`로 두고 `execute`에서 바로 지움 | 진짜 Redis도 "이미 지난 시각"이면 지우고 `:1`입니다 | 음수 시각 계산: `Instant`는 음수가 없어서 다루기 어렵습니다 |
| 시각을 재는 곳 | 커넥션에서 명령마다 `Instant::now()` 한 번 | 진짜 Redis도 명령 하나 안에서는 같은 시각을 씁니다 | 커맨드 안에서 필요할 때마다 재기 |

아래쪽 5개는 구현하면서 정했습니다. 반박 환영입니다.

**뒤집을 조건**
- 키가 많아서 active 한 번 훑는 동안 다른 커넥션이 눈에 띄게 멈출 때. 실측으로 GET 최악 응답이 키 0개 0.8ms, 10만 개 6.2ms, 100만 개 14ms였습니다. 문제가 되면 TTL 있는 키만 따로 모아 일부만 뽑아 보는 방식으로 바꾸고, 그때 별도 맵이 다시 후보가 됩니다
- S05 `INCR`: 진짜 Redis는 값만 바꾸고 TTL은 그대로 둡니다. `Entry.value`만 바꾸면 됩니다
- S06: `Entry.value`가 `enum Value`가 됩니다. 만료 처리는 그대로입니다
- S07 `BLPOP`: 이제 락을 잡는 쪽이 커넥션 스레드들 + 백그라운드 스레드입니다
- S10 `KEYS`/`SCAN`: 만료된 키를 빼고 보여 주려면 역시 `live`를 거쳐야 합니다

## 3. AI를 어떻게 썼나

- **첫 프롬프트:** "S04는 뭐 어때"라고 물어서 브리핑부터 받았습니다. AI는 진짜 Redis 8.10.2에 명령을 직접 보내 응답을 뽑고, 함정 7개를 먼저 정리했습니다. 그다음 "S04도 구현하고, 이해하기 쉽게 발표 자료도 만들어 달라"고 했습니다
- **AI가 정한 것:** 토론 갈림길 4개는 AI가 추천과 근거를 낸 것을 그대로 골랐습니다. 2번 표의 아래쪽 5개는 AI가 구현하면서 정했습니다
- **AI 제안 중 거절한 것:** 없습니다. 이번에도 네 개 다 추천대로 갔습니다
- **AI가 틀린 것:**
  - active 만료가 락을 얼마나 잡는지 처음엔 `PING`으로 쟀습니다. `PING`은 저장소 락을 안 잡아서 아무것도 안 잰 셈이었고, `GET`으로 다시 쟀습니다
  - 케이스 초안에 `PX 5000`, `PX 3000` 키가 있었는데, difftest가 파일 끝의 `DEL`까지 가는 데 몇 초 걸려서 그 사이에 만료될 수 있었습니다. 10배로 늘렸습니다
  - 브리핑 때 실측 스크립트가 zsh에서 인자를 안 쪼개서, 첫 결과가 전부 `unknown command`였습니다
- **이해하지 못한 채 넘어간 코드:** 아직 코드에서 직접 따라가 보지 않았습니다
  - `let (Some(key), Some(value)) = (args.next(), args.next()) else { ... }` (`src/command.rs` SET 갈래)
  - `b"NX" if condition != Some(SetCondition::Xx) => ...`처럼 match에 `if`가 붙은 갈래 (match guard)
  - `Entries::live`가 `get`으로 보고 → `remove` → 다시 `get_mut` 하는 이유 (한 번에 하면 borrow checker가 막는다고 들었습니다)
  - `entry.expires_at.take()`, `(…).then_some(number)`
- **AI 없이 결정한 것:** S04를 S03과 같은 주에 이어서 하기, 구현이 끝나면 발표 자료부터 만들게 한 것

## 4. 내가 추가한 테스트 케이스

`tests/cases/04-ttl.txt` (주석 뺀 것)

```
SET k v
TTL k
TTL nope
SET k v EX 100
TTL k
SET p v PX 50000
TTL p
SET k v
TTL k
EXPIRE k 100
TTL k
EXPIRE nope 100
PERSIST k
PERSIST k
TTL k
PERSIST nope
EXPIRE k 1000
EXPIRE k 50
TTL k
EXPIRE k 0
EXISTS k
SET k v
EXPIRE k -5
GET k
EXPIRE nope -5
SET lock owner1 NX EX 30
SET lock owner2 NX EX 30
GET lock
SET nope v XX
GET nope
SET lock owner3 XX
GET lock
TTL lock
SET k v nx ex 10
TTL k
SET k v EX 10 EX 20
TTL k
SET k v XX PX 30000
TTL k
SET short v PX 100
SLEEP 0.3
GET short
EXISTS short
TTL short
DEL short
SET short again NX
GET short
SET k v NX XX
SET k v EX 10 PX 100
SET k v EX
SET k v EX abc NX XX
SET k v foo
SET k v EX abc
SET k v EX 1.5
SET k v EX +10
SET k v EX 0
SET k v EX -1
SET k v PX 0
EXPIRE k
EXPIRE k abc
EXPIRE k 010
EXPIRE k 9999999999999999
EXPIRE k abc foo
TTL
TTL a b
PERSIST
PERSIST a b
DBSIZE x
DEL k p lock short
SET a v PX 500
SET b v PX 500
SET c v
DBSIZE
SLEEP 1.2
DBSIZE
DEL c
```

- **`SET lock owner1 NX EX 30` → `owner2`는 nil:** 로드맵의 분산 락 예시 그대로입니다
- **`EXPIRE k 0` vs `SET k v EX 0`:** 같은 0인데 EXPIRE는 성공하고 키를 지우고, SET은 에러입니다
- **`SET k v EX abc NX XX`:** 숫자도 틀리고 옵션도 틀리면 syntax error가 먼저입니다
- **`SET k v EX +10`, `EXPIRE k 010`:** Rust `parse`는 받아 주지만 진짜 Redis는 정수로 안 봅니다
- **`SET short v PX 100` → `SLEEP 0.3` → 다섯 가지로 보기:** 만료된 키가 GET만이 아니라 어디서 봐도 없는 키인지
- **마지막 DBSIZE:** 아무도 안 읽은 `a`, `b`가 1.2초 뒤에 사라져 있으면 active가 동작한 것입니다
- **TTL은 걸자마자만 확인:** TTL은 반올림이라, SLEEP 뒤에 찍으면 x.5초 근처에서 두 서버가 갈릴 수 있습니다
- **넣었다가 깨진 것:** 구현 전에는 전체 64 / 60이었습니다(04는 74줄 중 14줄만 통과). lazy 커밋 뒤에는 DBSIZE 3줄만 실패했습니다

**difftest가 못 잡는 것은 단위 테스트에 바이트로 박아 두었습니다**
- `SET NX` 실패의 `$-1`과 빈 문자열은 redis-cli에서 둘 다 빈 줄입니다 (S03 함정 그대로)
- TTL 반올림 경계: 99.990초 → 100, 99.600초 → 100, 99.400초 → 99
- 만료 직후(정확히 100ms 뒤)에 GET, EXISTS, TTL, DEL, PERSIST, EXPIRE, SET XX, SET NX가 전부 "없는 키"로 동작하는지

## 5. 막혔던 곳

- **TTL 반올림:** 브리핑 실측에서 `PTTL` 99989인데 `TTL`이 100이었습니다. 남은 시간을 초로 자르면(`as_secs`) 99가 나와서 difftest가 바로 깨집니다. Redis는 `(밀리초 + 500) / 1000`입니다
- **FLUSHALL이 없어서 03의 키가 04로 넘어옴:** difftest는 파일마다 FLUSHALL을 보내지만 우리 서버엔 없어서 결과가 무시됩니다. 03의 `한글` 키가 남아 04의 DBSIZE가 Redis 0, 우리 1로 깨질 뻔해서, 03 끝에 `DEL 한글`을 넣었습니다
- **difftest를 연달아 돌리면 가끔 전부 실패:** 앞 실행이 띄운 정답지 Redis가 꺼지는 중에 다음 실행이 그걸 "이미 떠 있는 Redis"로 잡고, 곧 연결이 끊깁니다(`Could not connect ... TCP_NODELAY`). 스크립트 쪽 문제라 이번엔 안 고쳤습니다. 한 번씩 돌리면 5번 모두 74 / 74였습니다

## 6. 새로 배운 Rust 개념

- `Instant` / `Duration`: `Instant`는 뒤로 가지 않는 시계의 한 시점입니다. `checked_add`는 넘치면 None을 돌려줘서, 너무 큰 TTL을 `invalid expire time`으로 바꾸기 쉽습니다
- guard의 수명: `db.lock().remove_expired(now)`처럼 한 문장에서 쓰면 guard가 문장 끝에서 사라져 락이 바로 풀립니다. 백그라운드 스레드가 자는 동안에는 락을 안 잡습니다
- `HashMap::retain`, `Option::take`: 조건에 맞는 것만 남기기, 값을 꺼내고 그 자리를 None으로 비우기

## 7. 검증 결과

```
$ cargo fmt --check                          # 통과
$ cargo clippy --all-targets -- -D warnings  # 통과
$ cargo test                                 # 31 passed
$ ./scripts/difftest.sh                      # 통과 124 / 실패 0
raw 소켓 비교 (진짜 Redis 8.10.2)            # 11개 모두 같음
redis-benchmark -t set,get -n 100000 -c 50   # SET 22.1만 / GET 23.6만 rps (release), 끝난 뒤 PING 응답
```

- **바이트 비교:** NX/XX 실패 nil, TTL `-1`/`-2`/`:100`, PERSIST `1`/`0`, `EXPIRE k 0` 뒤 EXISTS `0`, 에러 문구 3종, `\r\n`이 든 옵션 이름, DBSIZE, PX 50 뒤 0.2초에 GET/TTL/EXISTS
- **분산 락:** 커넥션 50개가 동시에 `SET lock <id> NX EX 30`을 3번 → 매번 성공 1개, nil 49개 (Redis와 같음)
- **active 만료가 다른 커넥션을 얼마나 붙잡나:** 키를 넣고 `GET nope`을 쉬지 않고 보내며 응답 시간을 쟀습니다 (release, PX 8000)

| 키 개수 | GET 중간값 | GET 최악 | 1초마다 DBSIZE |
| --- | --- | --- | --- |
| 0 | 0.014ms | 0.8ms | 0 |
| 10만 | 0.014ms | 6.2ms | 100000 … → 0 |
| 100만 | 0.014ms | 14.0ms | 1000000 … → 201215 → 0 |

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- **남의 케이스로 내 서버를 돌려본 결과:**

## 9. 알려진 차이와 한계

- `EXPIRE`의 `NX`/`XX`/`GT`/`LT`는 `ERR Unsupported option NX`로 답합니다. 진짜 Redis는 받습니다
- `SET`의 `GET`/`KEEPTTL`/`EXAT`/`PXAT`는 syntax error입니다. 진짜 Redis는 받습니다
- `PTTL`, `PEXPIRE`, `EXPIREAT`, `EXPIRETIME`, `FLUSHALL`이 없습니다
- i64 밀리초 끝 근처(약 2억 9천만 년)의 TTL: 진짜 Redis는 `SET k v EX 9223372036854775`에서 덧셈이 넘쳐 키가 바로 사라지고(TTL `-2`), `EXPIRE k 9223372036854775`는 `invalid expire time`입니다. 우리는 `Instant`로 나타낼 수 있어서 그냥 받습니다
- active는 전체를 훑습니다. 키 100만 개면 그동안 다른 커넥션이 최대 14ms쯤 기다립니다
- 만료 직후 잠깐은 DBSIZE가 진짜 Redis와 다를 수 있습니다. 양쪽 active 주기가 서로 맞춰져 있지 않기 때문이고, 케이스는 `SLEEP 1.2`로 피합니다
- S02 `resp.rs`의 길이 파싱은 여전히 `+1`, `04`를 받아 줍니다. 이번 정수 파싱은 커맨드 인자에만 썼습니다

# S06 List — kdh

- 구현 브랜치: `kdh/s06-list` (main `c8514ff` 위에서 시작, 로컬). main에 `b2f823e`로 반영했습니다
- 걸린 시간: 약 15분 (11/12 22:11쯤 요청 → 22:25 S06 커밋). S07과 한 번에 진행했고, 주제 이해는 11/9에 만든 구현 전 설명 덱으로 먼저 했습니다

## 1. 무엇을 만들었나

커맨드 6개보다 **저장소의 값이 두 종류가 되는 것**이 본론입니다. 지금까지는 값이 전부 문자열이라 "이 키에 뭐가 들어 있지?"를 물을 일이 없었습니다.

| 항목 | 내용 |
| --- | --- |
| `LPUSH key v [v ...]` / `RPUSH` | 왼쪽/오른쪽 끝에 하나씩 넣고, 넣은 뒤 길이를 답합니다. `LPUSH q a b c`는 c가 맨 앞입니다 |
| `LPOP key` / `RPOP key` | 끝에서 하나 빼서 답합니다. 없는 키는 `$-1`입니다 |
| `LRANGE key start stop` | 음수는 뒤에서 센 번호이고 끝 번호도 포함입니다. 범위 밖은 잘라 내고, 걸리는 게 없으면 `*0`입니다 |
| `LLEN key` | 길이입니다. 없는 키는 0입니다 |
| 타입이 다를 때 | `WRONGTYPE Operation against a key holding the wrong kind of value`. 문자열 키에 리스트 커맨드, 리스트 키에 `GET`/`INCR` 계열 둘 다입니다 |
| 빈 리스트 | 저장하지 않습니다. 마지막 원소를 빼면 키도 사라지고, TTL도 같이 사라집니다 |
| 타입을 안 따지는 커맨드 | `SET`은 리스트를 덮어씁니다. `DEL`, `EXISTS`, `EXPIRE`, `TTL`, `PERSIST`, `DBSIZE`는 코드를 안 고쳤는데 그대로 됩니다 |

- 스펙: https://redis.io/docs/latest/commands/lpush/ · https://redis.io/docs/latest/commands/lpop/ · https://redis.io/docs/latest/commands/lrange/ · https://redis.io/docs/latest/commands/llen/
- 정답지 Redis: 8.10.2

**빼기로 한 것:** `LPOP`/`RPOP`의 개수 인자(`LPOP q 2`), `LINDEX`·`LSET`·`LINSERT`·`LREM`·`LTRIM` 같은 다른 리스트 커맨드, `TYPE`(S10), `OBJECT ENCODING`. 구현 전에 AI가 이 목록을 먼저 내고 확인받았습니다.

## 2. 설계 선택

| 갈림길 | 고른 것 | 이유 | 버린 것 |
| --- | --- | --- | --- |
| 리스트를 무엇으로 만들지 (토론 주제) | `VecDeque<Vec<u8>>` | `LPUSH`/`LPOP`은 앞, `RPUSH`/`RPOP`은 뒤라서 양 끝을 다 씁니다. `VecDeque`는 앞뒤 넣고 빼기와 i번째 접근이 다 빠릅니다. 표준 라이브러리라 의존성도 아닙니다 | **`Vec`**: 새로 배울 건 없지만 한쪽 끝은 넣고 뺄 때마다 나머지를 전부 옮깁니다. 작업 큐는 `LPUSH` + `RPOP`이라 반드시 그 느린 쪽을 씁니다. **직접 만든 연결 리스트**: 한 칸을 앞뒤 이웃이 같이 가리켜야 해서 `Rc<RefCell>`이 필요하고(`unsafe` 금지), `LRANGE q 5000 5010`이 5000칸을 따라갑니다 |
| 값 타입을 어떻게 나눌지 | `enum Value { Str(Vec<u8>), List(VecDeque<Vec<u8>>) }`를 `Entry.value`에 둠 | 로드맵이 말한 모양입니다. 만료 시각은 `Entry`에 그대로 있어서 S04의 TTL 코드를 한 줄도 안 고쳤습니다 | 타입마다 맵을 따로 두기: 같은 키가 두 맵에 동시에 있을 수 있게 되고, `DEL`/`EXPIRE`가 맵마다 봐야 합니다 |
| WRONGTYPE을 어디서 판정할지 | `as_string`, `as_list` helper 두 개가 `match`로 판정 | 값 종류가 늘면(S08 Hash) 이 두 `match`가 컴파일 에러를 내서 빠뜨릴 수가 없습니다. 커맨드 갈래에는 `as_list(&mut entry.value)?` 한 줄만 남습니다 | 커맨드마다 `match`: 같은 코드가 6번 나옵니다 |
| `execute`의 실패 | `Result<Vec<u8>, CommandError>`로 바꿈 (리팩터링 커밋 `4eadb6e`를 따로 둠) | S05 문서에 "WRONGTYPE을 내는 커맨드가 여러 개가 되면 바꾼다"고 적어 둔 조건이 왔습니다. 각 갈래에서 `?`로 빠져나갑니다 | S05 방식(helper만 `Result`): helper가 커맨드마다 하나씩 필요해집니다 |
| `LPUSH`와 `RPUSH`를 어떻게 만들지 | `Command::Push { end }`, `Command::Pop { end }`로 묶고 `enum End { Left, Right }`로 구분 | 다른 건 `push_front`냐 `push_back`이냐 한 줄뿐입니다 | 커맨드마다 갈래: 같은 코드가 두 번씩 나옵니다 |
| `LRANGE` 번호를 언제 볼지 | 정수인지는 `parse`에서, 범위 계산은 `execute`에서 | 정답지도 인자 에러가 타입 에러보다 먼저입니다(`LRANGE 문자열키 abc 1` → not an integer). 범위는 길이를 알아야 계산됩니다 | 전부 `execute`에서: 락을 잡은 채로 인자 에러가 납니다 |
| `LPOP q 2`를 어떻게 답할지 | 인자 개수 에러 | 개수 인자는 이번 범위가 아닙니다. S04에서 `SET`의 모르는 옵션을 syntax error로 보낸 것과 같은 처리입니다 | 조용히 무시하고 하나만 빼기: 틀린 응답 모양(`$`)을 정상처럼 보냅니다 |

위쪽 1개(리스트 자료구조)는 AI가 선택지 3개와 추천을 냈고 제가 추천을 골랐습니다. 나머지는 AI가 구현하면서 정했습니다. 반박 환영입니다.

**뒤집을 조건**
- 리스트 자료구조: `LINSERT`처럼 중간에 끼워 넣는 커맨드가 주가 되면 다시 봅니다. `VecDeque`도 중간 삽입은 원소를 옮깁니다
- 값 타입: S08에서 `Value::Hash`가 생기면 `as_string`/`as_list`에 갈래를 하나씩 더합니다. helper가 타입 수만큼 늘어 번거로워지면 그때 묶습니다
- `LRANGE`: 락을 잡은 채 응답 바이트를 만듭니다. 원소 100만 개짜리 리스트에 `LRANGE q 0 -1`을 하면 그동안 다른 커넥션이 전부 기다립니다. S10(`KEYS` vs `SCAN`)에서 같은 문제로 다시 봅니다

## 3. AI를 어떻게 썼나

- **첫 프롬프트:** "이거 5 6 끝까지 구현해주고 그 아티팩트 링크 다시주셈". 세션 번호를 하나씩 틀리게 적었고, AI가 "S05는 이미 main에 있는데 덱의 S06·S07을 말한 게 맞냐"고 되물었습니다
- **AI가 먼저 물은 것:** 범위(S06+S07, main 푸시), 리스트 자료구조, S07의 재우고 깨우는 방식, 나간 대기자 처리. 네 개 다 선택지마다 지금 비용/나중 비용, 추천, 뒤집을 조건, 새 Rust 개념이 붙어 있었고 네 개 다 추천을 골랐습니다
- **AI가 정한 것:** 2번 표에서 리스트 자료구조를 뺀 나머지 전부
- **AI 제안 중 거절한 것:** 없습니다
- **AI가 틀린 것:** S06 구현에서는 고친 것이 없습니다. 구현 전에 정답지 Redis에 커맨드 107개를 먼저 보내서 응답을 받아 두고, 구현 뒤에 같은 걸 우리 서버에 보내 바이트로 비교했습니다
- **이해하지 못한 채 넘어간 코드:** 아직 코드를 직접 읽지 않았습니다. 발표 전에 따라가 볼 곳입니다
  - `Command::Pop`의 `let list = as_list(&mut entry.value)?;` 뒤에 `entries.remove(&key, now)`를 불러도 되는 이유. `list`가 `entries`를 빌리고 있는데 왜 컴파일되는지
  - `Command::Push`의 `match entries.live(&key, now) { Some(..) => .., None => { entries.insert(..) } }`: `None` 갈래에서 `entries`를 또 쓸 수 있는 이유 (S05의 `let ... else`와 같은 질문)
  - `clamp_range`가 돌려주는 `RangeInclusive<usize>`와 `list.range(range)`: 복사 없이 구간만 빌려주는 반복자
  - `as_string`이 `&mut Vec<u8>`를 돌려주고 `incr_by`가 `*bytes = ...`로 써넣는 부분
- **AI 없이 결정한 것:** S06과 S07을 한 번에 하기, PR 없이 main에 올리기

## 4. 내가 추가한 테스트 케이스

`tests/cases/06-list.txt` (주석 뺀 것, 136줄)

```
LPUSH l6:q a b c
RPUSH l6:q x
LRANGE l6:q 0 -1
LLEN l6:q
LPOP l6:q
RPOP l6:q
LRANGE l6:q 0 -1
LLEN l6:q
LLEN l6:nope
LPOP l6:nope
RPOP l6:nope
LRANGE l6:nope 0 -1
EXISTS l6:nope
RPUSH l6:n 1 2 3 4 5 6 7 8 9 10 11 12
LRANGE l6:n 0 -1
LRANGE l6:n 0 0
LRANGE l6:n 5 5
LRANGE l6:n -3 -1
LRANGE l6:n -2 -1
LRANGE l6:n 1 -2
LRANGE l6:n 0 100
LRANGE l6:n 10 20
LRANGE l6:n -100 100
LRANGE l6:n -13 0
LRANGE l6:n 3 1
LRANGE l6:n 12 20
LRANGE l6:n -1 -2
LRANGE l6:n -20 -13
LRANGE l6:n -9223372036854775808 9223372036854775807
LRANGE l6:n 9223372036854775807 -9223372036854775808
LRANGE l6:n -9223372036854775808 -9223372036854775808
LRANGE l6:n 9223372036854775807 9223372036854775807
LRANGE l6:n abc 1
LRANGE l6:n 0 abc
LRANGE l6:n 0 1.5
LRANGE l6:n +0 1
LRANGE l6:n 9223372036854775808 1
LRANGE l6:n 0 -9223372036854775809
RPUSH l6:e a
LPOP l6:e
EXISTS l6:e
LLEN l6:e
LPOP l6:e
RPUSH l6:e a b
RPOP l6:e
RPOP l6:e
EXISTS l6:e
SET l6:s hello
LPUSH l6:s a
RPUSH l6:s a
LPOP l6:s
RPOP l6:s
LLEN l6:s
LRANGE l6:s 0 -1
GET l6:s
RPUSH l6:l a
GET l6:l
INCR l6:l
DECR l6:l
INCRBY l6:l 1
LRANGE l6:l 0 -1
LRANGE l6:s abc 1
LRANGE l6:s 0
LPUSH l6:s
INCRBY l6:l abc
EXISTS l6:l
TTL l6:l
EXPIRE l6:l 100
TTL l6:l
PERSIST l6:l
TTL l6:l
SET l6:l v NX
LRANGE l6:l 0 -1
SET l6:l v XX
GET l6:l
LLEN l6:l
RPUSH l6:d a b
DEL l6:d
EXISTS l6:d
RPUSH l6:t a
EXPIRE l6:t 100
RPUSH l6:t b
LPUSH l6:t c
TTL l6:t
LPOP l6:t
TTL l6:t
LPOP l6:t
LPOP l6:t
TTL l6:t
RPUSH l6:t d
TTL l6:t
RPUSH l6:x old1 old2
EXPIRE l6:x 0
EXISTS l6:x
SET l6:pxs v PX 100
SLEEP 0.3
RPUSH l6:pxs new
LRANGE l6:pxs 0 -1
TTL l6:pxs
lpush l6:c a
RpUsH l6:c b
lrange l6:c 0 -1
llen l6:c
rpop l6:c
lpop l6:c
LPUSH
LPUSH l6:a
RPUSH
RPUSH l6:a
LPOP
RPOP
LLEN
LLEN l6:a l6:b
LRANGE
LRANGE l6:a
LRANGE l6:a 0
LRANGE l6:a 0 1 2
LRANGE l6:a abc 1 2
EXISTS l6:a
LPUSH l6:mail m1
LPUSH l6:mail m2
LPUSH l6:mail m3
LLEN l6:mail
LRANGE l6:mail -2 -1
RPUSH l6:mail reset
RPOP l6:mail
RPOP l6:mail
LPOP l6:mail
LRANGE l6:mail 0 -1
PING
ECHO hello
SET l6:k 10
INCR l6:k
GET l6:k
DEL l6:q l6:n l6:s l6:l l6:t l6:pxs l6:mail l6:k
DBSIZE
```

- **`LPUSH l6:q a b c` → `LRANGE`:** 한꺼번에 넣는 게 아니라 하나씩 왼쪽에 넣어서 순서가 뒤집히는지
- **`LRANGE l6:n -20 -13`:** 끝 번호가 맨 앞보다도 앞이면 0으로 당기지 않고 빈 배열입니다. 시작 번호만 당깁니다
- **`LRANGE`의 i64 양 끝 4줄:** 음수 번호에 길이를 더할 때 넘치지 않는지
- **`LRANGE l6:s abc 1`, `INCRBY l6:l abc`:** 인자 에러가 WRONGTYPE보다 먼저인지. `parse`와 `execute`를 나눈 순서가 정답지와 같은지 봅니다
- **`SET l6:l v NX` → `LRANGE`:** `NX`가 리스트 키도 "있는 키"로 보는지, 실패한 `SET`이 리스트를 안 건드리는지
- **`RPUSH l6:t ...` → 다 빼기 → `RPUSH` → `TTL`:** 비어서 지워졌다 다시 생긴 리스트는 TTL이 없는 새 키인지
- **`SET l6:pxs v PX 100` → `SLEEP 0.3` → `RPUSH l6:pxs new`:** 만료된 문자열 키 자리에 WRONGTYPE 없이 리스트가 생기는지
- **작업 큐 장면:** 덱의 메일 큐 시나리오 그대로입니다
- **키 이름을 `l6:`로 시작한 이유:** 우리 서버는 FLUSHALL이 없어서 앞 파일의 키가 남습니다. 이제 타입이 생겨서, 남은 문자열 키와 이름이 겹치면 우리 쪽만 WRONGTYPE이 납니다
- **넣었다가 깨진 것:** 없습니다

**difftest가 못 잡는 것은 단위 테스트에 바이트로 박아 두었습니다**
- `LPOP`의 nil `$-1`과 `LRANGE`의 빈 배열 `*0`은 redis-cli에서 둘 다 빈 줄입니다
- 길이 `:3`과 문자열 `$1\r\n3`도 구분되지 않습니다
- 빈 값(`LPUSH q ""`)은 케이스 파일에 못 넣습니다
- 동시성: 넣는 스레드 4개와 빼는 스레드 4개를 같이 돌려서, 뺀 개수 + 남은 개수 = 넣은 개수인지 봅니다

## 5. 막혔던 곳

없었습니다. S04에서 값 옆에 TTL을 둔 덕분에 `Entry.value`의 타입만 바꾸면 됐고, 컴파일러가 고칠 곳 세 군데(`SET`, `GET`, `incr_by`)를 다 찾아 줬습니다.

생각할 곳은 `LRANGE`의 번호 계산이었습니다. "음수는 0으로 당긴다"로 외우면 `LRANGE q -20 -13`이 원소 하나를 돌려줘서 틀립니다. 정답지는 시작 번호만 당기고, 끝 번호가 음수로 남으면 빈 배열입니다.

## 6. 새로 배운 Rust 개념

- 데이터를 품은 `enum`과 `match`: `Value::Str(bytes)`처럼 경우마다 다른 데이터를 들고 있고, `match`가 모든 경우를 처리했는지 컴파일할 때 확인합니다. 그 "다른 경우"의 답이 WRONGTYPE입니다
- `VecDeque`: `push_front`/`push_back`/`pop_front`/`pop_back`이 전부 원소를 옮기지 않습니다. `range(a..=b)`는 구간을 복사하지 않고 차례로 빌려줍니다
- `Result`를 돌려주는 함수 안의 `?`: `as_list(&mut entry.value)?` 한 줄이 "리스트가 아니면 WRONGTYPE으로 바로 끝내기"입니다

## 7. 검증 결과

`kdh/s06-list` 끝 커밋(`e3655a8`)에서 돌린 결과입니다.

```
$ cargo fmt --check                          # 통과
$ cargo clippy --all-targets -- -D warnings  # 통과
$ cargo test                                 # 49 passed
$ ./scripts/difftest.sh                      # 통과 355 / 실패 0
raw 소켓 비교 (진짜 Redis 8.10.2)            # 107개 모두 같음
```

S07까지 올린 뒤 release 빌드로 잰 값입니다 (`redis-benchmark -n 100000 -c 50`, 한 번 잰 값).

| | 우리 | Redis 8.10.2 |
| --- | --- | --- |
| `LPUSH` | 19.7만 rps | 25.5만 rps |
| `RPOP` | 21.3만 rps | 25.6만 rps |
| `LRANGE` 100개 | 9.6만 rps | 14.3만 rps |

- **바이트 비교:** 6개 커맨드의 응답, `LRANGE` 번호 계산, WRONGTYPE 양방향, 인자 에러 우선순위, 빈 리스트가 사라지는지, TTL 유지와 소멸, 빈 값
- **금지 규칙:** 프로덕션 코드에 `unwrap()`, `expect(`, `unsafe`, `.clone()` 0개

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- **남의 케이스로 내 서버를 돌려본 결과:**

## 9. 알려진 차이와 한계

- `LPOP key count`, `RPOP key count`가 없습니다. 정답지는 배열을 답하는데 우리는 인자 개수 에러입니다
- `LINDEX`, `LSET`, `LINSERT`, `LREM`, `LTRIM`, `TYPE`이 없습니다
- `LRANGE`가 락을 잡은 채 응답 전체를 만듭니다. 아주 긴 리스트에서는 다른 커넥션이 그동안 기다립니다
- 진짜 Redis는 작은 리스트를 listpack, 큰 리스트를 quicklist로 저장합니다. 우리는 크기와 상관없이 `VecDeque` 하나입니다. 응답 바이트는 같습니다

# S07 BLPOP / BRPOP — kdh

- 구현 브랜치: `kdh/s07-blpop` (`kdh/s06-list` 위에서 시작, 로컬). main에 `f17ef00`으로 반영했습니다
- 걸린 시간: 약 15분 (11/12 22:25 S06 커밋 → 22:39 S07 커밋). 그중 절반은 테스트가 버그를 진짜 잡는지 확인하고 보강하는 데 썼습니다

## 1. 무엇을 만들었나

커맨드 2개보다 **응답을 당장 못 주는 손님을 어떻게 재우고, 누가 어떻게 깨우는가**가 본론입니다.

| 항목 | 내용 |
| --- | --- |
| `BLPOP key [key ...] timeout` | 차 있으면 `LPOP`처럼 바로 꺼냅니다. 응답은 `[키, 값]` 두 칸 배열입니다. 키가 여럿이면 앞 키부터 봅니다 |
| `BRPOP ...` | 똑같고 뒤에서 꺼냅니다 |
| 전부 비어 있으면 | 응답을 안 보내고 기다립니다. 다른 손님이 그 키에 넣으면 바로 받습니다 |
| timeout | 초 단위이고 소수도 됩니다(`0.5`). 0은 무한정입니다. 다 되면 nil 배열 `*-1`입니다 |
| 대기 순서 | 같은 키를 먼저 기다린 손님이 먼저 받습니다 |
| 넣기와 깨우기의 순서 | 넣기를 다 끝낸 다음 넘깁니다. 대기자가 있어도 `RPUSH q a b c`는 `:3`이고, `LPUSH q a b c`면 대기자는 c를 받습니다 |
| 에러 | `timeout is negative`, `timeout is not a float or out of range`, `timeout is out of range`, 문자열 키면 기다리지 않고 WRONGTYPE |

- 스펙: https://redis.io/docs/latest/commands/blpop/ · https://redis.io/docs/latest/commands/brpop/
- 정답지 Redis: 8.10.2

**빼기로 한 것:** 기다리던 손님이 연결을 끊고 나간 경우의 처리(9번), `MULTI` 안의 `BLPOP`(S09), `BLMOVE`·`BLMPOP`·`BRPOPLPUSH`, tokio 전환, `redis` crate(여러 손님 테스트는 std `TcpStream`으로 짰습니다). 구현 전에 AI가 이 목록을 먼저 내고 확인받았습니다.

## 2. 설계 선택

| 갈림길 | 고른 것 | 이유 | 버린 것 |
| --- | --- | --- | --- |
| 어떻게 재우고 깨울지 | 대기자마다 `mpsc` 채널. 대기자는 `recv_timeout`으로 자고, 넣는 쪽이 `Sender`로 값을 직접 넘김 | 정답지에 `RPUSH q x`와 `LPOP q`를 붙여 보내면 `LPOP`은 nil이고 대기자가 x를 받습니다. 즉 넣는 쪽이 넣자마자 넘겨야 응답이 같아집니다. 채널은 "누구에게"가 정해져 있어서 순서가 명단 순서 그대로입니다 | **`Condvar`**: 코드는 제일 적지만 누구를 깨울지 못 골라서 다 깨우고 순번을 따로 세워야 합니다. 깬 대기자가 직접 꺼내는 구조라, 그 사이에 다른 `LPOP`이 가로채는 틈을 또 막아야 합니다. **tokio 전환**: 새 의존성이고 S01~S06 코드를 전부 async로 다시 짜야 합니다 |
| 대기자 명단을 어디 둘지 (토론 주제) | 저장소와 같은 `Mutex` 안(`Entries`의 필드) | "비었는지 확인 → 명단에 올리기"와 "넣기 → 명단 확인 → 넘기기"가 각각 락 한 번 안에서 끝납니다. 그 사이에 끼어들 틈이 없습니다 | **따로 둔 락**: 저장소와 명단이 각자 단순해지지만 락 두 개를 잡는 순서를 정해야 하고, 엇갈리면 데드락입니다. S03에서 샤딩을 버린 이유와 같습니다 |
| S03의 동시성 모델이 버티는지 (토론 주제) | 버팁니다. `Arc<Mutex<...>>` + 커넥션당 스레드 그대로 | 자는 건 락 밖에서, 그 커넥션의 스레드 하나만 잡니다. 락을 쥐는 시간은 여전히 커맨드 하나만큼입니다 | 갈아엎기: 이유가 없었습니다 |
| 명단의 모양 | `waiters: HashMap<번호표, Waiter>` + `queues: HashMap<키, VecDeque<번호표>>` | "번호표가 `waiters`에 있다 = 아직 못 받았다"가 유일한 기준입니다. 시간이 다 된 대기자가 락을 잡고 자기 번호표를 지워 보면, 못 받았는지(지워짐) 받았는지(이미 없음)가 갈립니다 | 키마다 `VecDeque<Sender>`만 두기: 여러 키를 기다리는 대기자가 한 곳에서 받았는지 알 방법이 없어서 두 번 받거나, 시간이 다 된 순간 도착한 값을 잃습니다 |
| 어디서 자는지 | `execute`는 `Outcome::Block(Blocked)`을 돌려주기만 하고, `Connection`이 `blocked.wait()`를 부름 | `execute` 안에서 자면 락 guard가 살아 있는 채로 잘 위험이 있고, 앞 명령의 응답을 먼저 보낼 수도 없습니다. `PING`과 `BLPOP q 0`을 붙여 보내면 정답지는 PONG을 바로 줍니다 | `execute` 안에서 guard를 `drop`하고 자기: 응답을 먼저 보낼 방법이 없습니다 |
| 넘겨줄 곳 | `Push`의 "리스트가 없던" 갈래에서만 `hand_off` | 대기자는 리스트가 없을 때만 명단에 올라가므로, 리스트가 있으면 대기자가 없습니다. 새 리스트를 저장하기 전에 넘기고, 다 가져가면 아예 저장하지 않습니다 | 모든 `Push` 뒤에 명단 확인: 절대 안 걸리는 검사가 생깁니다 |
| timeout 표현 | `parse`에서 `Option<Instant>`(언제까지)로 바꿈. 0이면 `None` | 문자열 키여도 timeout 에러가 WRONGTYPE보다 먼저라는 정답지 순서가 그대로 나옵니다. 밀리초로 올림하는 것도 정답지와 같습니다(`0.0001` → 1ms) | `Duration`으로 들고 다니기: 자기 직전까지 흐른 시간을 따로 빼야 합니다 |
| 여러 손님 테스트 | 저장소 수준은 `std::thread::scope` 단위 테스트, 소켓 수준은 `tests/blocking.rs`(서버를 띄우고 std `TcpStream`으로 붙음) | difftest는 한 줄마다 redis-cli를 새로 띄워서 손님이 늘 하나입니다. `redis` crate는 새 의존성이라 안 썼습니다 | difftest 확장: 러너를 여러 손님용으로 다시 짜야 합니다 |

위쪽 2개는 AI가 선택지와 추천을 냈고 제가 추천을 골랐습니다. 나머지는 AI가 구현하면서 정했습니다. 반박 환영입니다.

**뒤집을 조건**
- 스레드 + 채널: 대기자 1명이 스레드 1개를 깔고 앉습니다. 워커 수천 대가 `BRPOP`으로 기다리는 상황을 재현하거나, Pub/Sub처럼 "커넥션이 읽기와 깨어나기를 동시에 기다려야" 하는 기능이 오면 tokio를 다시 봅니다
- 나간 대기자: 9번의 값 유실을 실제로 막아야 하면 자는 동안 소켓을 같이 봐야 합니다. 스레드 모델에서는 짧게 깨서 확인하는 수밖에 없어서, 이것도 tokio(`select!`)로 가는 이유가 됩니다
- 명단 위치: S09 `MULTI`/`WATCH`에서 커넥션별 상태가 또 `Entries`에 붙으면 구조체가 너무 커집니다. 그때 저장소와 "커넥션 사이의 약속"을 나누는 걸 봅니다

## 3. AI를 어떻게 썼나

- **첫 프롬프트:** S06과 같은 요청입니다("이거 5 6 끝까지 구현해주고…"). 주제는 11/9에 만든 구현 전 설명 덱으로 먼저 봤습니다
- **AI가 먼저 한 것:** 갈림길을 묻기 전에 정답지에 `RPUSH q x` + `LPOP q`를 붙여 보내서 "대기자가 다음 명령보다 먼저 받는다"를 확인했고, 그 결과를 추천 근거로 냈습니다
- **AI가 정한 것:** 2번 표의 위쪽 2개를 뺀 나머지 전부
- **AI 제안 중 거절한 것:** 없습니다
- **AI가 틀린 것**
  - **테스트가 버그를 못 잡았습니다.** "시간이 다 된 순간 도착한 값을 꺼내는" 줄을 지워도, "받은 대기자를 다른 줄에서 지우는" 줄을 지워도 처음 테스트는 전부 통과했습니다. 스트레스 테스트는 그 순간을 못 만들었습니다. 그래서 테스트가 락을 쥐고 그 순간을 직접 만드는 테스트와, 두 줄에 선 대기자 뒤에 한 명을 더 세우는 테스트를 추가했습니다. 지금은 두 줄 다 지우면 실패합니다
  - **주석이 틀렸습니다.** "다른 줄에서 이름을 안 지우면 이미 받은 사람이 또 받는다"고 적었는데(덱 19장도 그렇게 말합니다), 이 구현에서는 번호표가 `waiters`에서 빠져 있어서 또 받지는 않습니다. 대신 주인 없는 번호표가 줄 앞에 남아 **뒤에 선 사람이 못 받습니다**. 주석을 고쳤습니다
  - **통합 테스트가 가끔 실패했습니다.** 빈 포트를 찾으려고 테스트가 잠깐 연 소켓이 다른 테스트가 띄우는 서버 프로세스로 복사돼서, 연결이 엉뚱한 곳에 붙었다가 끊겼습니다. 서버가 아니라 테스트 코드 문제였고, 포트를 직접 열어 보지 않게 고친 뒤 40번 연속 통과했습니다
  - 정답지 실측 중에 빈 키에 `BLPOP q 1e15`를 보내서 실측 스크립트가 멈췄습니다. 케이스 파일에 "빈 리스트에 긴 timeout을 주는 줄은 넣지 않는다"를 적어 둔 이유입니다
- **이해하지 못한 채 넘어간 코드:** 아직 코드를 직접 읽지 않았습니다. 발표 전에 따라가 볼 곳입니다
  - `Blocked::wait`의 `let still_waiting = db.lock().remove_waiter(self.id);`: guard가 이 줄 끝에서 사라져 락이 풀리는 이유. `if db.lock()... { }`로 쓰면 뭐가 달라지는지
  - `BlockingPop` 갈래가 `return Ok(Outcome::Block(...))`으로 끝날 때 락이 풀리는 시점. 자는 건 그 뒤라는 것
  - `Waiter::deliver`의 `.map_err(|SendError((_, value))| value)`: `send`가 실패하면 보내려던 값을 돌려준다는 것
  - `hand_off`가 `key.to_vec()`으로 키를 복사하는 이유, `add_waiter`가 `key.clone()`을 하는 이유
  - `tests/blocking.rs`의 `impl Drop for Server`: 테스트가 실패해도 서버가 꺼지는 이유
- **AI 없이 결정한 것:** S06과 S07을 한 번에 하기, PR 없이 main에 올리기

## 4. 내가 추가한 테스트 케이스

`tests/cases/07-blpop.txt` (주석 뺀 것, 77줄). 손님 하나로 재현되는 것만 있습니다.

```
RPUSH b7:q a b c
BLPOP b7:q 0
BRPOP b7:q 0
BLPOP b7:q 5
EXISTS b7:q
RPUSH b7:q2 x y
BLPOP b7:q1 b7:q2 0
RPUSH b7:q1 first
BRPOP b7:q1 b7:q2 0
BRPOP b7:q1 b7:q2 0
EXISTS b7:q1 b7:q2
BLPOP b7:none 0.1
BRPOP b7:none 0.1
BLPOP b7:none b7:none2 0.1
BLPOP b7:none b7:none 0.1
BLPOP b7:none 0.0001
EXISTS b7:none
SET b7:s str
BLPOP b7:s 0
BRPOP b7:s 0
BLPOP b7:none b7:s 0
RPUSH b7:l v1 v2
BLPOP b7:l b7:s 0
BLPOP b7:s b7:l 0
BLPOP b7:none b7:l b7:s 0
EXISTS b7:l
GET b7:s
RPUSH b7:f 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16
BLPOP b7:f -1
BLPOP b7:f -0.5
BLPOP b7:f abc
BLPOP b7:f nan
BLPOP b7:f inf
BLPOP b7:f -inf
BLPOP b7:f 1e16
BLPOP b7:f 9223372036854775807
BLPOP b7:f 1,5
BLPOP b7:f 1_0
BLPOP b7:s abc
BLPOP b7:s -1
BRPOP b7:f -1
BRPOP b7:f abc
LLEN b7:f
BLPOP b7:f 0.5
BLPOP b7:f +1
BLPOP b7:f 1.
BLPOP b7:f .5
BLPOP b7:f 1e0
BLPOP b7:f 1E0
BLPOP b7:f -0
BLPOP b7:f -0.0
BLPOP b7:f 00.5
BLPOP b7:f 0.0001
BLPOP b7:f 1e-9
BLPOP b7:f 1e15
blpop b7:f 0
bRpOp b7:f 0
LRANGE b7:f 0 -1
BLPOP
BLPOP b7:f
BRPOP
BRPOP b7:f
RPUSH b7:t a b
EXPIRE b7:t 100
BLPOP b7:t 0
TTL b7:t
BRPOP b7:t 0
TTL b7:t
RPUSH b7:px a
EXPIRE b7:px 0
BLPOP b7:px 0.1
PING
RPUSH b7:k a
LPOP b7:k
LPOP b7:k
DEL b7:s b7:f
DBSIZE
```

- **`BLPOP b7:q1 b7:q2 0`:** 앞 키가 비어 있으면 다음 키에서 꺼내고, 응답에 그 키 이름이 들어가는지
- **`BLPOP b7:none b7:s 0`, `BLPOP b7:l b7:s 0`, `BLPOP b7:s b7:l 0`:** 문자열 키가 어디 있느냐에 따라 WRONGTYPE인지 정상인지. 차 있는 리스트보다 앞에 있을 때만 에러입니다
- **`BLPOP b7:s abc`:** timeout 에러가 WRONGTYPE보다 먼저인지
- **`BLPOP b7:f -inf` / `inf` / `nan`:** 에러 문구가 셋 다 다릅니다(negative / out of range / not a float)
- **timeout 표기 12줄:** `+1`, `1.`, `.5`, `-0`, `1e-9`를 정답지가 다 받습니다. 리스트가 차 있어서 기다리지 않고 바로 답합니다
- **`BLPOP b7:none 0.0001`:** 1ms보다 짧은 timeout이 0(무한정)으로 내려가지 않고 올림되는지. 내려가면 difftest가 멈춥니다
- **`BLPOP b7:none b7:none 0.1`:** 같은 키를 두 번 적어도 되는지
- **넣었다가 깨진 것:** 없습니다

**difftest가 못 잡는 것**

| 무엇 | 어디서 확인 |
| --- | --- |
| nil 배열 `*-1` (redis-cli에서는 `$-1`, `*0`과 똑같이 빈 줄) | `src/command.rs` 단위 테스트에서 바이트로 |
| 먼저 기다린 손님이 먼저 받는지 (로드맵 완료 기준) | 단위 테스트 + `tests/blocking.rs` |
| 넣기를 끝낸 다음 넘기는지, `BLPOP`/`BRPOP` 대기자가 섞였을 때 | 단위 테스트 |
| 넣은 값을 다음 명령이 가로채지 못하는지 | 단위 테스트 + `tests/blocking.rs` |
| 여러 키를 기다리다 받으면 다른 줄에서도 빠지는지 | 단위 테스트 (뒤에 한 명을 더 세움) |
| 시간이 다 된 순간 넘겨받은 값이 사라지지 않는지 | 단위 테스트 (테스트가 락을 쥐고 그 순간을 만듦) |
| 한 손님이 자는 동안 다른 손님이 안 막히는지 | `tests/blocking.rs` |
| 자기 전에 앞 명령의 응답을 먼저 보내는지 | `tests/blocking.rs` |
| 넣는 쪽 4개 + 1ms마다 깨는 대기자 4개에서 값이 빠지거나 겹치지 않는지 | 단위 테스트 |

`tests/blocking.rs`의 기대 바이트와 위 장면들은 손님 여럿을 붙이는 스크립트로 정답지와 우리 서버에 똑같이 재생해서 비교했습니다(16장면, 전부 같음).

## 5. 막혔던 곳

- **락을 쥔 채로 자지 않기.** S03 문서에 "S7에서 막혀 보는 경험을 먼저 한다"고 적었는데, 실제로 막히게 짜 보지는 않았습니다. 처음부터 `execute`가 `Block`을 돌려주고 락이 풀린 뒤에 자는 구조로 갔습니다. 대신 `tests/blocking.rs`의 "한 손님이 자는 동안 다른 손님은 막히지 않는다"가 그 장면을 지킵니다
- **시간이 다 된 순간에 도착한 값.** 대기자가 깨어나서 락을 잡기 전에 넣는 쪽이 값을 넘기면, 넣는 쪽은 줬다고 알고 대기자는 nil을 답해서 값이 사라집니다. 번호표가 `waiters`에 남아 있는지를 락 안에서 확인하고, 없으면 우편함을 한 번 더 보는 것으로 막았습니다
- **테스트가 통과하는 것과 테스트가 잡는 것은 달랐습니다.** 3번에 적은 대로, 위 두 줄을 지워도 처음 테스트는 초록불이었습니다

## 6. 새로 배운 Rust 개념

- `mpsc::channel()`: 보내는 쪽 `Sender`와 받는 쪽 `Receiver`로 나뉜 우편함입니다. `recv_timeout`은 값이 오거나 시간이 다 될 때까지 그 스레드만 재웁니다. `send`는 받는 쪽이 사라졌으면 보내려던 값을 에러에 담아 돌려줍니다
- guard의 수명: `MutexGuard`는 변수에 담으면 그 블록 끝까지, `db.lock().f()`처럼 바로 쓰면 그 문장 끝까지 삽니다. "락을 놓고 잔다"는 결국 guard가 언제 사라지느냐의 문제입니다
- `Duration::try_from_secs_f64`: `from_secs_f64`는 음수나 무한대에서 패닉합니다. `try_`가 붙은 쪽은 `Result`를 돌려줘서 `inf` 한 번으로 서버가 죽지 않습니다

## 7. 검증 결과

`kdh/s07-blpop` 끝 커밋(`c861fab`, main `f17ef00`과 같은 내용)에서 돌린 결과입니다.

```
$ cargo fmt --check                          # 통과
$ cargo clippy --all-targets -- -D warnings  # 통과
$ cargo test                                 # 단위 65 passed + tests/blocking.rs 5 passed (40번 연속)
$ ./scripts/difftest.sh                      # 통과 432 / 실패 0
raw 소켓 비교 (진짜 Redis 8.10.2)            # 손님 하나 77개, 손님 여럿 16장면 모두 같음
```

- **변이 확인:** `try_recv`를 지우면 "시간이 다 된 순간에…" 테스트가, `take_waiter`의 `leave_queues`를 지우면 "여러 키를 기다리다…" 테스트가 실패합니다
- **금지 규칙:** 프로덕션 코드에 `unwrap()`, `expect(`, `unsafe` 0개. `.clone()` 1개(`add_waiter`: 줄과 대기자가 키를 각자 들고 있어야 해서)와 `to_vec()` 복사 1개(`hand_off`: 대기자 응답에 들어갈 키)는 이유를 주석에 적었습니다
- **S06 벤치:** `s06-kdh.md` 7번에 있습니다. `BLPOP`은 따로 재지 않았습니다

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- **남의 케이스로 내 서버를 돌려본 결과:**

## 9. 알려진 차이와 한계

- **기다리던 손님이 연결을 끊고 나가도 서버는 모릅니다.** 자는 스레드는 소켓을 보지 않습니다. 그 뒤에 값이 들어오면 정답지는 리스트에 남기는데, 우리는 나간 손님 몫으로 넘겨서 **그 값 하나가 사라집니다**. timeout이 0이면 그 스레드는 값이 올 때까지 남아 있습니다. 구현 전에 이번 범위에서 빼기로 정한 것입니다. 워커가 재시작되는 장면을 재현하려면 여기부터 고쳐야 합니다
- 대기자 1명이 OS 스레드 1개를 씁니다
- timeout 표기 중 정답지와 다른 것: 16진수(`0x1`)는 정답지가 받는데 우리는 not a float입니다. `1e400`은 정답지가 not a float인데 우리는 out of range입니다. `1e-400`은 정답지가 not a float인데 우리는 0으로 읽어 무한정 기다립니다. 약 9,223,370,000,000,000초를 넘는 아주 큰 값의 경계도 조금 다릅니다(정답지는 현재 시각을 더해 보고 넘치면 거부합니다)
- `MULTI` 안의 `BLPOP`(기다리지 않고 바로 nil)은 S09 범위입니다
- `BLMOVE`, `BLMPOP`, `BRPOPLPUSH`가 없습니다
- `tests/blocking.rs`는 서버가 명단에 올릴 시간을 150ms sleep으로 줍니다. 소켓 밖에서는 명단을 볼 수 없어서입니다. 아주 느린 CI에서는 이 테스트가 흔들릴 수 있습니다

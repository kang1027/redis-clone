# S01 PING — kdh

> **이 문서는 AI(Claude)가 S01을 끝까지 진행해 본 데모입니다.**
> "설계 문서가 어떻게 생겼나" 보려고 만든 것이고, 실제 세션에서는 각자 자기 것으로 채웁니다.

- 구현 브랜치: `kdh/s01-ping`
- 걸린 시간: 약 1시간

## 1. 무엇을 만들었나

TCP로 연결을 받고 `PING` / `PING <msg>`에 진짜 Redis와 같은 바이트로 응답합니다.
커맨드는 `PING` 하나만 구현했고, 나머지는 모두 `unknown command` 에러로 돌려보냅니다.

- 스펙: https://redis.io/docs/latest/commands/ping/
- RESP: https://redis.io/docs/latest/develop/reference/protocol-spec/

## 2. 설계 선택 — 이 문서의 핵심

| 갈림길 | 고른 것 | 이유 | 버린 선택지와 그 이유 |
| --- | --- | --- | --- |
| 커넥션 처리 | `std` + 커넥션당 스레드 | 의존성 0개. 1주차에 배울 게 소유권/`move` 클로저/`Result`로 끝납니다 | **tokio async**: S7 `BLPOP`이 자연스러워지지만, 소유권도 모르는 상태에서 async 런타임까지 얹으면 둘 다 흐려집니다. S7에서 막히는 경험을 먼저 하는 쪽을 골랐습니다 |
| 파싱 범위 | RESP 배열 최소 파서를 S1에 포함 | 테스트 케이스에 `PING hello`가 있어서 인자를 읽어야 합니다. "무조건 `+PONG`"으로는 통과 못 합니다 | **하드코딩**: 3줄로 끝나지만 `PING hello`에서 바로 깨집니다 |
| 인라인 명령 | 지원함 (`PING\r\n`) | README의 "서버가 죽지 않는지 확인하는 법"이 `nc`로 생 텍스트를 던집니다. 진짜 Redis도 인라인을 받습니다 | **RESP만 지원**: `nc` 테스트에서 진짜 Redis와 응답이 갈립니다 |
| 형식이 깨졌을 때 | 에러 보내고 **연결을 끊음** | 진짜 Redis가 그렇게 합니다. 버퍼에 쓰레기가 남은 채로 계속 읽으면 뒤 명령이 전부 오염됩니다 | **버퍼만 비우고 계속**: 어디까지 버릴지 기준이 없습니다 |
| 버퍼 관리 | `Vec<u8>`에 누적하고 처리한 만큼 `drain` | TCP는 바이트 흐름이라 명령 하나가 쪼개져 도착합니다. 파이프라이닝도 같은 코드로 처리됩니다 | **`read` 한 번으로 끝내기**: 쪼개서 보내면 바로 깨집니다 (리뷰 체크리스트 항목이기도 합니다) |
| 배열 길이 처리 | `Vec::new()` 사용 | `*999999999\r\n` 한 줄로 메모리를 왕창 잡을 수 있어서, 선언된 길이를 믿고 `with_capacity`를 쓰지 않았습니다 | **`with_capacity(count)`**: 재할당은 줄지만 악의적 입력에 약합니다 |

**나중에 이 선택을 뒤집어야 하는 조건:**

- S7 `BLPOP`에서 "대기 중인 커넥션을 어떻게 재우고 깨울지"가 스레드로 안 풀리면 → tokio로 전환
- 커넥션 수가 수백 개로 늘어 스레드가 부담이 되면 → 그때도 전환

## 3. AI를 어떻게 썼나

- **첫 프롬프트에 뭐라고 썼나:** "S1 기준으로 핑퐁 한번 작성해봐. 어떻게 나오나 보게"
- **AI 제안 중 거절한 것과 이유:** AI가 먼저 "스레드 vs tokio 중 뭘로 할까요"를 선택지만 던졌는데,
  근거 없이 고르면 동전던지기라서 **추천과 이유를 붙여 달라고 되돌렸습니다.**
  그 뒤 "std 스레드 + S7에서 막히는 경험을 학습으로 쓴다"는 근거를 받고 수긍했습니다.
- **AI가 틀렸던 것 / 스펙과 달랐던 것:** 없었습니다. 다만 **먼저 진짜 Redis를 띄워 응답을 확인한 뒤**
  구현했기 때문입니다. 특히 아래 둘은 추측으로는 절대 못 맞혔을 문구입니다.
  - 인자 없는 모르는 커맨드: `ERR unknown command 'garbage'`
  - 인자 있는 모르는 커맨드: `ERR unknown command 'foo', with args beginning with: 'a' 'b' ` ← **끝에 공백**
- **이해하지 못한 채 넘어간 코드:** `let Some(x) = ... else { return };` (let-else) 문법.
  `match`로도 되는데 왜 이걸 쓰는지는 세션에서 물어볼 생각입니다.
- **AI 없이 내가 결정한 것:** 동시성 모델, 테스트 케이스에 뭘 넣을지.

## 4. 내가 추가한 테스트 케이스

```
# 커맨드 이름은 대소문자를 가리지 않습니다
ping
PiNg hello

# 인자 개수가 틀리면 에러 문구까지 같아야 합니다
PING a b
PING a b c

# 모르는 커맨드 — 인자가 있을 때와 없을 때 문구가 다릅니다
garbage
NOPE a b

# UTF-8 인자
PING 한글
```

- **왜 이 케이스를 넣었나:**
  - `ping` / `PiNg` — 커맨드 이름 비교를 `==`로 하면 바로 깨집니다
  - `PING a b` — 인자 개수 에러는 문구를 통째로 맞춰야 합니다
  - `garbage` vs `NOPE a b` — **인자 유무에 따라 에러 문구가 다릅니다.** 이걸 모르면 한쪽만 맞습니다
  - `PING 한글` — 문자열을 `String`으로 다루면 바이트 길이가 어긋납니다 (`$6`이지 `$2`가 아닙니다)
- **넣었다가 깨진 것:** 없습니다. 단 **정답을 먼저 보고 맞췄기 때문**입니다.
  학습만 보면 먼저 짜고 나중에 difftest로 깨지는 걸 보는 쪽이 낫습니다.

## 5. 막혔던 곳

- README의 견고성 테스트 `exec 3<>/dev/tcp/...`가 **zsh에서 동작하지 않습니다.**
  macOS 기본 셸이 zsh인데 `/dev/tcp`는 bash 기능입니다. `bash -c '...'`로 감싸야 합니다 → README 수정함
- `redis-cli -p 6379 "PING hello"`처럼 따옴표로 묶으면 **커맨드 이름 전체가 한 덩어리**로 가서
  `unknown command 'PING hello'`가 나옵니다. difftest는 따옴표 없이 보내기 때문에 이 차이를 모르면 헷갈립니다

## 6. 새로 배운 Rust 개념

- `thread::spawn(move || ...)` — `move`는 클로저가 값의 **소유권을 가져간다**는 뜻입니다.
  스레드가 언제까지 살지 모르니 빌려주면 안 되고 넘겨줘야 합니다.
- `Vec::drain(..n)` — 앞에서 n바이트를 잘라내고 나머지를 앞으로 당깁니다. 처리한 명령을 버릴 때 씁니다.
- `let Some(x) = ... else { ... }` (let-else) — 실패하면 바로 빠져나가고, 성공하면 `x`를 그 아래에서 계속 씁니다.

## 7. 검증 결과

```
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.18s

$ cargo test
running 7 tests
test tests::깨진_형식은_broken_이다 ... ok
test tests::resp_배열을_파싱한다 ... ok
test tests::ping_응답이_진짜_redis와_같다 ... ok
test tests::명령이_쪼개져_들어오면_기다린다 ... ok
test tests::모르는_커맨드_문구가_진짜_redis와_같다 ... ok
test tests::버퍼에_명령이_둘이면_앞의_것만_꺼낸다 ... ok
test tests::인라인_명령도_받는다 ... ok
test result: ok. 7 passed; 0 failed

$ ./scripts/difftest.sh tests/cases/01-ping.txt
=== 01-ping.txt ===
  ✅ PING
  ✅ PING
  ✅ PING hello
  ✅ ping
  ✅ PiNg hello
  ✅ PING a b
  ✅ PING a b c
  ✅ garbage
  ✅ NOPE a b
  ✅ PING 한글
------------------------------
통과 10 / 실패 0
```

서버가 죽지 않는지도 확인했습니다.

| 던진 것 | 결과 |
| --- | --- |
| `garbage\r\n` (인라인 쓰레기) | `-ERR unknown command 'garbage'`, 서버 생존 |
| 명령을 두 번에 쪼개서 전송 | `+PONG` 정상 |
| 파이프라인 3개 한 번에 | `+PONG` `+PONG` `$2 hi` 순서대로 |
| `*1\r\n+PING\r\n` (깨진 형식) | `-ERR Protocol error` 후 연결 종료, 서버 생존 |
| 동시 접속 50개 | 50개 전부 `PONG` |

> `02-echo.txt`는 아직 실패합니다. `ECHO`는 S2 주제라 이번에 구현하지 않았습니다.

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- **남의 케이스로 내 서버를 돌려본 결과:**

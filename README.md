# redis-clone

Rust로 Redis를 바닥부터 만들어보는 3인 스터디 저장소입니다.
Rust를 몰라도 AI와 함께 구현하면서, **설계 결정과 그 근거를 남기는 것**을 목표로 합니다.

**처음 왔다면 이 문서만 순서대로 읽으면 됩니다.** 나머지 문서는 필요할 때 링크로 연결해 뒀습니다.

## 한 줄 요약

> 매주 같은 주제를 **각자 혼자** 구현해 오고, 모여서 **왜 그렇게 짰는지**를 공유합니다.
> 스터디 시간에는 코드를 짜지 않습니다.

## 원칙 3개

1. **테스트가 먼저 있어야 합니다.** Rust를 모르면 코드만 보고 맞는지 판단할 수 없습니다.
   진짜 Redis를 정답지로 두고 응답을 비교합니다. (`scripts/difftest.sh`)
2. **한 세션에 기능 하나만.** "Redis 만들어줘" 한 방이면 아무것도 남지 않습니다.
3. **Rust는 코드에 등장한 것만 그때그때 배웁니다.** 사전 문법 공부는 하지 않습니다.

---

# 처음 온 사람: 세팅 (30분)

## 1. 준비물 설치

```bash
# macOS
brew install redis        # 정답지로 쓸 진짜 Redis (redis-server + redis-cli)
brew install gh           # GitHub CLI (PR 올릴 때)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust

# Ubuntu / WSL
sudo apt-get install -y redis-server redis-tools gh
```

```bash
# 설치 확인 — 셋 다 버전이 찍혀야 합니다
redis-server --version
redis-cli --version
cargo --version           # 1.98 이상
```

> `brew services start redis`는 **하지 않아도 됩니다.** 테스트 러너가 필요할 때 알아서 띄웁니다.

## 2. 저장소 받기

```bash
git clone git@github.com:kang1027/redis-clone.git
cd redis-clone
```

## 3. 진짜 Redis를 한 번 만져보기

우리가 뭘 베끼는지 먼저 봐야 합니다. **터미널 두 개**를 씁니다.

```bash
# 터미널 A — 진짜 Redis 서버를 띄웁니다 (끄려면 Ctrl+C)
redis-server --port 6379
```

```bash
# 터미널 B — 클라이언트로 접속해서 명령어를 쳐봅니다
redis-cli -p 6379
```

```
127.0.0.1:6379> PING
PONG
127.0.0.1:6379> SET name hello
OK
127.0.0.1:6379> GET name
"hello"
127.0.0.1:6379> GET 없는키
(nil)
127.0.0.1:6379> exit
```

`redis-cli`는 **서버가 아니라 클라이언트**입니다. 서버(`redis-server`)가 떠 있어야 붙습니다.
한 줄만 실행하고 빠져나오려면 이렇게도 됩니다.

```bash
redis-cli -p 6379 PING            # → PONG
```

## 4. 포트 약속

| 포트 | 무엇 | 누가 띄우나 |
| --- | --- | --- |
| **6379** | 진짜 Redis (정답지) | `difftest.sh`가 자동으로, 또는 내가 수동으로 |
| **6380** | **내가 짠 서버** | `cargo run -- --port 6380` |

우리 서버가 6380을 쓰는 이유는 6379를 진짜 Redis가 쓰기 때문입니다. 이 인터페이스는 바꾸지 않습니다.

## 5. 테스트 러너를 한 번 돌려보기

```bash
./scripts/difftest.sh
```

지금은 **실패하는 게 정상입니다.** `src/main.rs`가 아직 비어 있기 때문입니다.
이걸 초록불로 만드는 게 세션 1의 목표입니다.

---

# 테스트 러너가 하는 일

`./scripts/difftest.sh` 한 줄이 이걸 다 합니다.

1. **6379**에 진짜 Redis를 띄웁니다 (이미 떠 있으면 그걸 씁니다)
2. `cargo build` 후 **6380**에 내 서버를 띄웁니다
3. 양쪽에 `FLUSHALL`로 데이터를 비웁니다
4. `tests/cases/*.txt`의 명령어를 **양쪽에 똑같이** 보냅니다
5. 두 응답 문자열을 **그대로 비교**합니다

```
=== 01-ping.txt ===
  ✅ PING
  ❌ PING hello
       기대: hello|
       실제: PONG|

------------------------------
통과 2 / 실패 1
```

그래서 **"내가 짠 서버 vs 진짜 Redis" 비교는 이미 자동화되어 있습니다.**
에러 메시지 문구까지 똑같아야 통과입니다.

```bash
./scripts/difftest.sh                          # 전체
./scripts/difftest.sh tests/cases/01-ping.txt  # 이번 세션 것만
```

---

# 테스트 케이스 작성법

세션마다 **코드보다 테스트를 먼저** 씁니다. `tests/cases/NN-이름.txt`에 한 줄씩 적으면 끝입니다.
`tests/cases/01-ping.txt`와 `02-echo.txt`가 예시로 들어 있습니다.

```
# 세션 3: SET / GET / DEL / EXISTS
SET name hello
GET name
GET 없는키
EXISTS name
DEL name
GET name
SET name first
SET name second
GET name
```

규칙은 네 개뿐입니다.

- **한 줄에 커맨드 하나.** 인자는 공백으로 구분합니다
- `#`으로 시작하는 줄과 빈 줄은 무시됩니다
- `SLEEP 1.5`는 서버에 보내지 않고 그냥 기다립니다 (TTL 테스트용)
- **따옴표는 지원하지 않습니다.** `SET k "hello world"` 같은 건 못 씁니다

```
# 세션 4: TTL — SLEEP 사용 예시
SET k v EX 1
TTL k
GET k
SLEEP 1.5
GET k
TTL k
EXISTS k
```

**틀린 입력도 꼭 넣으세요.** 진짜 Redis가 뱉는 에러 문구를 그대로 맞춰야 하고,
서버가 죽지 않는지도 여기서 걸립니다.

```
SET
GET a b c
INCR name
COMMAND_THAT_DOES_NOT_EXIST
```

---

# 세션 한 번 따라하기 (S01 예시)

한 주가 실제로 이렇게 굴러갑니다. 이름이 `길동`이라고 하겠습니다.

## 세션 전 — 혼자 (2~3시간)

**① 이번 주 주제 확인**

- GitHub Issue [#1](../../issues/1) 읽기
- [`docs/backend-roadmap.md`](docs/backend-roadmap.md)의 S1 부분 읽기 — 특히 **설계 토론** 항목
- [redis.io의 PING 문서](https://redis.io/docs/latest/commands/ping/) 읽기

**② 브랜치 만들기**

```bash
git checkout main && git pull
git checkout -b 길동/s01-ping
```

**③ 테스트가 빨간불인지 먼저 확인**

```bash
./scripts/difftest.sh tests/cases/01-ping.txt    # 실패해야 정상
```

**④ AI와 구현**

```
S01 이슈 #1 진행할 거임. PING만 구현해줘.
설계 갈림길 나오면 구현하지 말고 선택지부터 보여줘.
```

여기서 AI가 "커넥션마다 스레드로 할까요, async로 할까요?"라고 되묻습니다.
**이 선택이 곧 발표 내용입니다.** 아무거나 고르지 말고 이유를 만들어 두세요.

**⑤ 초록불 만들기**

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
./scripts/difftest.sh
```

**⑥ 서버가 안 죽는지 확인** (아래 "서버가 죽지 않는지 확인하는 법")

**⑦ 설계 문서 쓰기 — 이게 발표 자료입니다**

`docs/designs/TEMPLATE.md`는 빈 양식입니다. 그대로 두고 **자기 이름 붙인 사본**을 만듭니다.

```bash
cp docs/designs/TEMPLATE.md docs/designs/s01-길동.md
# 열어서 채웁니다. 특히 'AI를 어떻게 썼나' 항목
```

**⑧ 설계 문서만 PR로 올리기 (세션 전날까지)**

다른 사람이 세션 전에 읽어야 하므로, 구현 코드는 내 브랜치에 두고 **문서 파일 하나만** 따로 올립니다.

```bash
# 구현 브랜치에 문서까지 커밋하고 올려둡니다 (문서에서 이 브랜치를 링크합니다)
git add docs/designs/s01-길동.md
git commit -m "docs: S01 설계 문서 (길동)"
git push -u origin 길동/s01-ping

# main에서 깨끗하게 딴 브랜치에 문서 파일만 가져와 PR을 올립니다
git checkout -b 길동/s01-design main
git checkout 길동/s01-ping -- docs/designs/s01-길동.md
git commit -m "docs: S01 설계 문서 (길동)"
git push -u origin 길동/s01-design
gh pr create --base main --title "docs: S01 설계 문서 (길동)"
```

**⑨ 다른 두 명의 설계 문서 PR을 읽고 옵니다.** 안 읽고 오면 세션이 성립하지 않습니다.

## 세션 당일 (약 2시간)

| 순서 | 내용 | 시간 |
| --- | --- | --- |
| 1 | 각자 발표 — 왜 이렇게 짰나 / 어떻게 짰나 / **AI를 어떻게 썼나** | 15분 × 3 |
| 2 | **갈린 지점 토론 — 같은 스펙인데 왜 다르게 나왔나** | 30분 |
| 3 | 이번 주 main에 반영할 구현 확정 (당번 순서대로) | 10분 |
| 4 | 다음 세션 주제 + 미리 생각해 올 설계 갈림길 확인 | 10분 |

**2번이 이 스터디의 전부입니다.** 셋이 똑같이 짜 왔다면 AI에게 너무 많이 맡긴 것입니다.
"AI가 그렇게 해줬어요"는 발표가 아닙니다. 왜 그 제안을 받아들였는지가 발표입니다.

## 세션 후

- 이번 주 **당번**만 구현 PR을 올립니다 (`길동/s01-ping` → `main`)
- 승인 1명 받고 squash 머지 → 다음 주에는 셋 다 이 main 위에서 다시 시작합니다
- 당번은 **매주 순서대로** 돌아갑니다. 누가 더 잘 짰는지와 무관합니다
- 당번이 아닌 사람의 구현은 머지되지 않고 각자 브랜치에 남습니다. 버리는 게 아니라 비교 재료입니다
- 토론 결론은 해당 세션 Issue에 댓글로 남깁니다

---

# 매번 하는 검증

끝났다고 말하기 전에 넷 다 돌리고 **출력을 확인합니다.** "통과할 것 같습니다"는 안 됩니다.

```bash
cargo fmt                      # 포맷
cargo clippy -- -D warnings    # 경고 0개
cargo test                     # 단위 테스트
./scripts/difftest.sh          # 진짜 Redis와 응답 비교
```

CI가 PR에서 같은 걸 다시 돌립니다. 로컬이 초록불이면 CI도 초록불입니다.

## 서버가 죽지 않는지 확인하는 법

difftest가 잡아주지 않는 영역입니다. 세션마다 한 번씩 해보세요.

```bash
# 터미널 A
cargo run -- --port 6380
```

```bash
# 터미널 B — 쓰레기 바이트를 던져도 서버는 살아있어야 합니다
printf 'garbage\r\n' | nc localhost 6380
redis-cli -p 6380 PING        # 여전히 PONG 이어야 함

# 명령을 쪼개서 보내도 처리되어야 합니다
exec 3<>/dev/tcp/localhost/6380; printf '*1\r\n$4\r\n' >&3; sleep 1; printf 'PING\r\n' >&3; head -c 7 <&3
```

---

# 로드맵

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

세션마다 GitHub Issue가 하나씩 있습니다. 이번 주 주제, 미리 생각해 올 설계 갈림길, 당번이 거기 적혀 있습니다.

---

# 협업 규칙

- `main` 직접 커밋 금지. 브랜치 → PR → **승인 1명**을 받고 squash 머지합니다.
- 브랜치 이름: 구현은 `<이름>/s03-set-get`, 설계 문서는 `<이름>/s03-design`
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

---

# 막힐 때

| 증상 | 해결 |
| --- | --- |
| `'redis-cli' 이(가) 없습니다` | `brew install redis` |
| `Could not connect to Redis` | `redis-server`가 안 떠 있습니다. 터미널 하나에서 `redis-server --port 6379` |
| `Address already in use` | 그 포트에 뭔가 떠 있습니다. `lsof -i :6380`으로 찾아서 끄세요 |
| difftest가 `우리 서버가 뜨지 않았습니다` | `cargo run -- --port 6380`이 단독으로 되는지 먼저 확인하세요 |
| 응답은 맞는데 difftest가 실패 | 공백·대소문자·에러 문구까지 **바이트 단위로** 같아야 합니다. `기대:` / `실제:` 줄을 그대로 비교하세요 |
| 커밋이 main에 막힘 | 정상입니다. 브랜치를 파고 PR을 올리세요 |

---

# 더 읽을 것

- [`docs/backend-roadmap.md`](docs/backend-roadmap.md) — 세션별 상세: 실무에서 어디 쓰이는지, 설계 토론 항목, 완료 기준
- [`docs/designs/TEMPLATE.md`](docs/designs/TEMPLATE.md) — 설계 문서 양식
- [`CLAUDE.md`](CLAUDE.md) — AI에게 주는 규칙 (금지 사항, 설명 의무). 사람도 한 번은 읽어야 합니다
- [RESP 프로토콜 스펙](https://redis.io/docs/latest/develop/reference/protocol-spec/)
- [Redis 커맨드 레퍼런스](https://redis.io/docs/latest/commands/)

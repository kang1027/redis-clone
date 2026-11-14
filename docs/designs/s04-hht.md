# S04 TTL — 한호택

- 구현 브랜치: `hht/s04-ttl`
- 걸린 시간: 약 3시간

## 1. 무엇을 만들었나

키에 만료 시간을 설정하고 조회하거나 제거할 수 있도록 다음 명령과 옵션을 구현했습니다.

- `EXPIRE key seconds`
- `TTL key`
- `PERSIST key`
- `SET key value EX seconds`
- `SET key value PX milliseconds`
- `SET key value NX`
- `SET key value XX`

TTL이 추가되면서 HashMap에 물리적으로 존재하는 키와 Redis에서 논리적으로 존재하는 키가 달라질 수 있게 되었습니다.

이미 만료됐지만 아직 HashMap에서 삭제되지 않은 키는 논리적으로 존재하지 않는 키로 처리합니다.

## 2. 설계 선택 — 이 문서의 핵심

| 갈림길 | 고른 것 | 이유 | 버린 선택지와 그 이유 |
| --- | --- | --- | --- |
| 만료 정리 방식 | Lazy expiration | 키에 접근할 때 만료 여부를 확인하고 제거하는 방식이 현재 구조에서 단순하고 TTL 동작 자체를 이해하기 좋다고 판단 | Active expiration: 별도의 주기적인 작업과 동시성 처리가 필요해 현재 단계에서는 복잡도가 커짐 |
| 만료 정보 저장 위치 | `Entry` 안에 value와 `expires_at`을 함께 저장 | 하나의 키에 대한 값과 TTL을 한 곳에서 관리할 수 있어 상태가 어긋날 가능성이 적음 | 값과 TTL을 별도 HashMap에 저장: 기존 구조 변경은 적지만 SET/DEL/PERSIST 등에서 두 Map을 항상 같이 관리해야 함 |
| 시간 표현 | `Instant` + `Duration` | 현재 clone은 영속성이 없고 실행 중인 프로세스 안에서 상대적인 TTL을 처리하면 되므로 적합하다고 판단 | 절대 시각/SystemTime: 재시작 후 TTL 복구까지 고려할 때 필요하지만 현재 범위에서는 필요하지 않음 |
| 시간 테스트 | 실제 시간 경과 + `SLEEP` | difftest에서 바로 사용할 수 있고 실제 Redis와 동일한 입력으로 비교하기 쉬움 | Fake Clock/Clock 주입: 테스트는 더 안정적이지만 현재 단계에서는 구조가 복잡해짐 |

**나중에 이 선택을 뒤집어야 하는 조건:**

서버 재시작 후에도 TTL을 복구하는 영속성을 구현한다면 `Instant` 기반 만료 시점 저장 방식을 다시 검토해야 합니다.

또한 만료된 키가 접근되지 않아 메모리에 오래 남는 문제가 커지면 lazy expiration만 사용하는 방식 대신 active expiration을 함께 사용하는 구조를 고려해야 합니다.

## 3. AI를 어떻게 썼나

- **첫 프롬프트에 뭐라고 썼나:**  
  S03에서 구현한 저장소를 이어서 S04의 TTL 기능을 구현하고 싶다고 요청했습니다.

- **AI 제안 중 거절한 것과 이유:**  
  처음에는 AI가 설계 선택을 빠르게 정하려 했지만, 선택 기준을 모르는 상태에서 결정을 요구받기보다 각 방식의 장단점과 선택 기준을 먼저 설명해달라고 요청했습니다.

- **AI가 틀렸던 것 / 스펙과 달랐던 것:**  
  `PERSIST` 구현 중 AI가 마지막 `b":0\r\n".to_vec()`에 `return`이 없어도 된다고 잘못 설명해 컴파일 오류가 발생했습니다.  
  `build_reply()` 함수의 마지막 표현식이 아니라 중간 `if` 블록이었기 때문에 `return`이 필요했습니다.

- **이해하지 못한 채 넘어간 코드:**  
  특별히 없음. `Instant`, `Duration`, lazy expiration, `let Some(...) = ... else`, `Option::take()` 등을 구현하면서 확인했습니다.

- **AI 없이 내가 결정한 것:**  
  만료 정리 방식은 lazy expiration을 선택했습니다.  
  만료 정보를 별도 Map으로 분리하지 않고 value와 함께 `Entry`에 저장하는 방식을 선택했습니다.

## 4. 내가 추가한 테스트 케이스

`tests/cases/04-ttl.txt`

```text
SET a 1
TTL a
EXPIRE a 10
TTL a
PERSIST a
TTL a

EXPIRE missing 10
TTL missing
PERSIST missing

SET ex-key hello EX 1
GET ex-key
SLEEP 1.2
GET ex-key
TTL ex-key

SET px-key hello PX 500
GET px-key
SLEEP 0.6
GET px-key

SET nx-key first NX
GET nx-key
SET nx-key second NX
GET nx-key

SET xx-key first XX
GET xx-key
SET xx-key first
SET xx-key second XX
GET xx-key
```

- 왜 이 케이스를 넣었나:  
  TTL이 없는 키, EXPIRE/PERSIST, 존재하지 않는 키, 실제 시간 경과 후 만료, EX/PX, NX/XX의 기본 동작을 실제 Redis와 비교하기 위해 넣었습니다.

- 넣었다가 깨진 것:  
  특별히 없음.

## 5. 막혔던 곳

TTL이 추가되면서 기존의

```rust
HashMap<Vec<u8>, Vec<u8>>
```

구조를 그대로 사용할 수 없어서 value와 만료 시점을 함께 가지는 `Entry`를 추가했습니다.

```rust
struct Entry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
}
```

또한 lazy expiration을 사용하기 때문에 HashMap에 키가 존재한다고 해서 Redis에서도 존재하는 키라고 바로 판단할 수 없었습니다.

예를 들어 이미 만료된 키가 HashMap에 남아 있는 상태에서 `SET key value NX`를 실행하면, 단순히 `contains_key()`만 사용할 경우 키가 존재한다고 잘못 판단하게 됩니다.

따라서 NX/XX의 존재 여부를 검사하기 전에 만료된 키를 먼저 확인하고 제거하도록 했습니다.

## 6. 새로 배운 Rust 개념

- `Duration`은 60초와 같은 시간의 길이를 표현합니다.
- `Instant`는 프로그램 내부 시간축에서 특정 시점을 표현합니다.
- `Instant::now() + Duration`으로 미래의 만료 시점을 만들 수 있습니다.
- `Option<Instant>`를 사용해 TTL이 없는 상태(`None`)와 있는 상태(`Some`)를 구분할 수 있습니다.
- `HashMap::get_mut()`을 사용하면 저장된 Entry를 수정할 수 있습니다.
- `let Some(entry) = ... else { ... }` 문법으로 Option에서 값을 꺼내고 `None`일 때 바로 처리할 수 있습니다.
- `Option::take()`는 값을 꺼내면서 기존 Option을 `None`으로 바꿀 수 있어 `PERSIST` 구현에 사용할 수 있습니다.

## 7. 검증 결과

```sh
$ cargo fmt --check
```

통과

```sh
$ cargo clippy -- -D warnings
```

통과

```sh
$ cargo test
```

통과

```sh
$ ./scripts/difftest.sh
```

통과

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- 남의 케이스로 내 서버를 돌려본 결과:
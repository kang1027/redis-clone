# S05 INCR / DECR / INCRBY — 한호택

- 구현 브랜치: `hht/s05-incr`
- 걸린 시간: 약 1.5시간

## 1. 무엇을 만들었나

Redis의 숫자 증가/감소 명령을 구현했습니다.

- `INCR key`
- `DECR key`
- `INCRBY key increment`

Redis의 String 값을 필요할 때 `i64` 정수로 해석해서 계산하고, 결과는 다시 바이트 형태로 저장합니다.

키가 존재하지 않으면 값을 `0`으로 간주하고 연산합니다.

숫자로 해석할 수 없는 값이나 `i64` 범위를 넘어가는 연산은 Redis와 동일하게 에러를 반환하도록 했습니다.

## 2. 설계 선택 — 이 문서의 핵심

| 갈림길 | 고른 것 | 이유 | 버린 선택지와 그 이유 |
| --- | --- | --- | --- |
| 숫자 저장 방식 | 기존 `Vec<u8>` 유지 | Redis에서 INCR 대상도 논리적으로 String이며, 현재 저장 구조를 유지하면서 계산할 때만 `i64`로 변환하는 것이 단순함 | 정수를 `i64`로 별도 저장: INCR 자체는 편해지지만 GET/SET에서 저장 표현을 구분해야 하고, 바로 다음 S06에서 List 등 여러 자료형을 다룰 예정이라 지금 enum 구조를 미리 도입할 필요가 적다고 판단 |
| overflow 처리 | `checked_add()` 사용 | `i64` 범위를 넘어가는 연산을 panic이나 잘못된 값 없이 명시적으로 감지할 수 있음 | 일반 `+` 연산: overflow 여부를 안전하게 처리하기 어려움 |
| INCR 계열 구현 | `increment_by()` 공통 함수로 분리 | 세 명령의 차이는 결국 얼마를 더하느냐이므로 중복 코드를 줄일 수 있음 | INCR/DECR/INCRBY 각각 구현: 같은 파싱·저장·overflow 코드가 반복됨 |

**나중에 이 선택을 뒤집어야 하는 조건:**

여러 Redis 자료형을 지원하면서 값의 내부 표현을 구분해야 한다면 `Vec<u8>`만 사용하는 현재 구조를 다시 검토합니다.

S06에서 List가 추가되면 `Value::String`, `Value::List` 같은 enum 구조가 필요할 수 있습니다.

## 3. AI를 어떻게 썼나

- **첫 프롬프트에 뭐라고 썼나:**  
  S04 구현을 이어서 S05의 `INCR / DECR / INCRBY`를 구현하고 싶다고 요청했습니다.

- **AI 제안 중 거절한 것과 이유:**  
  특별히 거절한 구현은 없었지만, 바로 코드를 작성하기 전에 왜 숫자를 별도의 정수 타입으로 저장하지 않고 기존 바이트 저장 구조를 유지하는지 먼저 설명을 듣고 설계 방향을 결정했습니다.

- **AI가 틀렸던 것 / 스펙과 달랐던 것:**  
  특별히 발견한 것은 없었습니다.

- **이해하지 못한 채 넘어간 코드:**  
  특별히 없음. `checked_add()`와 공통 함수로 분리하는 흐름을 확인하면서 구현했습니다.

- **내가 결정한 것:**  
  숫자를 별도 타입으로 저장하지 않고 기존 `Vec<u8>` 구조를 유지하는 방향에 동의했습니다.  
  S05에서는 숫자 연산 자체에 집중하고, 여러 데이터 타입을 표현하는 구조는 실제로 List가 등장하는 S06에서 다시 고민하기로 했습니다.

## 4. 내가 추가한 테스트 케이스

`tests/cases/05-incr.txt`

```text
SET count 10
INCR count
GET count

DECR count
GET count

INCRBY count 5
GET count

INCRBY count -3
GET count

INCR missing
GET missing

SET bad hello
INCR bad

SET max 9223372036854775807
INCR max

SET min -9223372036854775808
DECR min

SET ttl-count 10 EX 10
INCR ttl-count
TTL ttl-count
```

- 왜 이 케이스를 넣었나:  
  기본적인 증가/감소뿐 아니라 음수 `INCRBY`, 존재하지 않는 키, 숫자가 아닌 값, `i64` 최대/최소 overflow, 기존 TTL 유지까지 확인하기 위해 넣었습니다.

- 넣었다가 깨진 것:  
  특별히 없었습니다.

## 5. 막혔던 곳

큰 구현 문제는 없었습니다.

다만 처음에는 숫자를 실제 정수 타입으로 저장하는 편이 자연스럽지 않을까 생각했습니다.

현재 Redis clone에서는 String 값을 `Vec<u8>`로 저장하고 있기 때문에 정수만 별도 타입으로 저장하면 GET/SET에서도 내부 표현을 구분해야 합니다.

따라서 현재 구조를 유지하고 연산할 때만 다음 흐름으로 처리했습니다.

```text
Vec<u8>
→ i64로 파싱
→ 숫자 연산
→ 문자열로 변환
→ Vec<u8>로 다시 저장
```

또한 기존 키에 INCR 계열 명령을 실행할 때 Entry 전체를 교체하지 않고 `value`만 변경했습니다.

```rust
entry.value = next.to_string().into_bytes();
```

이렇게 하면 S04에서 저장한 `expires_at`은 그대로 유지되어 기존 TTL이 사라지지 않습니다.

## 6. 새로 배운 Rust 개념

- `checked_add()`는 정수 덧셈 결과가 타입의 범위를 넘는지 안전하게 확인할 수 있습니다.
- 계산이 가능하면 `Some(result)`, overflow가 발생하면 `None`을 반환합니다.
- `let Some(next) = ... else { ... }` 형태와 함께 사용해 overflow를 바로 에러 처리할 수 있습니다.
- 여러 명령의 공통 동작을 함수로 분리하면 `INCR`, `DECR`, `INCRBY`처럼 차이가 작은 명령들의 중복 구현을 줄일 수 있습니다.
- `to_string().into_bytes()`를 사용해 계산한 `i64` 값을 다시 Redis String 저장 형태인 `Vec<u8>`로 변환했습니다.

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
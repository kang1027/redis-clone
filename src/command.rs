//! 파싱된 명령(인자 목록)을 커맨드로 바꾸고, 실행해서 응답 바이트를 만듭니다.

use std::time::{Duration, Instant};

use crate::db::{Db, Entries, Entry};
use crate::reply::{bulk_string, error_reply, integer, null_bulk_string};

/// 인자 개수 검사를 통과한 커맨드입니다.
/// 인자를 복사하지 않고 넘겨받아(소유해서) 들고 있습니다.
enum Command {
    /// `PING`은 인자가 0개 또는 1개입니다.
    Ping(Option<Vec<u8>>),
    Echo(Vec<u8>),
    Set {
        key: Vec<u8>,
        value: Vec<u8>,
        condition: Option<SetCondition>,
        /// `EX`/`PX`가 없으면 None입니다. 그래도 덮어쓰므로 걸려 있던 TTL은 지워집니다.
        expires_at: Option<Instant>,
    },
    Get {
        key: Vec<u8>,
    },
    /// 키가 하나 이상 들어 있습니다.
    Del(Vec<Vec<u8>>),
    /// 키가 하나 이상 들어 있습니다.
    Exists(Vec<Vec<u8>>),
    /// `at`이 `now`보다 앞이거나 같으면 TTL을 거는 대신 키를 바로 지웁니다.
    Expire {
        key: Vec<u8>,
        at: Instant,
    },
    Ttl {
        key: Vec<u8>,
    },
    Persist {
        key: Vec<u8>,
    },
    DbSize,
    /// `INCR`은 `delta`가 1, `DECR`은 -1인 `INCRBY`입니다.
    IncrBy {
        key: Vec<u8>,
        delta: i64,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum SetCondition {
    /// `NX`: 키가 없을 때만 씁니다.
    Nx,
    /// `XX`: 키가 있을 때만 씁니다.
    Xx,
}

#[derive(Clone, Copy, PartialEq)]
enum TimeUnit {
    /// `EX`
    Seconds,
    /// `PX`
    Millis,
}

/// 커맨드 단계의 에러입니다. 프로토콜 에러와 달리 응답만 보내고 연결은 유지합니다.
enum CommandError {
    Unknown {
        name: Vec<u8>,
        args: Vec<Vec<u8>>,
    },
    /// 문구에는 사용자가 친 대소문자와 상관없이 소문자 이름이 들어갑니다.
    WrongArity(&'static str),
    /// `SET`의 모르는 옵션이나 같이 쓸 수 없는 옵션(`NX XX`, `EX PX`)입니다.
    Syntax,
    NotInteger,
    Overflow,
    /// 이름이 들어가는 자리는 `'set'`, `'expire'`처럼 소문자입니다.
    InvalidExpireTime(&'static str),
    /// `EXPIRE` 뒤에 붙은 옵션입니다. 옵션(NX/XX/GT/LT)은 이번 범위가 아니라 전부 여기로 옵니다.
    UnsupportedOption(Vec<u8>),
}

/// `now`는 이 명령을 처리하는 시각입니다. 테스트는 여기에 "200초 뒤"를 넘겨서 sleep 없이 만료를 확인합니다.
pub fn build_reply(command: Vec<Vec<u8>>, db: &Db, now: Instant) -> Vec<u8> {
    let mut parts = command.into_iter();
    // 빈 배열(`*0\r\n`)이나 빈 인라인 줄에는 진짜 Redis도 아무 응답을 하지 않습니다.
    let Some(name) = parts.next() else {
        return Vec::new();
    };

    match Command::parse(name, parts.collect(), now) {
        Ok(command) => command.execute(db, now),
        Err(error) => error_reply(&error.message()),
    }
}

impl Command {
    /// 인자 개수, 옵션 문법, 인자로 온 숫자는 여기서 전부 확인합니다.
    /// `execute`에서 나는 에러는 저장된 값을 봐야 알 수 있는 것(`INCR` 계열)뿐입니다.
    fn parse(name: Vec<u8>, args: Vec<Vec<u8>>, now: Instant) -> Result<Command, CommandError> {
        match name.to_ascii_uppercase().as_slice() {
            b"PING" => {
                if args.len() > 1 {
                    return Err(CommandError::WrongArity("ping"));
                }
                // 인자가 없으면 None, 하나면 Some(그 인자)
                Ok(Command::Ping(args.into_iter().next()))
            }
            // PING과 달리 ECHO는 인자가 정확히 하나여야 합니다.
            // `try_from`은 Vec의 길이가 딱 1일 때만 배열로 바꿔 줍니다. 원소는 복사하지 않고 옮깁니다.
            b"ECHO" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([message]) => Ok(Command::Echo(message)),
                Err(_) => Err(CommandError::WrongArity("echo")),
            },
            b"SET" => {
                let mut args = args.into_iter();
                let (Some(key), Some(value)) = (args.next(), args.next()) else {
                    return Err(CommandError::WrongArity("set"));
                };
                // 키와 값 뒤에 남은 인자는 전부 옵션 자리입니다.
                parse_set(key, value, args, now)
            }
            b"GET" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::Get { key }),
                Err(_) => Err(CommandError::WrongArity("get")),
            },
            b"DEL" => {
                if args.is_empty() {
                    return Err(CommandError::WrongArity("del"));
                }
                Ok(Command::Del(args))
            }
            b"EXISTS" => {
                if args.is_empty() {
                    return Err(CommandError::WrongArity("exists"));
                }
                Ok(Command::Exists(args))
            }
            b"EXPIRE" => {
                let mut args = args.into_iter();
                let (Some(key), Some(seconds)) = (args.next(), args.next()) else {
                    return Err(CommandError::WrongArity("expire"));
                };
                // 진짜 Redis는 숫자보다 옵션을 먼저 봅니다. (`EXPIRE k abc foo` → Unsupported option foo)
                if let Some(option) = args.next() {
                    return Err(CommandError::UnsupportedOption(option));
                }
                let seconds = parse_integer(&seconds).ok_or(CommandError::NotInteger)?;
                // SET의 EX와 달리 0이나 음수도 받습니다. 그때 `after_millis`는 `now`를 돌려줘서 바로 만료됩니다.
                // 초를 밀리초로 바꾸다 i64를 넘치면(`checked_mul`이 None) 진짜 Redis도 invalid expire time입니다.
                let at = seconds
                    .checked_mul(1000)
                    .and_then(|millis| after_millis(now, millis))
                    .ok_or(CommandError::InvalidExpireTime("expire"))?;
                Ok(Command::Expire { key, at })
            }
            b"TTL" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::Ttl { key }),
                Err(_) => Err(CommandError::WrongArity("ttl")),
            },
            b"PERSIST" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::Persist { key }),
                Err(_) => Err(CommandError::WrongArity("persist")),
            },
            b"DBSIZE" => {
                if !args.is_empty() {
                    return Err(CommandError::WrongArity("dbsize"));
                }
                Ok(Command::DbSize)
            }
            b"INCR" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::IncrBy { key, delta: 1 }),
                Err(_) => Err(CommandError::WrongArity("incr")),
            },
            b"DECR" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::IncrBy { key, delta: -1 }),
                Err(_) => Err(CommandError::WrongArity("decr")),
            },
            b"INCRBY" => match <[Vec<u8>; 2]>::try_from(args) {
                Ok([key, delta]) => {
                    let delta = parse_integer(&delta).ok_or(CommandError::NotInteger)?;
                    Ok(Command::IncrBy { key, delta })
                }
                Err(_) => Err(CommandError::WrongArity("incrby")),
            },
            _ => Err(CommandError::Unknown { name, args }),
        }
    }

    /// 저장소가 필요한 커맨드만 락을 잡습니다. 락은 각 갈래 안의 `entries`가 사라질 때 풀립니다.
    /// 키를 읽을 때는 항상 `live`를 거칩니다. 만료된 키는 거기서 지워지고 없는 키로 보입니다.
    fn execute(self, db: &Db, now: Instant) -> Vec<u8> {
        match self {
            Command::Ping(None) => b"+PONG\r\n".to_vec(),
            Command::Ping(Some(message)) => bulk_string(&message),
            Command::Echo(message) => bulk_string(&message),
            Command::Set {
                key,
                value,
                condition,
                expires_at,
            } => {
                let mut entries = db.lock();
                // 있는지 보기와 쓰기가 락 한 번 안에서 끝납니다. 그 사이에 다른 커넥션이 끼어들 수 없어서
                // `SET lock v NX`가 "먼저 온 한 명만 성공"하는 락이 됩니다.
                let exists = entries.live(&key, now).is_some();
                let allowed = match condition {
                    None => true,
                    Some(SetCondition::Nx) => !exists,
                    Some(SetCondition::Xx) => exists,
                };
                if !allowed {
                    return null_bulk_string();
                }
                // key와 value를 복사하지 않고 저장소로 옮깁니다. 있던 값과 TTL은 통째로 바뀝니다.
                entries.insert(key, Entry { value, expires_at });
                b"+OK\r\n".to_vec()
            }
            Command::Get { key } => {
                let mut entries = db.lock();
                // 값을 저장소 밖으로 복사해 꺼내지 않고, 락을 잡은 채 바로 응답 바이트로 씁니다.
                match entries.live(&key, now) {
                    Some(entry) => bulk_string(&entry.value),
                    None => null_bulk_string(),
                }
            }
            Command::Del(keys) => {
                let mut entries = db.lock();
                let mut deleted = 0;
                // 같은 키가 두 번 오면 두 번째에는 이미 지워져 있어서 세지 않습니다. (`DEL k k` → 1)
                for key in &keys {
                    if entries.remove(key, now) {
                        deleted += 1;
                    }
                }
                integer(deleted)
            }
            Command::Exists(keys) => {
                let mut entries = db.lock();
                let mut found = 0;
                // DEL과 달리 같은 키가 여러 번 오면 그만큼 셉니다. (`EXISTS k k` → 2)
                for key in &keys {
                    if entries.live(key, now).is_some() {
                        found += 1;
                    }
                }
                integer(found)
            }
            Command::Expire { key, at } => {
                let mut entries = db.lock();
                // 이미 지난 시각이면 TTL을 거는 대신 바로 지웁니다. 키가 있었으면 1입니다. (`EXPIRE k 0` → 1)
                if at <= now {
                    return integer(i64::from(entries.remove(&key, now)));
                }
                match entries.live(&key, now) {
                    Some(entry) => {
                        entry.expires_at = Some(at);
                        integer(1)
                    }
                    None => integer(0),
                }
            }
            Command::Ttl { key } => {
                let mut entries = db.lock();
                // 바깥 Option은 키가 있는지, 안쪽 Option은 TTL이 걸려 있는지입니다.
                match entries.live(&key, now).map(|entry| entry.expires_at) {
                    None => integer(-2),
                    Some(None) => integer(-1),
                    Some(Some(at)) => {
                        // 진짜 Redis처럼 초 단위로 반올림합니다. 99.989초 남았으면 99가 아니라 100입니다.
                        let millis = at.saturating_duration_since(now).as_millis();
                        integer(i64::try_from((millis + 500) / 1000).unwrap_or(i64::MAX))
                    }
                }
            }
            Command::Persist { key } => {
                let mut entries = db.lock();
                match entries.live(&key, now) {
                    // `take`는 TTL을 꺼내고 그 자리를 None으로 비웁니다. 꺼낸 게 있었으면 1입니다.
                    Some(entry) => integer(i64::from(entry.expires_at.take().is_some())),
                    None => integer(0),
                }
            }
            // 만료됐지만 아직 안 지워진 키도 셉니다. 진짜 Redis도 같아서, active expiration이 없으면 줄지 않습니다.
            Command::DbSize => integer(i64::try_from(db.lock().count()).unwrap_or(i64::MAX)),
            Command::IncrBy { key, delta } => {
                let mut entries = db.lock();
                // 읽기, 더하기, 쓰기가 락 한 번 안에서 끝납니다. 앱이 GET → +1 → SET으로 나눠 보내면
                // 그 사이에 다른 커넥션이 끼어들어 값을 잃어버립니다(lost update). 이걸 막는 게 INCR입니다.
                match incr_by(&mut entries, key, delta, now) {
                    Ok(number) => integer(number),
                    Err(error) => error_reply(&error.message()),
                }
            }
        }
    }
}

/// 값은 숫자여도 바이트로 둡니다. 매번 읽어서 정수로 바꾸고, 결과를 다시 바이트로 씁니다.
/// 정수로 따로 저장하면 `SET k 010`을 `GET`했을 때 `10`이 되지 않게 신경 써야 합니다.
/// `entry.value`만 바꾸므로 걸려 있던 TTL은 그대로입니다.
fn incr_by(
    entries: &mut Entries,
    key: Vec<u8>,
    delta: i64,
    now: Instant,
) -> Result<i64, CommandError> {
    let Some(entry) = entries.live(&key, now) else {
        // 없는 키(만료된 키 포함)는 0에서 시작합니다. 0 + delta는 넘칠 수 없습니다.
        let value = delta.to_string().into_bytes();
        entries.insert(
            key,
            Entry {
                value,
                expires_at: None,
            },
        );
        return Ok(delta);
    };
    let current = parse_integer(&entry.value).ok_or(CommandError::NotInteger)?;
    let next = current.checked_add(delta).ok_or(CommandError::Overflow)?;
    entry.value = next.to_string().into_bytes();
    Ok(next)
}

/// `SET key value [NX | XX] [EX seconds | PX milliseconds]`
/// 옵션 문법을 끝까지 본 뒤에 숫자를 봅니다. 진짜 Redis도 그래서 `SET k v EX abc NX XX`는 syntax error입니다.
fn parse_set(
    key: Vec<u8>,
    value: Vec<u8>,
    mut options: impl Iterator<Item = Vec<u8>>,
    now: Instant,
) -> Result<Command, CommandError> {
    let mut condition = None;
    // 숫자는 아직 글자 그대로 받아 둡니다. 같은 옵션을 또 쓰면(`EX 10 EX 20`) 뒤의 것이 이깁니다.
    let mut expire: Option<(Vec<u8>, TimeUnit)> = None;

    while let Some(option) = options.next() {
        match option.to_ascii_uppercase().as_slice() {
            // 같이 쓸 수 없는 짝(NX와 XX, EX와 PX)이 이미 있으면 match guard에서 걸러져 맨 아래 syntax error로 갑니다.
            b"NX" if condition != Some(SetCondition::Xx) => condition = Some(SetCondition::Nx),
            b"XX" if condition != Some(SetCondition::Nx) => condition = Some(SetCondition::Xx),
            b"EX" if !matches!(expire, Some((_, TimeUnit::Millis))) => {
                let amount = options.next().ok_or(CommandError::Syntax)?;
                expire = Some((amount, TimeUnit::Seconds));
            }
            b"PX" if !matches!(expire, Some((_, TimeUnit::Seconds))) => {
                let amount = options.next().ok_or(CommandError::Syntax)?;
                expire = Some((amount, TimeUnit::Millis));
            }
            // GET, KEEPTTL, EXAT, PXAT 같은 진짜 옵션도 이번 범위가 아니라서 여기로 옵니다.
            _ => return Err(CommandError::Syntax),
        }
    }

    let expires_at = match expire {
        None => None,
        Some((amount, unit)) => {
            let amount = parse_integer(&amount).ok_or(CommandError::NotInteger)?;
            let millis = match unit {
                TimeUnit::Seconds => amount.checked_mul(1000),
                TimeUnit::Millis => Some(amount),
            };
            // EXPIRE와 달리 SET은 0 이하를 에러로 봅니다. (`SET k v EX 0` → invalid expire time)
            let at = millis
                .filter(|&millis| millis > 0)
                .and_then(|millis| after_millis(now, millis))
                .ok_or(CommandError::InvalidExpireTime("set"))?;
            Some(at)
        }
    };

    Ok(Command::Set {
        key,
        value,
        condition,
        expires_at,
    })
}

/// `now`에서 `millis`밀리초 뒤의 시각입니다. 0 이하면 `now` 그대로라 이미 만료된 시각이 됩니다.
/// `Instant`로 나타낼 수 없을 만큼 크면 None입니다.
fn after_millis(now: Instant, millis: i64) -> Option<Instant> {
    match u64::try_from(millis) {
        Ok(millis) => now.checked_add(Duration::from_millis(millis)),
        // 음수는 u64로 못 바꿉니다
        Err(_) => Some(now),
    }
}

/// 진짜 Redis(string2ll)처럼 `+10`, `010`, `-0`, 공백이 낀 숫자는 정수로 보지 않습니다.
/// Rust의 `parse`는 이런 것도 받아 주므로, 다시 글자로 바꿨을 때 입력과 똑같은지 확인합니다.
fn parse_integer(bytes: &[u8]) -> Option<i64> {
    let number: i64 = std::str::from_utf8(bytes).ok()?.parse().ok()?;
    (number.to_string().as_bytes() == bytes).then_some(number)
}

impl CommandError {
    fn message(&self) -> Vec<u8> {
        match self {
            CommandError::Unknown { name, args } => {
                let mut message =
                    format!("ERR unknown command '{}'", String::from_utf8_lossy(name));
                if !args.is_empty() {
                    message.push_str(", with args beginning with: ");
                    for argument in args {
                        // 진짜 Redis는 인자마다 뒤에 공백을 하나씩 붙입니다.
                        message.push_str(&format!("'{}' ", String::from_utf8_lossy(argument)));
                    }
                }
                message.into_bytes()
            }
            CommandError::WrongArity(name) => {
                format!("ERR wrong number of arguments for '{name}' command").into_bytes()
            }
            CommandError::Syntax => b"ERR syntax error".to_vec(),
            CommandError::NotInteger => b"ERR value is not an integer or out of range".to_vec(),
            CommandError::Overflow => b"ERR increment or decrement would overflow".to_vec(),
            CommandError::InvalidExpireTime(name) => {
                format!("ERR invalid expire time in '{name}' command").into_bytes()
            }
            CommandError::UnsupportedOption(option) => {
                let mut message = b"ERR Unsupported option ".to_vec();
                // 에러는 한 줄이라, 진짜 Redis는 옵션 안의 \r과 \n을 공백으로 바꿔서 보냅니다.
                message.extend(option.iter().map(|&byte| match byte {
                    b'\r' | b'\n' => b' ',
                    other => other,
                }));
                message
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|part| part.as_bytes().to_vec()).collect()
    }

    /// 저장소를 쓰지 않는 커맨드용입니다. 매번 빈 저장소로 실행합니다.
    fn run(parts: &[&str]) -> Vec<u8> {
        build_reply(words(parts), &Db::default(), Instant::now())
    }

    /// 같은 저장소에 커맨드를 이어서 보냅니다.
    fn run_on(db: &Db, parts: &[&str]) -> Vec<u8> {
        build_reply(words(parts), db, Instant::now())
    }

    /// 시각을 정해서 보냅니다. `start + 200초`를 넘기면 sleep 없이 "200초 뒤"가 됩니다.
    fn run_at(db: &Db, now: Instant, parts: &[&str]) -> Vec<u8> {
        build_reply(words(parts), db, now)
    }

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn ping_응답이_진짜_redis와_같다() {
        assert_eq!(run(&["PING"]), b"+PONG\r\n".to_vec());
        assert_eq!(run(&["ping"]), b"+PONG\r\n".to_vec());
        assert_eq!(run(&["PING", "hello"]), b"$5\r\nhello\r\n".to_vec());
        assert_eq!(
            run(&["PING", "a", "b"]),
            b"-ERR wrong number of arguments for 'ping' command\r\n".to_vec()
        );
    }

    #[test]
    fn echo_응답이_진짜_redis와_같다() {
        assert_eq!(run(&["ECHO", "hello"]), b"$5\r\nhello\r\n".to_vec());
        assert_eq!(run(&["echo", "hello"]), b"$5\r\nhello\r\n".to_vec());
        // PING과 다릅니다. 인자가 없거나 둘 이상이면 에러입니다.
        let wrong = b"-ERR wrong number of arguments for 'echo' command\r\n".to_vec();
        assert_eq!(run(&["ECHO"]), wrong);
        assert_eq!(run(&["ECHO", "a", "b"]), wrong);
    }

    #[test]
    fn 한글은_바이트_길이로_응답한다() {
        assert_eq!(run(&["ECHO", "한글"]), "$6\r\n한글\r\n".as_bytes().to_vec());
    }

    #[test]
    fn 모르는_커맨드_문구가_진짜_redis와_같다() {
        assert_eq!(
            run(&["garbage"]),
            b"-ERR unknown command 'garbage'\r\n".to_vec()
        );
        assert_eq!(
            run(&["foo", "a", "b"]),
            b"-ERR unknown command 'foo', with args beginning with: 'a' 'b' \r\n".to_vec()
        );
    }

    #[test]
    fn set한_값을_get으로_읽고_덮어쓴다() {
        let db = Db::default();
        assert_eq!(run_on(&db, &["SET", "name", "hello"]), b"+OK\r\n".to_vec());
        assert_eq!(run_on(&db, &["GET", "name"]), b"$5\r\nhello\r\n".to_vec());
        assert_eq!(run_on(&db, &["set", "name", "world"]), b"+OK\r\n".to_vec());
        assert_eq!(run_on(&db, &["get", "name"]), b"$5\r\nworld\r\n".to_vec());
        // 키는 대소문자를 가립니다
        assert_eq!(run_on(&db, &["GET", "NAME"]), b"$-1\r\n".to_vec());
    }

    #[test]
    fn 없는_키와_빈_값은_다른_바이트다() {
        // difftest(redis-cli 출력 비교)에서는 둘 다 빈 줄로 보여서 여기서 바이트로 확인합니다.
        let db = Db::default();
        assert_eq!(run_on(&db, &["GET", "nope"]), b"$-1\r\n".to_vec());
        run_on(&db, &["SET", "empty", ""]);
        assert_eq!(run_on(&db, &["GET", "empty"]), b"$0\r\n\r\n".to_vec());
    }

    #[test]
    fn del과_exists는_정수로_센다() {
        // 정수(`:1`)와 문자열 "1"(`$1\r\n1`)도 difftest에서는 구분되지 않습니다.
        let db = Db::default();
        run_on(&db, &["SET", "k", "v"]);
        assert_eq!(run_on(&db, &["EXISTS", "k"]), b":1\r\n".to_vec());
        assert_eq!(
            run_on(&db, &["EXISTS", "k", "k", "nope"]),
            b":2\r\n".to_vec()
        );
        assert_eq!(run_on(&db, &["DEL", "k", "k"]), b":1\r\n".to_vec());
        assert_eq!(run_on(&db, &["DEL", "k"]), b":0\r\n".to_vec());
        assert_eq!(run_on(&db, &["EXISTS", "k"]), b":0\r\n".to_vec());
    }

    #[test]
    fn 인자가_틀리면_진짜_redis와_같은_에러다() {
        let wrong = |name: &str| {
            format!("-ERR wrong number of arguments for '{name}' command\r\n").into_bytes()
        };
        assert_eq!(run(&["SET"]), wrong("set"));
        assert_eq!(run(&["SET", "k"]), wrong("set"));
        assert_eq!(run(&["GET"]), wrong("get"));
        assert_eq!(run(&["GET", "k", "extra"]), wrong("get"));
        assert_eq!(run(&["DEL"]), wrong("del"));
        assert_eq!(run(&["EXISTS"]), wrong("exists"));
        assert_eq!(run(&["EXPIRE", "k"]), wrong("expire"));
        assert_eq!(run(&["TTL"]), wrong("ttl"));
        assert_eq!(run(&["TTL", "a", "b"]), wrong("ttl"));
        assert_eq!(run(&["PERSIST"]), wrong("persist"));
        assert_eq!(run(&["PERSIST", "a", "b"]), wrong("persist"));
        assert_eq!(run(&["DBSIZE", "x"]), wrong("dbsize"));
        // 키와 값 뒤에 남는 인자는 개수 에러가 아니라 모르는 옵션입니다
        assert_eq!(
            run(&["SET", "k", "v", "foo"]),
            b"-ERR syntax error\r\n".to_vec()
        );
    }

    #[test]
    fn 에러가_난_set은_저장하지_않는다() {
        let db = Db::default();
        run_on(&db, &["SET", "k", "v", "foo"]);
        run_on(&db, &["SET", "k", "v", "EX", "0"]);
        assert_eq!(run_on(&db, &["GET", "k"]), b"$-1\r\n".to_vec());
    }

    #[test]
    fn ttl은_초로_반올림한다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "k", "v", "EX", "100"]);
        // 99.990초 남음 → 100, 99.600초 → 100, 99.400초 → 99
        assert_eq!(run_at(&db, after(start, 10), &["TTL", "k"]), b":100\r\n");
        assert_eq!(run_at(&db, after(start, 400), &["TTL", "k"]), b":100\r\n");
        assert_eq!(run_at(&db, after(start, 600), &["TTL", "k"]), b":99\r\n");
        // TTL이 없는 키는 -1, 없는 키는 -2
        run_at(&db, start, &["SET", "plain", "v"]);
        assert_eq!(run_at(&db, start, &["TTL", "plain"]), b":-1\r\n");
        assert_eq!(run_at(&db, start, &["TTL", "nope"]), b":-2\r\n");
    }

    #[test]
    fn 만료된_키는_어디서_봐도_없다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "k", "v", "PX", "100"]);
        assert_eq!(run_at(&db, after(start, 99), &["GET", "k"]), b"$1\r\nv\r\n");
        // 100ms가 지나면 만료입니다. 각 커맨드마다 다시 만들어 확인합니다.
        let later = after(start, 100);
        let fresh = || {
            let db = Db::default();
            run_at(&db, start, &["SET", "k", "v", "PX", "100"]);
            db
        };
        assert_eq!(run_at(&fresh(), later, &["GET", "k"]), b"$-1\r\n");
        assert_eq!(run_at(&fresh(), later, &["EXISTS", "k"]), b":0\r\n");
        assert_eq!(run_at(&fresh(), later, &["TTL", "k"]), b":-2\r\n");
        assert_eq!(run_at(&fresh(), later, &["DEL", "k"]), b":0\r\n");
        assert_eq!(run_at(&fresh(), later, &["PERSIST", "k"]), b":0\r\n");
        assert_eq!(run_at(&fresh(), later, &["EXPIRE", "k", "100"]), b":0\r\n");
        assert_eq!(
            run_at(&fresh(), later, &["SET", "k", "w", "XX"]),
            b"$-1\r\n"
        );
        assert_eq!(
            run_at(&fresh(), later, &["SET", "k", "w", "NX"]),
            b"+OK\r\n"
        );
    }

    #[test]
    fn nx_xx로_못_쓰면_nil이다() {
        // difftest에서는 nil이 빈 줄로 보여서 바이트로 확인합니다.
        let db = Db::default();
        assert_eq!(
            run_on(&db, &["SET", "lock", "a", "NX", "EX", "30"]),
            b"+OK\r\n"
        );
        assert_eq!(
            run_on(&db, &["SET", "lock", "b", "NX", "EX", "30"]),
            b"$-1\r\n"
        );
        assert_eq!(run_on(&db, &["GET", "lock"]), b"$1\r\na\r\n");
        assert_eq!(run_on(&db, &["SET", "nope", "v", "XX"]), b"$-1\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "nope"]), b":0\r\n");
        assert_eq!(run_on(&db, &["SET", "lock", "c", "xx"]), b"+OK\r\n");
        assert_eq!(run_on(&db, &["GET", "lock"]), b"$1\r\nc\r\n");
    }

    #[test]
    fn 옵션_없는_set은_ttl을_지운다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "k", "v", "EX", "100"]);
        run_at(&db, start, &["SET", "k", "v2", "XX"]);
        assert_eq!(run_at(&db, start, &["TTL", "k"]), b":-1\r\n");
        // 200초 뒤에도 남아 있습니다
        assert_eq!(
            run_at(&db, after(start, 200_000), &["GET", "k"]),
            b"$2\r\nv2\r\n"
        );
    }

    #[test]
    fn 같은_옵션을_또_쓰면_뒤의_것이_이긴다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "k", "v", "EX", "10", "EX", "20"]);
        assert_eq!(run_at(&db, start, &["TTL", "k"]), b":20\r\n");
        run_at(&db, start, &["SET", "k", "v", "NX", "NX"]);
        run_at(&db, start, &["SET", "k", "v", "px", "3000", "xx"]);
        assert_eq!(run_at(&db, start, &["TTL", "k"]), b":3\r\n");
    }

    #[test]
    fn expire와_persist() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "k", "v"]);
        assert_eq!(run_at(&db, start, &["EXPIRE", "k", "100"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["EXPIRE", "nope", "100"]), b":0\r\n");
        // 다시 걸면 짧아져도 덮어씁니다
        assert_eq!(run_at(&db, start, &["EXPIRE", "k", "50"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["TTL", "k"]), b":50\r\n");
        assert_eq!(run_at(&db, start, &["PERSIST", "k"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["PERSIST", "k"]), b":0\r\n");
        assert_eq!(run_at(&db, start, &["TTL", "k"]), b":-1\r\n");
        assert_eq!(run_at(&db, start, &["PERSIST", "nope"]), b":0\r\n");
    }

    #[test]
    fn 영이나_음수로_expire하면_바로_지운다() {
        let db = Db::default();
        run_on(&db, &["SET", "k", "v"]);
        assert_eq!(run_on(&db, &["EXPIRE", "k", "0"]), b":1\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "k"]), b":0\r\n");
        run_on(&db, &["SET", "k", "v"]);
        assert_eq!(run_on(&db, &["EXPIRE", "k", "-5"]), b":1\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "k"]), b":0\r\n");
        assert_eq!(run_on(&db, &["EXPIRE", "nope", "-5"]), b":0\r\n");
    }

    #[test]
    fn 옵션과_숫자_에러가_진짜_redis와_같다() {
        let syntax = b"-ERR syntax error\r\n".to_vec();
        let not_integer = b"-ERR value is not an integer or out of range\r\n".to_vec();
        let invalid_set = b"-ERR invalid expire time in 'set' command\r\n".to_vec();
        let invalid_expire = b"-ERR invalid expire time in 'expire' command\r\n".to_vec();

        assert_eq!(run(&["SET", "k", "v", "NX", "XX"]), syntax);
        assert_eq!(run(&["SET", "k", "v", "EX", "10", "PX", "100"]), syntax);
        assert_eq!(run(&["SET", "k", "v", "EX"]), syntax);
        assert_eq!(run(&["SET", "k", "v", "KEEPTTL"]), syntax);
        // 문법 에러가 숫자 에러보다 먼저입니다
        assert_eq!(run(&["SET", "k", "v", "EX", "abc", "NX", "XX"]), syntax);

        for bad in ["abc", "1.5", "+10", "010", "-0", " 10", ""] {
            assert_eq!(
                run(&["SET", "k", "v", "EX", bad]),
                not_integer,
                "EX {bad:?}"
            );
            assert_eq!(run(&["EXPIRE", "k", bad]), not_integer, "EXPIRE {bad:?}");
        }

        assert_eq!(run(&["SET", "k", "v", "EX", "0"]), invalid_set);
        assert_eq!(run(&["SET", "k", "v", "EX", "-1"]), invalid_set);
        assert_eq!(run(&["SET", "k", "v", "PX", "0"]), invalid_set);
        // 초 → 밀리초에서 i64를 넘칩니다
        assert_eq!(
            run(&["SET", "k", "v", "EX", "9223372036854776"]),
            invalid_set
        );
        assert_eq!(run(&["EXPIRE", "k", "9999999999999999"]), invalid_expire);
        assert_eq!(run(&["EXPIRE", "k", "-9223372036854776"]), invalid_expire);
    }

    #[test]
    fn expire_옵션은_모르는_옵션이다() {
        assert_eq!(
            run(&["EXPIRE", "k", "10", "foo"]),
            b"-ERR Unsupported option foo\r\n".to_vec()
        );
        // 진짜 Redis도 숫자보다 옵션을 먼저 봅니다
        assert_eq!(
            run(&["EXPIRE", "k", "abc", "foo"]),
            b"-ERR Unsupported option foo\r\n".to_vec()
        );
        // 에러 줄 안에 \r\n이 들어가지 않게 공백으로 바꿉니다
        assert_eq!(
            run(&["EXPIRE", "k", "10", "a\r\nb!"]),
            b"-ERR Unsupported option a  b!\r\n".to_vec()
        );
    }

    #[test]
    fn dbsize는_만료된_키도_지워지기_전까지_센다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "a", "v", "PX", "100"]);
        run_at(&db, start, &["SET", "b", "v"]);
        let later = after(start, 200);
        // a는 만료됐지만 아무도 안 읽어서 아직 저장소에 있습니다
        assert_eq!(run_at(&db, later, &["DBSIZE"]), b":2\r\n");
        // 백그라운드 스레드가 하는 일을 직접 한 번 부릅니다
        db.lock().remove_expired(later);
        assert_eq!(run_at(&db, later, &["DBSIZE"]), b":1\r\n");
    }

    #[test]
    fn incr_계열은_정수로_답하고_get은_문자열로_답한다() {
        // 정수(`:1`)와 문자열 "1"(`$1\r\n1`)은 difftest에서 구분되지 않습니다.
        let db = Db::default();
        assert_eq!(run_on(&db, &["INCR", "n"]), b":1\r\n");
        assert_eq!(run_on(&db, &["incr", "n"]), b":2\r\n");
        assert_eq!(run_on(&db, &["GET", "n"]), b"$1\r\n2\r\n");
        assert_eq!(run_on(&db, &["DECR", "d"]), b":-1\r\n");
        assert_eq!(run_on(&db, &["INCRBY", "b", "5"]), b":5\r\n");
        assert_eq!(run_on(&db, &["INCRBY", "b", "-7"]), b":-2\r\n");
        assert_eq!(run_on(&db, &["GET", "b"]), b"$2\r\n-2\r\n");
        // 0을 더해도 키가 생깁니다
        assert_eq!(run_on(&db, &["INCRBY", "z", "0"]), b":0\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "z"]), b":1\r\n");
    }

    #[test]
    fn 정수가_아닌_값은_에러이고_그대로_남는다() {
        let not_integer = b"-ERR value is not an integer or out of range\r\n".to_vec();
        // 빈 값, 공백, UTF-8이 아닌 바이트는 redis-cli 케이스로 못 보내서 여기서 봅니다.
        let bad_values: [&[u8]; 10] = [
            b"abc",
            b"",
            b"1.5",
            b"+10",
            b"010",
            b"-0",
            b" 10",
            b"10 ",
            b"9223372036854775808",
            b"\xff\xfe",
        ];
        for bad in bad_values {
            let db = Db::default();
            build_reply(
                vec![b"SET".to_vec(), b"k".to_vec(), bad.to_vec()],
                &db,
                Instant::now(),
            );
            assert_eq!(run_on(&db, &["INCR", "k"]), not_integer, "{bad:?}");
            assert_eq!(run_on(&db, &["DECR", "k"]), not_integer, "{bad:?}");
            assert_eq!(run_on(&db, &["INCRBY", "k", "1"]), not_integer, "{bad:?}");
            assert_eq!(
                run_on(&db, &["GET", "k"]),
                [format!("${}\r\n", bad.len()).as_bytes(), bad, b"\r\n"].concat(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn incrby_인자가_정수가_아니면_키를_만들지_않는다() {
        let not_integer = b"-ERR value is not an integer or out of range\r\n".to_vec();
        let db = Db::default();
        for bad in [
            "abc",
            "",
            "1.5",
            "+1",
            "010",
            "-0",
            " 1",
            "9223372036854775808",
        ] {
            assert_eq!(
                run_on(&db, &["INCRBY", "nope", bad]),
                not_integer,
                "{bad:?}"
            );
        }
        assert_eq!(run_on(&db, &["EXISTS", "nope"]), b":0\r\n");
    }

    #[test]
    fn 넘치면_다른_에러이고_값은_그대로다() {
        let overflow = b"-ERR increment or decrement would overflow\r\n".to_vec();
        let db = Db::default();
        run_on(&db, &["SET", "k", "9223372036854775807"]);
        assert_eq!(run_on(&db, &["INCR", "k"]), overflow);
        assert_eq!(
            run_on(&db, &["GET", "k"]),
            b"$19\r\n9223372036854775807\r\n"
        );

        run_on(&db, &["SET", "k", "-9223372036854775808"]);
        assert_eq!(run_on(&db, &["DECR", "k"]), overflow);
        assert_eq!(run_on(&db, &["INCR", "k"]), b":-9223372036854775807\r\n");

        run_on(&db, &["SET", "k", "1"]);
        assert_eq!(
            run_on(&db, &["INCRBY", "k", "9223372036854775807"]),
            overflow
        );
        assert_eq!(run_on(&db, &["GET", "k"]), b"$1\r\n1\r\n");

        // 없는 키는 0에서 시작하므로 i64 최소값도 받습니다
        assert_eq!(
            run_on(&db, &["INCRBY", "new", "-9223372036854775808"]),
            b":-9223372036854775808\r\n"
        );
        assert_eq!(run_on(&db, &["INCRBY", "new", "-1"]), overflow);
    }

    #[test]
    fn 인자_개수가_틀리면_이름이_각자_들어간다() {
        let wrong = |name: &str| {
            format!("-ERR wrong number of arguments for '{name}' command\r\n").into_bytes()
        };
        assert_eq!(run(&["INCR"]), wrong("incr"));
        assert_eq!(run(&["INCR", "a", "b"]), wrong("incr"));
        assert_eq!(run(&["DECR"]), wrong("decr"));
        assert_eq!(run(&["DECR", "a", "b"]), wrong("decr"));
        assert_eq!(run(&["INCRBY"]), wrong("incrby"));
        assert_eq!(run(&["INCRBY", "a"]), wrong("incrby"));
        // 숫자보다 개수를 먼저 봅니다
        assert_eq!(run(&["INCRBY", "a", "abc", "2"]), wrong("incrby"));
    }

    #[test]
    fn ttl은_그대로이고_만료된_키는_새로_시작한다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["SET", "k", "1", "EX", "100"]);
        assert_eq!(run_at(&db, start, &["INCR", "k"]), b":2\r\n");
        assert_eq!(run_at(&db, start, &["DECR", "k"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["INCRBY", "k", "5"]), b":6\r\n");
        assert_eq!(run_at(&db, start, &["TTL", "k"]), b":100\r\n");

        // 실패해도 TTL은 그대로입니다
        run_at(&db, start, &["SET", "s", "abc", "EX", "100"]);
        run_at(&db, start, &["INCR", "s"]);
        assert_eq!(run_at(&db, start, &["TTL", "s"]), b":100\r\n");

        // 만료된 뒤에는 0에서 시작하고 TTL도 없습니다
        let later = after(start, 200_000);
        assert_eq!(run_at(&db, later, &["INCR", "k"]), b":1\r\n");
        assert_eq!(run_at(&db, later, &["TTL", "k"]), b":-1\r\n");
    }

    #[test]
    fn 여러_스레드가_incr해도_빠지는_값이_없다() {
        // S05의 본론(원자성)은 difftest로 볼 수 없습니다. 커넥션이 한 번에 하나씩만 보내기 때문입니다.
        let db = Db::default();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..1000 {
                        run_on(&db, &["INCR", "n"]);
                    }
                });
            }
        });
        assert_eq!(run_on(&db, &["GET", "n"]), b"$4\r\n8000\r\n");
    }
}

//! 파싱된 명령(인자 목록)을 커맨드로 바꾸고, 실행해서 응답 바이트를 만듭니다.

use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::db::{Db, Delivery, End, Entries, Entry, Value};
use crate::reply::{array_header, bulk_string, error_reply, integer, null_array, null_bulk_string};

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
    /// `LPUSH`와 `RPUSH`입니다. 값이 하나 이상 들어 있습니다.
    Push {
        key: Vec<u8>,
        values: Vec<Vec<u8>>,
        end: End,
    },
    /// `LPOP`과 `RPOP`입니다.
    Pop {
        key: Vec<u8>,
        end: End,
    },
    /// 번호는 음수일 수 있습니다. 실제 범위는 리스트 길이를 알아야 정해져서 `execute`에서 계산합니다.
    LRange {
        key: Vec<u8>,
        start: i64,
        stop: i64,
    },
    LLen {
        key: Vec<u8>,
    },
    /// `BLPOP`과 `BRPOP`입니다. 키가 하나 이상 들어 있습니다.
    BlockingPop {
        keys: Vec<Vec<u8>>,
        end: End,
        /// 이 시각까지 기다립니다. None이면 timeout 0, 즉 값이 올 때까지 기다립니다.
        deadline: Option<Instant>,
    },
}

/// 커맨드를 실행한 결과입니다. 거의 전부 바로 보낼 응답이고, 빈 리스트를 만난 `BLPOP`/`BRPOP`만 `Block`입니다.
pub enum Outcome {
    Reply(Vec<u8>),
    Block(Blocked),
}

/// 값을 못 받고 대기자 명단에 올라간 `BLPOP`/`BRPOP`입니다.
/// 커넥션이 `wait`를 부르면 값이 오거나 시간이 다 될 때까지 그 커넥션 스레드만 잡니다.
pub struct Blocked {
    /// 명단에서 나를 찾는 번호표입니다.
    id: u64,
    /// 내 우편함의 받는 쪽입니다. 넣는 쪽 커넥션이 명단에 있는 `Sender`로 값을 넣어 줍니다.
    receiver: Receiver<Delivery>,
    deadline: Option<Instant>,
}

impl Blocked {
    /// 락을 잡지 않은 채로 잡니다. 락을 쥐고 자면 값을 넣으러 온 커넥션이 못 들어와서 서로 영원히 기다립니다.
    pub fn wait(self, db: &Db) -> Vec<u8> {
        let received = match self.deadline {
            // `recv`는 값이 올 때까지, `recv_timeout`은 길어야 그 시간만큼 이 스레드를 재웁니다.
            None => self.receiver.recv().ok(),
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                self.receiver.recv_timeout(left).ok()
            }
        };
        let delivery = match received {
            Some(delivery) => Some(delivery),
            None => {
                // 시간이 다 됐습니다. 락을 잡고 명단에서 내 이름을 지웁니다.
                // 이 줄 끝에서 guard가 사라져 락이 풀립니다.
                let still_waiting = db.lock().remove_waiter(self.id);
                if still_waiting {
                    None
                } else {
                    // 깨어나서 락을 잡기 직전에 넣는 쪽이 나를 명단에서 꺼내 값을 넘긴 경우입니다.
                    // 그 값은 이미 우편함에 있습니다. 여기서 안 꺼내면 아무도 못 받고 사라집니다.
                    self.receiver.try_recv().ok()
                }
            }
        };
        match delivery {
            Some((key, value)) => key_and_value(&key, &value),
            // 시간이 다 되면 nil 배열입니다. `LPOP`의 nil(`$-1`)과 바이트가 다릅니다.
            None => null_array(),
        }
    }

    /// 자러 가기 전에 연결이 끊긴 걸 알았을 때 부릅니다. 명단에서 이름만 지웁니다.
    pub fn cancel(self, db: &Db) {
        db.lock().remove_waiter(self.id);
    }
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
    /// 문자열 키에 리스트 커맨드를 썼거나 그 반대입니다. 이 에러만 `ERR`로 시작하지 않습니다.
    WrongType,
    /// `BLPOP`의 timeout이 숫자가 아닙니다.
    TimeoutNotFloat,
    TimeoutNegative,
    /// 숫자이긴 한데 밀리초로 바꾸면 i64에 안 들어갑니다. `inf`도 여기입니다.
    TimeoutOutOfRange,
}

/// `now`는 이 명령을 처리하는 시각입니다. 테스트는 여기에 "200초 뒤"를 넘겨서 sleep 없이 만료를 확인합니다.
pub fn build_reply(command: Vec<Vec<u8>>, db: &Db, now: Instant) -> Outcome {
    let mut parts = command.into_iter();
    // 빈 배열(`*0\r\n`)이나 빈 인라인 줄에는 진짜 Redis도 아무 응답을 하지 않습니다.
    let Some(name) = parts.next() else {
        return Outcome::Reply(Vec::new());
    };

    // `and_then`은 parse가 성공했을 때만 execute를 부릅니다. 어느 쪽에서 실패하든 에러는 여기 한 곳에서 응답으로 바뀝니다.
    match Command::parse(name, parts.collect(), now).and_then(|command| command.execute(db, now)) {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Reply(error_reply(&error.message())),
    }
}

impl Command {
    /// 인자 개수, 옵션 문법, 인자로 온 숫자는 여기서 전부 확인합니다.
    /// `execute`에서 나는 에러는 저장된 값을 봐야 알 수 있는 것(WRONGTYPE, `INCR` 계열의 숫자 에러)뿐입니다.
    /// 그래서 인자 에러가 언제나 타입 에러보다 먼저 나갑니다. (`LRANGE 문자열키 abc 1` → not an integer)
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
            b"LPUSH" => parse_push(args, End::Left, "lpush"),
            b"RPUSH" => parse_push(args, End::Right, "rpush"),
            // 진짜 Redis는 `LPOP key count`도 받지만 이번 범위가 아니라서 인자 개수 에러로 답합니다.
            b"LPOP" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::Pop {
                    key,
                    end: End::Left,
                }),
                Err(_) => Err(CommandError::WrongArity("lpop")),
            },
            b"RPOP" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::Pop {
                    key,
                    end: End::Right,
                }),
                Err(_) => Err(CommandError::WrongArity("rpop")),
            },
            b"LRANGE" => match <[Vec<u8>; 3]>::try_from(args) {
                Ok([key, start, stop]) => {
                    let start = parse_integer(&start).ok_or(CommandError::NotInteger)?;
                    let stop = parse_integer(&stop).ok_or(CommandError::NotInteger)?;
                    Ok(Command::LRange { key, start, stop })
                }
                Err(_) => Err(CommandError::WrongArity("lrange")),
            },
            b"LLEN" => match <[Vec<u8>; 1]>::try_from(args) {
                Ok([key]) => Ok(Command::LLen { key }),
                Err(_) => Err(CommandError::WrongArity("llen")),
            },
            b"BLPOP" => parse_blocking_pop(args, End::Left, "blpop", now),
            b"BRPOP" => parse_blocking_pop(args, End::Right, "brpop", now),
            _ => Err(CommandError::Unknown { name, args }),
        }
    }

    /// 저장소가 필요한 커맨드만 락을 잡습니다. 락은 각 갈래 안의 `entries`가 사라질 때 풀립니다.
    /// 키를 읽을 때는 항상 `live`를 거칩니다. 만료된 키는 거기서 지워지고 없는 키로 보입니다.
    fn execute(self, db: &Db, now: Instant) -> Result<Outcome, CommandError> {
        // 성공한 응답은 맨 아래에서 한 번에 `Ok`로 감쌉니다. 실패는 각 갈래에서 `?`로 바로 빠져나갑니다.
        let reply = match self {
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
                    return Ok(Outcome::Reply(null_bulk_string()));
                }
                // key와 value를 복사하지 않고 저장소로 옮깁니다. 있던 값과 TTL은 통째로 바뀝니다.
                // 있던 값이 리스트여도 따지지 않고 덮어씁니다. SET은 WRONGTYPE을 내지 않습니다.
                let value = Value::Str(value);
                entries.insert(key, Entry { value, expires_at });
                b"+OK\r\n".to_vec()
            }
            Command::Get { key } => {
                let mut entries = db.lock();
                // 값을 저장소 밖으로 복사해 꺼내지 않고, 락을 잡은 채 바로 응답 바이트로 씁니다.
                match entries.live(&key, now) {
                    Some(entry) => bulk_string(as_string(&mut entry.value)?),
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
                    let removed = entries.remove(&key, now);
                    return Ok(Outcome::Reply(integer(i64::from(removed))));
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
                integer(incr_by(&mut entries, key, delta, now)?)
            }
            Command::Push { key, values, end } => {
                let mut entries = db.lock();
                let length = match entries.live(&key, now) {
                    Some(entry) => {
                        // 넣고 빼도 `entry.expires_at`은 건드리지 않아서 걸려 있던 TTL이 그대로입니다.
                        let list = as_list(&mut entry.value)?;
                        push_all(list, values, end);
                        list.len()
                    }
                    None => {
                        let mut list = VecDeque::new();
                        push_all(&mut list, values, end);
                        // 응답할 길이는 넣기를 다 끝낸 이 시점의 길이입니다. 대기자가 가져가는 건 그다음입니다.
                        // 그래서 대기자가 있어도 `RPUSH q a b c`의 응답은 3입니다.
                        let length = list.len();
                        // 대기자는 리스트가 없을 때만 명단에 올라가므로, 넘겨줄 일은 새로 만드는 이 갈래에서만 생깁니다.
                        hand_off(&mut entries, &key, &mut list);
                        // 대기자들이 다 가져갔으면 빈 리스트를 저장하지 않습니다.
                        if !list.is_empty() {
                            let value = Value::List(list);
                            let expires_at = None;
                            entries.insert(key, Entry { value, expires_at });
                        }
                        length
                    }
                };
                integer(i64::try_from(length).unwrap_or(i64::MAX))
            }
            Command::Pop { key, end } => {
                let mut entries = db.lock();
                let Some(entry) = entries.live(&key, now) else {
                    return Ok(Outcome::Reply(null_bulk_string()));
                };
                let list = as_list(&mut entry.value)?;
                let popped = pop(list, end);
                // 빈 리스트는 남겨 두지 않습니다. 마지막 원소가 빠졌으면 키도 같이 지웁니다.
                // 그래서 다시 넣으면 TTL이 없는 새 리스트가 됩니다.
                if list.is_empty() {
                    entries.remove(&key, now);
                }
                match popped {
                    Some(value) => bulk_string(&value),
                    None => null_bulk_string(),
                }
            }
            Command::LRange { key, start, stop } => {
                let mut entries = db.lock();
                // 없는 키는 nil이 아니라 빈 배열입니다.
                let Some(entry) = entries.live(&key, now) else {
                    return Ok(Outcome::Reply(array_header(0)));
                };
                let list = as_list(&mut entry.value)?;
                let Some(range) = clamp_range(start, stop, list.len()) else {
                    return Ok(Outcome::Reply(array_header(0)));
                };
                // `range`는 그 구간을 복사하지 않고 차례로 빌려줍니다. 락을 잡은 채 바로 응답 바이트로 씁니다.
                let items = list.range(range);
                let mut reply = array_header(items.len());
                for item in items {
                    reply.extend_from_slice(&bulk_string(item));
                }
                reply
            }
            Command::LLen { key } => {
                let mut entries = db.lock();
                let length = match entries.live(&key, now) {
                    Some(entry) => as_list(&mut entry.value)?.len(),
                    None => 0,
                };
                integer(i64::try_from(length).unwrap_or(i64::MAX))
            }
            Command::BlockingPop {
                keys,
                end,
                deadline,
            } => {
                let mut entries = db.lock();
                // 앞 키부터 보고, 처음으로 차 있는 리스트에서 꺼냅니다. 여기까지는 `LPOP`과 같습니다.
                for key in &keys {
                    let Some(entry) = entries.live(key, now) else {
                        continue;
                    };
                    // 차 있는 리스트보다 앞에 문자열 키가 있으면 기다리지 않고 바로 WRONGTYPE입니다.
                    let list = as_list(&mut entry.value)?;
                    let popped = pop(list, end);
                    if list.is_empty() {
                        entries.remove(key, now);
                    }
                    if let Some(value) = popped {
                        return Ok(Outcome::Reply(key_and_value(key, &value)));
                    }
                }
                // 전부 비어 있습니다. 우편함을 하나 만들어 보내는 쪽은 명단에 걸고, 받는 쪽은 들고 나갑니다.
                // "비었는지 확인"과 "명단에 올리기"가 같은 락 안이라, 그 사이에 들어온 값을 놓칠 수 없습니다.
                let (sender, receiver) = mpsc::channel();
                let id = entries.add_waiter(keys, end, sender);
                // 여기서 자지 않습니다. `Block`을 들고 돌아가면 이 함수가 끝나면서 락이 풀리고, 자는 건 그다음입니다.
                return Ok(Outcome::Block(Blocked {
                    id,
                    receiver,
                    deadline,
                }));
            }
        };
        Ok(Outcome::Reply(reply))
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
        let value = Value::Str(delta.to_string().into_bytes());
        entries.insert(
            key,
            Entry {
                value,
                expires_at: None,
            },
        );
        return Ok(delta);
    };
    let bytes = as_string(&mut entry.value)?;
    let current = parse_integer(bytes).ok_or(CommandError::NotInteger)?;
    let next = current.checked_add(delta).ok_or(CommandError::Overflow)?;
    // `bytes`는 저장소 안의 값을 가리키는 빌림입니다. `*`로 그 자리에 새 값을 써넣습니다.
    *bytes = next.to_string().into_bytes();
    Ok(next)
}

/// 문자열 커맨드가 값을 꺼낼 때 거칩니다. 리스트가 들어 있으면 WRONGTYPE입니다.
/// 값 종류가 늘면 이 `match`가 컴파일 에러를 내서, 새 종류를 어떻게 할지 여기서 정하게 됩니다.
fn as_string(value: &mut Value) -> Result<&mut Vec<u8>, CommandError> {
    match value {
        Value::Str(bytes) => Ok(bytes),
        Value::List(_) => Err(CommandError::WrongType),
    }
}

/// 리스트 커맨드가 값을 꺼낼 때 거칩니다. 문자열이 들어 있으면 WRONGTYPE입니다.
fn as_list(value: &mut Value) -> Result<&mut VecDeque<Vec<u8>>, CommandError> {
    match value {
        Value::List(list) => Ok(list),
        Value::Str(_) => Err(CommandError::WrongType),
    }
}

/// `LPUSH key value [value ...]`, `RPUSH key value [value ...]`
fn parse_push(args: Vec<Vec<u8>>, end: End, name: &'static str) -> Result<Command, CommandError> {
    let mut args = args.into_iter();
    let Some(key) = args.next() else {
        return Err(CommandError::WrongArity(name));
    };
    // 키 뒤에 남은 인자가 전부 값입니다. 하나도 없으면 인자 개수 에러입니다.
    let values: Vec<Vec<u8>> = args.collect();
    if values.is_empty() {
        return Err(CommandError::WrongArity(name));
    }
    Ok(Command::Push { key, values, end })
}

/// `BLPOP key [key ...] timeout`, `BRPOP key [key ...] timeout`
fn parse_blocking_pop(
    mut args: Vec<Vec<u8>>,
    end: End,
    name: &'static str,
    now: Instant,
) -> Result<Command, CommandError> {
    // 마지막 인자가 timeout이고 그 앞은 전부 키입니다. 키가 하나는 있어야 합니다.
    if args.len() < 2 {
        return Err(CommandError::WrongArity(name));
    }
    let Some(timeout) = args.pop() else {
        return Err(CommandError::WrongArity(name));
    };
    // timeout은 키를 보기 전에 확인합니다. 그래서 문자열 키여도 timeout 에러가 먼저 나갑니다.
    let deadline = parse_timeout(&timeout, now)?;
    let keys = args;
    Ok(Command::BlockingPop {
        keys,
        end,
        deadline,
    })
}

/// timeout(초)을 "언제까지 기다릴지"로 바꿉니다. 0이면 무한정이라 None입니다.
/// `0.5` 같은 소수도 받습니다. 진짜 Redis처럼 밀리초로 올림해서 `0.0001`은 1ms를 기다립니다.
fn parse_timeout(bytes: &[u8], now: Instant) -> Result<Option<Instant>, CommandError> {
    let seconds: f64 = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.parse().ok())
        // Rust의 `parse`는 "nan"도 숫자로 받아 주지만 진짜 Redis는 거부합니다.
        .filter(|seconds: &f64| !seconds.is_nan())
        .ok_or(CommandError::TimeoutNotFloat)?;
    let millis = (seconds * 1000.0).ceil();
    if millis < 0.0 {
        return Err(CommandError::TimeoutNegative);
    }
    if millis == 0.0 {
        return Ok(None);
    }
    // `Duration::from_secs_f64`는 무한대나 너무 큰 값에서 패닉합니다. `try_`가 붙은 쪽은 에러를 돌려줍니다.
    let duration = Duration::try_from_secs_f64(millis / 1000.0)
        .map_err(|_| CommandError::TimeoutOutOfRange)?;
    // 진짜 Redis는 밀리초를 i64에 담습니다. 거기 안 들어가는 timeout은 거부합니다.
    i64::try_from(duration.as_millis()).map_err(|_| CommandError::TimeoutOutOfRange)?;
    let deadline = now
        .checked_add(duration)
        .ok_or(CommandError::TimeoutOutOfRange)?;
    Ok(Some(deadline))
}

fn pop(list: &mut VecDeque<Vec<u8>>, end: End) -> Option<Vec<u8>> {
    match end {
        End::Left => list.pop_front(),
        End::Right => list.pop_back(),
    }
}

/// 방금 넣은 값을 기다리던 대기자들에게 먼저 온 순서대로 하나씩 넘깁니다.
/// 넣는 쪽이 락을 쥔 채로 직접 넘기기 때문에, 대기자가 깨어나는 사이에 다른 커넥션의 `LPOP`이 가로챌 수 없습니다.
fn hand_off(entries: &mut Entries, key: &[u8], list: &mut VecDeque<Vec<u8>>) {
    while !list.is_empty() {
        let Some(waiter) = entries.take_waiter(key) else {
            return;
        };
        let end = waiter.end();
        let Some(value) = pop(list, end) else {
            return;
        };
        // 대기자의 응답에 키 이름이 들어가야 해서 복사해 보냅니다. 원본은 저장소의 키로 쓰여야 합니다.
        if let Err(value) = waiter.deliver(key.to_vec(), value) {
            // 받는 쪽이 사라진 대기자입니다. 값을 꺼낸 자리에 되돌려 놓고 다음 대기자를 봅니다.
            match end {
                End::Left => list.push_front(value),
                End::Right => list.push_back(value),
            }
        }
    }
}

/// `BLPOP`/`BRPOP`의 응답입니다. 어느 키에서 꺼냈는지와 값, 두 칸짜리 배열입니다.
fn key_and_value(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut reply = array_header(2);
    reply.extend_from_slice(&bulk_string(key));
    reply.extend_from_slice(&bulk_string(value));
    reply
}

/// 값을 인자 순서대로 하나씩 넣습니다. 그래서 `LPUSH q a b c`는 a, b, c 순으로 왼쪽에 들어가 c가 맨 앞이 됩니다.
fn push_all(list: &mut VecDeque<Vec<u8>>, values: Vec<Vec<u8>>, end: End) {
    for value in values {
        match end {
            End::Left => list.push_front(value),
            End::Right => list.push_back(value),
        }
    }
}

/// `LRANGE`의 시작 번호와 끝 번호를 실제로 꺼낼 구간으로 바꿉니다. 끝 번호도 포함입니다.
/// 음수는 뒤에서 센 번호라 길이를 더해 앞에서 센 번호로 바꾸고, 범위를 넘는 쪽은 잘라 냅니다.
/// 걸리는 원소가 없으면 None입니다.
fn clamp_range(start: i64, stop: i64, length: usize) -> Option<RangeInclusive<usize>> {
    let length = i64::try_from(length).unwrap_or(i64::MAX);
    // 길이는 0 이상이고 더하는 쪽은 음수라서 이 덧셈은 넘치지 않습니다.
    let start = if start < 0 { length + start } else { start };
    let stop = if stop < 0 { length + stop } else { stop };
    // 앞으로 넘친 시작 번호는 0으로 당깁니다. 끝 번호는 당기지 않아서, 음수로 남으면 아래에서 빈 구간이 됩니다.
    let start = start.max(0);
    if start > stop || start >= length {
        return None;
    }
    let stop = stop.min(length - 1);
    Some(usize::try_from(start).ok()?..=usize::try_from(stop).ok()?)
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
            CommandError::WrongType => {
                b"WRONGTYPE Operation against a key holding the wrong kind of value".to_vec()
            }
            CommandError::TimeoutNotFloat => b"ERR timeout is not a float or out of range".to_vec(),
            CommandError::TimeoutNegative => b"ERR timeout is negative".to_vec(),
            CommandError::TimeoutOutOfRange => b"ERR timeout is out of range".to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn words(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|part| part.as_bytes().to_vec()).collect()
    }

    /// 저장소를 쓰지 않는 커맨드용입니다. 매번 빈 저장소로 실행합니다.
    fn run(parts: &[&str]) -> Vec<u8> {
        run_on(&Db::default(), parts)
    }

    /// 같은 저장소에 커맨드를 이어서 보냅니다.
    fn run_on(db: &Db, parts: &[&str]) -> Vec<u8> {
        run_at(db, Instant::now(), parts)
    }

    /// 시각을 정해서 보냅니다. `start + 200초`를 넘기면 sleep 없이 "200초 뒤"가 됩니다.
    /// 커넥션이 하는 것처럼, `BLPOP`이 기다려야 하면 이 스레드가 응답이 나올 때까지 기다립니다.
    fn run_at(db: &Db, now: Instant, parts: &[&str]) -> Vec<u8> {
        match build_reply(words(parts), db, now) {
            Outcome::Reply(reply) => reply,
            Outcome::Block(blocked) => blocked.wait(db),
        }
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

    const WRONGTYPE: &[u8] =
        b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n";

    #[test]
    fn push는_길이를_정수로_답하고_lrange는_배열로_답한다() {
        let db = Db::default();
        // a, b, c를 하나씩 왼쪽에 넣어서 c가 맨 앞입니다
        assert_eq!(run_on(&db, &["LPUSH", "q", "a", "b", "c"]), b":3\r\n");
        assert_eq!(run_on(&db, &["RPUSH", "q", "x"]), b":4\r\n");
        assert_eq!(
            run_on(&db, &["LRANGE", "q", "0", "-1"]),
            b"*4\r\n$1\r\nc\r\n$1\r\nb\r\n$1\r\na\r\n$1\r\nx\r\n"
        );
        assert_eq!(run_on(&db, &["LLEN", "q"]), b":4\r\n");
        assert_eq!(run_on(&db, &["LPOP", "q"]), b"$1\r\nc\r\n");
        assert_eq!(run_on(&db, &["RPOP", "q"]), b"$1\r\nx\r\n");
        assert_eq!(
            run_on(&db, &["lrange", "q", "0", "-1"]),
            b"*2\r\n$1\r\nb\r\n$1\r\na\r\n"
        );
    }

    #[test]
    fn 없는_리스트의_nil_모양은_커맨드마다_다르다() {
        // difftest(redis-cli 출력 비교)에서는 `$-1`, `*0`이 둘 다 빈 줄이라 여기서 바이트로 확인합니다.
        let db = Db::default();
        assert_eq!(run_on(&db, &["LPOP", "nope"]), b"$-1\r\n");
        assert_eq!(run_on(&db, &["RPOP", "nope"]), b"$-1\r\n");
        assert_eq!(run_on(&db, &["LRANGE", "nope", "0", "-1"]), b"*0\r\n");
        assert_eq!(run_on(&db, &["LLEN", "nope"]), b":0\r\n");
        // 읽기만 했으니 키가 생기지 않습니다
        assert_eq!(run_on(&db, &["DBSIZE"]), b":0\r\n");
        // 걸리는 게 없는 범위도 빈 배열입니다
        run_on(&db, &["RPUSH", "q", "a"]);
        assert_eq!(run_on(&db, &["LRANGE", "q", "3", "1"]), b"*0\r\n");
    }

    #[test]
    fn 빈_값도_원소_하나다() {
        // 줄을 공백으로 쪼개 보내는 difftest 케이스에는 빈 값을 못 넣습니다.
        let db = Db::default();
        assert_eq!(run_on(&db, &["LPUSH", "q", ""]), b":1\r\n");
        assert_eq!(
            run_on(&db, &["LRANGE", "q", "0", "-1"]),
            b"*1\r\n$0\r\n\r\n"
        );
        assert_eq!(run_on(&db, &["LPOP", "q"]), b"$0\r\n\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "q"]), b":0\r\n");
    }

    #[test]
    fn lrange는_음수를_뒤에서_세고_범위_밖은_잘라_낸다() {
        let db = Db::default();
        run_on(&db, &["RPUSH", "q", "0", "1", "2", "3"]);
        // (시작, 끝, 나와야 하는 원소들)
        let min = "-9223372036854775808";
        let max = "9223372036854775807";
        let cases: [(&str, &str, &[&str]); 18] = [
            ("0", "-1", &["0", "1", "2", "3"]),
            ("0", "0", &["0"]),
            ("1", "2", &["1", "2"]),
            ("-2", "-1", &["2", "3"]),
            ("1", "-2", &["1", "2"]),
            ("0", "100", &["0", "1", "2", "3"]),
            ("-100", "100", &["0", "1", "2", "3"]),
            ("-5", "0", &["0"]),
            ("3", "3", &["3"]),
            ("3", "1", &[]),
            ("4", "10", &[]),
            ("-1", "-2", &[]),
            // 끝 번호가 맨 앞보다도 앞이면 0으로 당기지 않고 빈 배열입니다
            ("-10", "-5", &[]),
            ("0", "-5", &[]),
            (min, max, &["0", "1", "2", "3"]),
            (max, min, &[]),
            (min, min, &[]),
            (max, max, &[]),
        ];
        for (start, stop, expected) in cases {
            let mut reply = format!("*{}\r\n", expected.len());
            for item in expected {
                reply.push_str(&format!("${}\r\n{item}\r\n", item.len()));
            }
            assert_eq!(
                run_on(&db, &["LRANGE", "q", start, stop]),
                reply.into_bytes(),
                "LRANGE q {start} {stop}"
            );
        }
    }

    #[test]
    fn 마지막_원소를_빼면_키도_사라진다() {
        let db = Db::default();
        run_on(&db, &["RPUSH", "q", "a", "b"]);
        assert_eq!(run_on(&db, &["LPOP", "q"]), b"$1\r\na\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "q"]), b":1\r\n");
        assert_eq!(run_on(&db, &["RPOP", "q"]), b"$1\r\nb\r\n");
        assert_eq!(run_on(&db, &["EXISTS", "q"]), b":0\r\n");
        assert_eq!(run_on(&db, &["DBSIZE"]), b":0\r\n");
        assert_eq!(run_on(&db, &["LPOP", "q"]), b"$-1\r\n");
    }

    #[test]
    fn 타입이_다르면_wrongtype이고_값은_그대로다() {
        let db = Db::default();
        run_on(&db, &["SET", "s", "hello"]);
        assert_eq!(run_on(&db, &["LPUSH", "s", "a"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["RPUSH", "s", "a"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["LPOP", "s"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["RPOP", "s"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["LLEN", "s"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["LRANGE", "s", "0", "-1"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["GET", "s"]), b"$5\r\nhello\r\n");

        run_on(&db, &["RPUSH", "l", "a"]);
        assert_eq!(run_on(&db, &["GET", "l"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["INCR", "l"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["DECR", "l"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["INCRBY", "l", "1"]), WRONGTYPE);
        assert_eq!(
            run_on(&db, &["LRANGE", "l", "0", "-1"]),
            b"*1\r\n$1\r\na\r\n"
        );
    }

    #[test]
    fn 인자_에러가_타입_에러보다_먼저다() {
        let not_integer = b"-ERR value is not an integer or out of range\r\n".to_vec();
        let db = Db::default();
        run_on(&db, &["SET", "s", "hello"]);
        run_on(&db, &["RPUSH", "l", "a"]);
        assert_eq!(run_on(&db, &["LRANGE", "s", "abc", "1"]), not_integer);
        assert_eq!(run_on(&db, &["LRANGE", "s", "0", "1.5"]), not_integer);
        assert_eq!(run_on(&db, &["INCRBY", "l", "abc"]), not_integer);
        assert_eq!(
            run_on(&db, &["LRANGE", "s", "0"]),
            b"-ERR wrong number of arguments for 'lrange' command\r\n"
        );
        assert_eq!(
            run_on(&db, &["LPUSH", "s"]),
            b"-ERR wrong number of arguments for 'lpush' command\r\n"
        );
    }

    #[test]
    fn 타입을_안_따지는_커맨드는_리스트에도_그대로_된다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["RPUSH", "l", "a", "b"]);
        assert_eq!(run_at(&db, start, &["EXISTS", "l"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["TTL", "l"]), b":-1\r\n");
        assert_eq!(run_at(&db, start, &["EXPIRE", "l", "100"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["PERSIST", "l"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["DBSIZE"]), b":1\r\n");
        // NX는 리스트 키도 "있는 키"로 봅니다
        assert_eq!(run_at(&db, start, &["SET", "l", "v", "NX"]), b"$-1\r\n");
        assert_eq!(run_at(&db, start, &["LLEN", "l"]), b":2\r\n");
        // SET은 타입을 따지지 않고 덮어씁니다
        assert_eq!(run_at(&db, start, &["SET", "l", "v"]), b"+OK\r\n");
        assert_eq!(run_at(&db, start, &["GET", "l"]), b"$1\r\nv\r\n");
        assert_eq!(run_at(&db, start, &["LLEN", "l"]), WRONGTYPE);
        run_at(&db, start, &["RPUSH", "d", "a"]);
        assert_eq!(run_at(&db, start, &["DEL", "d"]), b":1\r\n");
        assert_eq!(run_at(&db, start, &["EXISTS", "d"]), b":0\r\n");
    }

    #[test]
    fn 리스트의_ttl은_넣고_빼도_그대로이고_비면_사라진다() {
        let db = Db::default();
        let start = Instant::now();
        run_at(&db, start, &["RPUSH", "q", "a"]);
        run_at(&db, start, &["EXPIRE", "q", "100"]);
        run_at(&db, start, &["RPUSH", "q", "b"]);
        run_at(&db, start, &["LPUSH", "q", "c"]);
        run_at(&db, start, &["LPOP", "q"]);
        assert_eq!(run_at(&db, start, &["TTL", "q"]), b":100\r\n");
        // 비어서 지워졌다가 다시 생긴 리스트는 새 키라 TTL이 없습니다
        run_at(&db, start, &["LPOP", "q"]);
        run_at(&db, start, &["LPOP", "q"]);
        assert_eq!(run_at(&db, start, &["TTL", "q"]), b":-2\r\n");
        run_at(&db, start, &["RPUSH", "q", "d"]);
        assert_eq!(run_at(&db, start, &["TTL", "q"]), b":-1\r\n");

        // 만료된 리스트는 없는 키입니다. 넣으면 옛 원소 없이 새로 시작합니다
        run_at(&db, start, &["RPUSH", "old", "x", "y"]);
        run_at(&db, start, &["EXPIRE", "old", "100"]);
        let later = after(start, 200_000);
        assert_eq!(run_at(&db, later, &["LLEN", "old"]), b":0\r\n");
        assert_eq!(run_at(&db, later, &["RPUSH", "old", "z"]), b":1\r\n");
        assert_eq!(
            run_at(&db, later, &["LRANGE", "old", "0", "-1"]),
            b"*1\r\n$1\r\nz\r\n"
        );
        assert_eq!(run_at(&db, later, &["TTL", "old"]), b":-1\r\n");
        // 만료된 문자열 키 자리에도 리스트를 만들 수 있습니다
        run_at(&db, start, &["SET", "s", "v", "PX", "100"]);
        assert_eq!(run_at(&db, later, &["LPUSH", "s", "a"]), b":1\r\n");
    }

    #[test]
    fn 리스트_커맨드의_인자_개수_에러() {
        let wrong = |name: &str| {
            format!("-ERR wrong number of arguments for '{name}' command\r\n").into_bytes()
        };
        assert_eq!(run(&["LPUSH"]), wrong("lpush"));
        assert_eq!(run(&["LPUSH", "q"]), wrong("lpush"));
        assert_eq!(run(&["RPUSH"]), wrong("rpush"));
        assert_eq!(run(&["RPUSH", "q"]), wrong("rpush"));
        assert_eq!(run(&["LPOP"]), wrong("lpop"));
        assert_eq!(run(&["RPOP"]), wrong("rpop"));
        assert_eq!(run(&["LLEN"]), wrong("llen"));
        assert_eq!(run(&["LLEN", "a", "b"]), wrong("llen"));
        assert_eq!(run(&["LRANGE"]), wrong("lrange"));
        assert_eq!(run(&["LRANGE", "q", "0"]), wrong("lrange"));
        assert_eq!(run(&["LRANGE", "q", "0", "1", "2"]), wrong("lrange"));
        // 숫자보다 개수를 먼저 봅니다
        assert_eq!(run(&["LRANGE", "q", "abc", "1", "2"]), wrong("lrange"));
        // 에러가 난 커맨드는 키를 만들지 않습니다
        let db = Db::default();
        run_on(&db, &["LPUSH", "q"]);
        assert_eq!(run_on(&db, &["EXISTS", "q"]), b":0\r\n");
    }

    #[test]
    fn 여러_스레드가_넣고_빼도_빠지는_원소가_없다() {
        // 넣는 스레드 4개가 500개씩 넣는 동안 빼는 스레드 4개가 같이 뺍니다.
        // 뺀 개수와 남은 개수를 더하면 넣은 개수와 같아야 합니다.
        let db = Db::default();
        let popped = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..500 {
                        run_on(&db, &["LPUSH", "q", "x"]);
                    }
                });
                scope.spawn(|| {
                    for _ in 0..500 {
                        if run_on(&db, &["RPOP", "q"]) != b"$-1\r\n" {
                            popped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                });
            }
        });
        let left = 2000 - popped.into_inner();
        assert_eq!(
            run_on(&db, &["LLEN", "q"]),
            format!(":{left}\r\n").into_bytes()
        );
    }

    /// 대기자가 `count`명이 될 때까지 기다립니다. sleep으로 어림잡지 않고 명단을 직접 봅니다.
    fn wait_for_waiters(db: &Db, count: usize) {
        let give_up = Instant::now() + Duration::from_secs(5);
        while db.lock().waiter_count() != count {
            assert!(
                Instant::now() < give_up,
                "대기자가 {count}명이 되지 않았습니다"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn blpop은_차_있으면_바로_키와_값을_답한다() {
        let db = Db::default();
        run_on(&db, &["RPUSH", "q", "a", "b", "c"]);
        assert_eq!(
            run_on(&db, &["BLPOP", "q", "0"]),
            b"*2\r\n$1\r\nq\r\n$1\r\na\r\n"
        );
        assert_eq!(
            run_on(&db, &["brpop", "q", "0"]),
            b"*2\r\n$1\r\nq\r\n$1\r\nc\r\n"
        );
        // 앞 키가 비어 있으면 다음 키에서 꺼내고, 응답에 그 키 이름이 들어갑니다
        assert_eq!(
            run_on(&db, &["BLPOP", "nope", "q", "5"]),
            b"*2\r\n$1\r\nq\r\n$1\r\nb\r\n"
        );
        // 마지막 원소를 꺼냈으니 키도 사라집니다
        assert_eq!(run_on(&db, &["EXISTS", "q"]), b":0\r\n");
        assert_eq!(db.lock().waiter_count(), 0);
    }

    #[test]
    fn 시간이_다_되면_nil_배열이고_명단에서_빠진다() {
        // `LPOP`의 nil(`$-1`), 빈 배열(`*0`)과 바이트가 다릅니다. difftest에서는 셋 다 빈 줄입니다.
        let db = Db::default();
        let start = Instant::now();
        assert_eq!(run_on(&db, &["BLPOP", "q", "0.05"]), b"*-1\r\n");
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert_eq!(run_on(&db, &["BRPOP", "a", "b", "0.01"]), b"*-1\r\n");
        assert_eq!(db.lock().waiter_count(), 0);
        // 떠난 대기자에게는 넘기지 않습니다. 값은 리스트에 남습니다
        assert_eq!(run_on(&db, &["RPUSH", "q", "late"]), b":1\r\n");
        assert_eq!(
            run_on(&db, &["LRANGE", "q", "0", "-1"]),
            b"*1\r\n$4\r\nlate\r\n"
        );
    }

    #[test]
    fn timeout_에러_문구가_진짜_redis와_같다() {
        let not_float = b"-ERR timeout is not a float or out of range\r\n".to_vec();
        let negative = b"-ERR timeout is negative\r\n".to_vec();
        let out_of_range = b"-ERR timeout is out of range\r\n".to_vec();
        let db = Db::default();
        // 문자열 키여도 timeout 에러가 WRONGTYPE보다 먼저입니다
        run_on(&db, &["SET", "s", "str"]);
        for key in ["q", "s"] {
            for bad in ["abc", "", "nan", "NaN", " 1", "1 ", "1_0", "1,5"] {
                assert_eq!(run_on(&db, &["BLPOP", key, bad]), not_float, "{bad:?}");
            }
            for bad in ["-1", "-0.5", "-inf"] {
                assert_eq!(run_on(&db, &["BLPOP", key, bad]), negative, "{bad:?}");
            }
            for bad in ["inf", "Infinity", "1e16", "9223372036854776"] {
                assert_eq!(run_on(&db, &["BRPOP", key, bad]), out_of_range, "{bad:?}");
            }
        }
        assert_eq!(db.lock().waiter_count(), 0);
    }

    #[test]
    fn timeout은_소수와_여러_표기를_받는다() {
        // 리스트가 차 있어서 기다리지 않고 바로 답합니다. 에러가 아니면 통과입니다.
        let db = Db::default();
        let accepted = [
            "0", "0.5", "+1", "1.", ".5", "1e0", "1E0", "-0", "-0.0", "00.5", "0.0001", "1e-9",
            "5e-324", "1e15",
            // 밀리초로 올림하면 0이라 음수로 보지 않습니다. 진짜 Redis도 받습니다
            "-1e-9",
        ];
        for timeout in accepted {
            run_on(&db, &["RPUSH", "q", "v"]);
            assert_eq!(
                run_on(&db, &["BLPOP", "q", timeout]),
                b"*2\r\n$1\r\nq\r\n$1\r\nv\r\n",
                "{timeout:?}"
            );
        }
    }

    #[test]
    fn 문자열_키를_만나면_기다리지_않고_wrongtype이다() {
        let db = Db::default();
        run_on(&db, &["SET", "s", "str"]);
        assert_eq!(run_on(&db, &["BLPOP", "s", "0"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["BRPOP", "s", "0"]), WRONGTYPE);
        // 앞 키가 비어 있어도 뒤의 문자열 키에서 걸립니다
        assert_eq!(run_on(&db, &["BLPOP", "empty", "s", "0"]), WRONGTYPE);
        run_on(&db, &["RPUSH", "l", "v1", "v2"]);
        // 차 있는 리스트가 문자열 키보다 앞이면 거기서 꺼내고 끝납니다
        assert_eq!(
            run_on(&db, &["BLPOP", "l", "s", "0"]),
            b"*2\r\n$1\r\nl\r\n$2\r\nv1\r\n"
        );
        assert_eq!(run_on(&db, &["BLPOP", "s", "l", "0"]), WRONGTYPE);
        assert_eq!(run_on(&db, &["LLEN", "l"]), b":1\r\n");
        assert_eq!(db.lock().waiter_count(), 0);
    }

    #[test]
    fn blpop_인자_개수_에러() {
        let wrong = |name: &str| {
            format!("-ERR wrong number of arguments for '{name}' command\r\n").into_bytes()
        };
        assert_eq!(run(&["BLPOP"]), wrong("blpop"));
        assert_eq!(run(&["BLPOP", "q"]), wrong("blpop"));
        assert_eq!(run(&["BRPOP"]), wrong("brpop"));
        assert_eq!(run(&["BRPOP", "q"]), wrong("brpop"));
    }

    #[test]
    fn 먼저_기다린_사람이_먼저_받는다() {
        // 로드맵의 S07 완료 기준입니다. 손님이 여럿이라 difftest로는 볼 수 없습니다.
        let db = Db::default();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| run_on(&db, &["BLPOP", "q", "0"]));
            wait_for_waiters(&db, 1);
            let second = scope.spawn(|| run_on(&db, &["BLPOP", "q", "0"]));
            wait_for_waiters(&db, 2);

            // 넣은 직후의 길이를 답하고, 그다음에 대기자가 가져갑니다
            assert_eq!(run_on(&db, &["RPUSH", "q", "one"]), b":1\r\n");
            assert_eq!(first.join().unwrap(), b"*2\r\n$1\r\nq\r\n$3\r\none\r\n");
            // 두 번째 대기자는 아직 자고 있습니다
            assert_eq!(db.lock().waiter_count(), 1);
            assert!(!second.is_finished());

            assert_eq!(run_on(&db, &["RPUSH", "q", "two"]), b":1\r\n");
            assert_eq!(second.join().unwrap(), b"*2\r\n$1\r\nq\r\n$3\r\ntwo\r\n");
        });
        assert_eq!(run_on(&db, &["EXISTS", "q"]), b":0\r\n");
    }

    #[test]
    fn 넣기를_다_끝낸_다음에_대기자에게_넘긴다() {
        let db = Db::default();
        std::thread::scope(|scope| {
            // RPUSH a b c: 응답은 3이고, 대기자는 맨 앞 a를 받고, b c가 남습니다
            let waiter = scope.spawn(|| run_on(&db, &["BLPOP", "r", "0"]));
            wait_for_waiters(&db, 1);
            assert_eq!(run_on(&db, &["RPUSH", "r", "a", "b", "c"]), b":3\r\n");
            assert_eq!(waiter.join().unwrap(), b"*2\r\n$1\r\nr\r\n$1\r\na\r\n");
            assert_eq!(
                run_on(&db, &["LRANGE", "r", "0", "-1"]),
                b"*2\r\n$1\r\nb\r\n$1\r\nc\r\n"
            );

            // LPUSH a b c: 다 넣으면 [c b a]라서 대기자는 a가 아니라 c를 받습니다
            let waiter = scope.spawn(|| run_on(&db, &["BLPOP", "l", "0"]));
            wait_for_waiters(&db, 1);
            assert_eq!(run_on(&db, &["LPUSH", "l", "a", "b", "c"]), b":3\r\n");
            assert_eq!(waiter.join().unwrap(), b"*2\r\n$1\r\nl\r\n$1\r\nc\r\n");
            assert_eq!(
                run_on(&db, &["LRANGE", "l", "0", "-1"]),
                b"*2\r\n$1\r\nb\r\n$1\r\na\r\n"
            );
        });
    }

    #[test]
    fn blpop과_brpop_대기자는_각자_자기_끝에서_받는다() {
        let db = Db::default();
        std::thread::scope(|scope| {
            let left = scope.spawn(|| run_on(&db, &["BLPOP", "q", "0"]));
            wait_for_waiters(&db, 1);
            let right = scope.spawn(|| run_on(&db, &["BRPOP", "q", "0"]));
            wait_for_waiters(&db, 2);
            assert_eq!(run_on(&db, &["RPUSH", "q", "1", "2", "3"]), b":3\r\n");
            assert_eq!(left.join().unwrap(), b"*2\r\n$1\r\nq\r\n$1\r\n1\r\n");
            assert_eq!(right.join().unwrap(), b"*2\r\n$1\r\nq\r\n$1\r\n3\r\n");
        });
        assert_eq!(
            run_on(&db, &["LRANGE", "q", "0", "-1"]),
            b"*1\r\n$1\r\n2\r\n"
        );
    }

    #[test]
    fn 대기자가_있으면_넣은_값을_다른_커맨드가_가로채지_못한다() {
        // 진짜 Redis에 `RPUSH q x`와 `LPOP q`를 붙여 보내면 LPOP은 nil이고 대기자가 x를 받습니다.
        let db = Db::default();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| run_on(&db, &["BLPOP", "q", "0"]));
            wait_for_waiters(&db, 1);
            assert_eq!(run_on(&db, &["RPUSH", "q", "x"]), b":1\r\n");
            // 대기자 스레드가 깨어났는지와 상관없이, RPUSH가 끝난 시점에 x는 이미 넘어가 있습니다
            assert_eq!(run_on(&db, &["LPOP", "q"]), b"$-1\r\n");
            assert_eq!(run_on(&db, &["EXISTS", "q"]), b":0\r\n");
            assert_eq!(waiter.join().unwrap(), b"*2\r\n$1\r\nq\r\n$1\r\nx\r\n");
        });
    }

    #[test]
    fn 여러_키를_기다리다_한_곳에서_받으면_다른_줄에서도_빠진다() {
        let db = Db::default();
        std::thread::scope(|scope| {
            // 첫 대기자는 q1과 q2 두 줄에 다 서고, 둘째 대기자는 q1 줄에서 그 뒤에 섭니다.
            let both = scope.spawn(|| run_on(&db, &["BLPOP", "q1", "q2", "0"]));
            wait_for_waiters(&db, 1);
            let behind = scope.spawn(|| run_on(&db, &["BLPOP", "q1", "2"]));
            wait_for_waiters(&db, 2);

            assert_eq!(run_on(&db, &["RPUSH", "q2", "x"]), b":1\r\n");
            assert_eq!(both.join().unwrap(), b"*2\r\n$2\r\nq2\r\n$1\r\nx\r\n");
            // 첫 대기자의 이름이 q1 줄 앞에 남아 있으면, 이 값이 둘째 대기자에게 가지 못합니다.
            assert_eq!(run_on(&db, &["RPUSH", "q1", "y"]), b":1\r\n");
            assert_eq!(behind.join().unwrap(), b"*2\r\n$2\r\nq1\r\n$1\r\ny\r\n");
        });
        assert_eq!(db.lock().waiter_count(), 0);
        // 둘 다 떠난 뒤에 넣은 값은 리스트에 남습니다
        assert_eq!(run_on(&db, &["RPUSH", "q1", "z"]), b":1\r\n");
        assert_eq!(
            run_on(&db, &["LRANGE", "q1", "0", "-1"]),
            b"*1\r\n$1\r\nz\r\n"
        );
    }

    #[test]
    fn 시간이_다_된_순간에_넘겨받은_값도_사라지지_않는다() {
        // 대기자의 시간이 다 된 뒤, 명단에서 이름을 지우기 전에 넣는 쪽이 값을 넘기는 장면을 만듭니다.
        // 테스트가 락을 쥐고 있어서, 깨어난 대기자는 이름을 지우러 락 앞에서 기다리게 됩니다.
        let db = Db::default();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| run_on(&db, &["BLPOP", "q", "0.05"]));
            wait_for_waiters(&db, 1);
            let mut entries = db.lock();
            std::thread::sleep(Duration::from_millis(150));
            // 넣는 쪽이 락 안에서 하는 일을 그대로 합니다
            let mut list = VecDeque::from([b"x".to_vec()]);
            hand_off(&mut entries, b"q", &mut list);
            assert!(list.is_empty());
            drop(entries);
            // 넣는 쪽은 넘겼다고 알고 있으니, 대기자가 nil을 답하면 x는 아무도 못 받고 사라집니다.
            assert_eq!(waiter.join().unwrap(), b"*2\r\n$1\r\nq\r\n$1\r\nx\r\n");
        });
        assert_eq!(db.lock().waiter_count(), 0);
    }

    #[test]
    fn 같은_키를_두_번_적어도_한_번만_받는다() {
        let db = Db::default();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| run_on(&db, &["BLPOP", "q", "q", "0"]));
            wait_for_waiters(&db, 1);
            assert_eq!(run_on(&db, &["RPUSH", "q", "a", "b"]), b":2\r\n");
            assert_eq!(waiter.join().unwrap(), b"*2\r\n$1\r\nq\r\n$1\r\na\r\n");
        });
        assert_eq!(
            run_on(&db, &["LRANGE", "q", "0", "-1"]),
            b"*1\r\n$1\r\nb\r\n"
        );
    }

    #[test]
    fn 기다리는_키에_문자열이_와도_계속_기다린다() {
        let db = Db::default();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| run_on(&db, &["BLPOP", "z", "0"]));
            wait_for_waiters(&db, 1);
            assert_eq!(run_on(&db, &["SET", "z", "str"]), b"+OK\r\n");
            // 에러를 받고 깨어나지 않습니다
            assert_eq!(db.lock().waiter_count(), 1);
            assert!(!waiter.is_finished());
            assert_eq!(run_on(&db, &["DEL", "z"]), b":1\r\n");
            assert_eq!(run_on(&db, &["RPUSH", "z", "v"]), b":1\r\n");
            assert_eq!(waiter.join().unwrap(), b"*2\r\n$1\r\nz\r\n$1\r\nv\r\n");
        });
    }

    #[test]
    fn 받는_쪽이_사라진_대기자는_건너뛰고_값을_남긴다() {
        let db = Db::default();
        // 명단에는 올라갔는데 `wait`도 `cancel`도 하지 않고 사라진 대기자를 만듭니다.
        let outcome = build_reply(words(&["BLPOP", "q", "0"]), &db, Instant::now());
        assert!(matches!(outcome, Outcome::Block(_)));
        drop(outcome);
        assert_eq!(run_on(&db, &["RPUSH", "q", "a", "b"]), b":2\r\n");
        assert_eq!(
            run_on(&db, &["LRANGE", "q", "0", "-1"]),
            b"*2\r\n$1\r\na\r\n$1\r\nb\r\n"
        );
        assert_eq!(db.lock().waiter_count(), 0);
    }

    #[test]
    fn 넣는_쪽과_기다리는_쪽이_섞여도_빠지거나_겹치는_값이_없다() {
        // 기다리는 쪽이 1ms마다 시간이 다 돼서, "시간이 다 된 순간에 값이 도착"하는 장면이 계속 생깁니다.
        // 그때 값이 사라지면 받은 개수가 모자라고, 두 번 주면 겹칩니다.
        const PRODUCERS: usize = 4;
        const PER_PRODUCER: usize = 250;
        let total = PRODUCERS * PER_PRODUCER;
        let db = Db::default();
        let received = Mutex::new(Vec::new());
        let give_up = Instant::now() + Duration::from_secs(10);
        std::thread::scope(|scope| {
            for producer in 0..PRODUCERS {
                let db = &db;
                scope.spawn(move || {
                    for number in 0..PER_PRODUCER {
                        let value = format!("{producer}-{number}");
                        run_on(db, &["RPUSH", "q", &value]);
                    }
                });
            }
            for consumer in 0..4 {
                let (db, received) = (&db, &received);
                // 절반은 앞에서, 절반은 뒤에서 꺼냅니다
                let name = if consumer % 2 == 0 { "BLPOP" } else { "BRPOP" };
                scope.spawn(move || {
                    while received.lock().unwrap().len() < total && Instant::now() < give_up {
                        let reply = run_on(db, &[name, "q", "0.001"]);
                        if reply != b"*-1\r\n" {
                            received.lock().unwrap().push(reply);
                        }
                    }
                });
            }
        });
        let mut received = received.into_inner().unwrap();
        assert_eq!(received.len(), total);
        received.sort();
        received.dedup();
        assert_eq!(received.len(), total, "같은 값을 두 번 받았습니다");
        assert_eq!(run_on(&db, &["LLEN", "q"]), b":0\r\n");
        assert_eq!(db.lock().waiter_count(), 0);
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

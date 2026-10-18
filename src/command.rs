//! 파싱된 명령(인자 목록)을 커맨드로 바꾸고, 실행해서 응답 바이트를 만듭니다.

use crate::db::Db;
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
    },
    Get {
        key: Vec<u8>,
    },
    /// 키가 하나 이상 들어 있습니다.
    Del(Vec<Vec<u8>>),
    /// 키가 하나 이상 들어 있습니다.
    Exists(Vec<Vec<u8>>),
}

/// 커맨드 단계의 에러입니다. 프로토콜 에러와 달리 응답만 보내고 연결은 유지합니다.
enum CommandError {
    Unknown {
        name: Vec<u8>,
        args: Vec<Vec<u8>>,
    },
    /// 문구에는 사용자가 친 대소문자와 상관없이 소문자 이름이 들어갑니다.
    WrongArity(&'static str),
    /// 모르는 옵션입니다. 지금은 `SET k v` 뒤에 뭐가 붙으면 전부 여기로 옵니다.
    Syntax,
}

pub fn build_reply(command: Vec<Vec<u8>>, db: &Db) -> Vec<u8> {
    let mut parts = command.into_iter();
    // 빈 배열(`*0\r\n`)이나 빈 인라인 줄에는 진짜 Redis도 아무 응답을 하지 않습니다.
    let Some(name) = parts.next() else {
        return Vec::new();
    };

    match Command::parse(name, parts.collect()) {
        Ok(command) => command.execute(db),
        Err(error) => error_reply(&error.message()),
    }
}

impl Command {
    /// 인자 개수는 여기서 전부 확인합니다. 그래서 `execute`는 실패하지 않습니다.
    fn parse(name: Vec<u8>, args: Vec<Vec<u8>>) -> Result<Command, CommandError> {
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
            b"SET" => match <[Vec<u8>; 2]>::try_from(args) {
                Ok([key, value]) => Ok(Command::Set { key, value }),
                // 키와 값 뒤에 남는 인자는 옵션 자리입니다. 옵션(EX, NX 등)은 S04라서 지금은 전부 모르는 옵션입니다.
                // `try_from`이 실패하면 원래 Vec을 그대로 돌려주므로 길이를 다시 볼 수 있습니다.
                Err(args) if args.len() > 2 => Err(CommandError::Syntax),
                Err(_) => Err(CommandError::WrongArity("set")),
            },
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
            _ => Err(CommandError::Unknown { name, args }),
        }
    }

    /// 저장소가 필요한 커맨드만 락을 잡습니다. 락은 각 갈래 안의 `entries`가 사라질 때 풀립니다.
    fn execute(self, db: &Db) -> Vec<u8> {
        match self {
            Command::Ping(None) => b"+PONG\r\n".to_vec(),
            Command::Ping(Some(message)) => bulk_string(&message),
            Command::Echo(message) => bulk_string(&message),
            Command::Set { key, value } => {
                // key와 value를 복사하지 않고 저장소로 옮깁니다. 있던 값은 덮어씁니다.
                db.lock().insert(key, value);
                b"+OK\r\n".to_vec()
            }
            Command::Get { key } => {
                let entries = db.lock();
                // 값을 저장소 밖으로 복사해 꺼내지 않고, 락을 잡은 채 바로 응답 바이트로 씁니다.
                match entries.get(&key) {
                    Some(value) => bulk_string(value),
                    None => null_bulk_string(),
                }
            }
            Command::Del(keys) => {
                let mut entries = db.lock();
                let mut deleted = 0;
                // 같은 키가 두 번 오면 두 번째에는 이미 지워져 있어서 세지 않습니다. (`DEL k k` → 1)
                for key in &keys {
                    if entries.remove(key).is_some() {
                        deleted += 1;
                    }
                }
                integer(deleted)
            }
            Command::Exists(keys) => {
                let entries = db.lock();
                let mut found = 0;
                // DEL과 달리 같은 키가 여러 번 오면 그만큼 셉니다. (`EXISTS k k` → 2)
                for key in &keys {
                    if entries.contains_key(key) {
                        found += 1;
                    }
                }
                integer(found)
            }
        }
    }
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
        build_reply(words(parts), &Db::default())
    }

    /// 같은 저장소에 커맨드를 이어서 보냅니다.
    fn run_on(db: &Db, parts: &[&str]) -> Vec<u8> {
        build_reply(words(parts), db)
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
        assert_eq!(run_on(&db, &["GET", "k"]), b"$-1\r\n".to_vec());
    }
}

//! 파싱된 명령(인자 목록)을 실행하고 응답 바이트를 만듭니다.

use crate::reply::{bulk_string, error_reply};

pub fn build_reply(command: &[Vec<u8>]) -> Vec<u8> {
    // 빈 배열(`*0\r\n`)이나 빈 인라인 줄에는 진짜 Redis도 아무 응답을 하지 않습니다.
    let Some(name) = command.first() else {
        return Vec::new();
    };

    if name.eq_ignore_ascii_case(b"PING") {
        return match command.len() {
            1 => b"+PONG\r\n".to_vec(),
            2 => bulk_string(&command[1]),
            _ => error_reply(b"ERR wrong number of arguments for 'ping' command"),
        };
    }

    // PING과 달리 ECHO는 인자가 정확히 하나여야 합니다.
    if name.eq_ignore_ascii_case(b"ECHO") {
        return match command.len() {
            2 => bulk_string(&command[1]),
            _ => error_reply(b"ERR wrong number of arguments for 'echo' command"),
        };
    }

    unknown_command_reply(command)
}

fn unknown_command_reply(command: &[Vec<u8>]) -> Vec<u8> {
    let Some(name) = command.first() else {
        return Vec::new();
    };

    let mut message = format!("ERR unknown command '{}'", String::from_utf8_lossy(name));
    if command.len() > 1 {
        message.push_str(", with args beginning with: ");
        for argument in &command[1..] {
            // 진짜 Redis는 인자마다 뒤에 공백을 하나씩 붙입니다.
            message.push_str(&format!("'{}' ", String::from_utf8_lossy(argument)));
        }
    }

    error_reply(message.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|part| part.as_bytes().to_vec()).collect()
    }

    #[test]
    fn ping_응답이_진짜_redis와_같다() {
        assert_eq!(build_reply(&words(&["PING"])), b"+PONG\r\n".to_vec());
        assert_eq!(build_reply(&words(&["ping"])), b"+PONG\r\n".to_vec());
        assert_eq!(
            build_reply(&words(&["PING", "hello"])),
            b"$5\r\nhello\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&words(&["PING", "a", "b"])),
            b"-ERR wrong number of arguments for 'ping' command\r\n".to_vec()
        );
    }

    #[test]
    fn echo_응답이_진짜_redis와_같다() {
        assert_eq!(
            build_reply(&words(&["ECHO", "hello"])),
            b"$5\r\nhello\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&words(&["echo", "hello"])),
            b"$5\r\nhello\r\n".to_vec()
        );
        // PING과 다릅니다. 인자가 없거나 둘 이상이면 에러입니다.
        let wrong = b"-ERR wrong number of arguments for 'echo' command\r\n".to_vec();
        assert_eq!(build_reply(&words(&["ECHO"])), wrong);
        assert_eq!(build_reply(&words(&["ECHO", "a", "b"])), wrong);
    }

    #[test]
    fn 한글은_바이트_길이로_응답한다() {
        assert_eq!(
            build_reply(&words(&["ECHO", "한글"])),
            "$6\r\n한글\r\n".as_bytes().to_vec()
        );
    }

    #[test]
    fn 모르는_커맨드_문구가_진짜_redis와_같다() {
        assert_eq!(
            build_reply(&words(&["garbage"])),
            b"-ERR unknown command 'garbage'\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&words(&["foo", "a", "b"])),
            b"-ERR unknown command 'foo', with args beginning with: 'a' 'b' \r\n".to_vec()
        );
    }
}

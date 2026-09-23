use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

/// 진짜 Redis가 6379를 쓰기 때문에 우리는 6380을 씁니다.
const DEFAULT_PORT: u16 = 6380;

fn main() {
    let port = match port_from_args() {
        Ok(port) => port,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("포트 {port} 을(를) 열지 못했습니다: {error}");
            std::process::exit(1);
        }
    };
    eprintln!("포트 {port} 에서 연결을 기다립니다");

    for incoming in listener.incoming() {
        match incoming {
            // 커넥션 하나당 스레드 하나. 한 커넥션이 막혀도 다른 커넥션은 계속 처리됩니다.
            Ok(stream) => {
                thread::spawn(move || handle_connection(stream));
            }
            // 연결 하나를 못 받았다고 서버를 끄지 않습니다.
            Err(error) => eprintln!("연결을 수락하지 못했습니다: {error}"),
        }
    }
}

fn port_from_args() -> Result<u16, String> {
    let mut args = std::env::args().skip(1);
    let mut port = DEFAULT_PORT;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--port 뒤에 포트 번호가 없습니다".to_string())?;
                port = value
                    .parse()
                    .map_err(|_| format!("포트 번호가 잘못되었습니다: {value}"))?;
            }
            other => return Err(format!("모르는 인자입니다: {other}")),
        }
    }

    Ok(port)
}

fn handle_connection(mut stream: TcpStream) {
    // 읽은 바이트를 모아두는 버퍼입니다.
    // TCP는 메시지 단위가 아니라 바이트 흐름이라, 명령 하나가 여러 번에 나눠 도착할 수 있습니다.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        let read = match stream.read(&mut chunk) {
            Ok(0) => return, // 상대가 연결을 닫았습니다
            Ok(count) => count,
            Err(error) => {
                eprintln!("읽지 못했습니다: {error}");
                return;
            }
        };
        buffer.extend_from_slice(&chunk[..read]);

        // 한 번 읽은 버퍼에 명령이 여러 개 들어 있을 수 있습니다 (파이프라이닝).
        loop {
            let (command, consumed) = match parse_command(&buffer) {
                Parsed::Complete(command, consumed) => (command, consumed),
                Parsed::Incomplete => break, // 뒷부분이 아직 안 왔습니다. 더 읽습니다
                Parsed::Broken => {
                    let _ = stream.write_all(&error_reply(b"ERR Protocol error"));
                    return; // 프로토콜이 깨지면 진짜 Redis도 연결을 끊습니다
                }
            };

            buffer.drain(..consumed);

            let reply = build_reply(&command);
            if reply.is_empty() {
                continue;
            }
            if let Err(error) = stream.write_all(&reply) {
                eprintln!("쓰지 못했습니다: {error}");
                return;
            }
        }
    }
}

/// 버퍼 앞쪽에서 명령 하나를 꺼낸 결과입니다.
enum Parsed {
    /// 명령 하나와, 그 명령이 버퍼에서 차지한 바이트 수
    Complete(Vec<Vec<u8>>, usize),
    /// 아직 덜 왔습니다
    Incomplete,
    /// 형식이 깨졌습니다
    Broken,
}

fn parse_command(buffer: &[u8]) -> Parsed {
    match buffer.first() {
        None => Parsed::Incomplete,
        // redis-cli 는 RESP 배열로 보냅니다: *2\r\n$4\r\nPING\r\n$5\r\nhello\r\n
        Some(b'*') => parse_array(buffer),
        // nc 나 telnet 으로 그냥 친 줄(인라인 명령)도 진짜 Redis는 받아줍니다.
        Some(_) => parse_inline(buffer),
    }
}

fn parse_inline(buffer: &[u8]) -> Parsed {
    let Some(end) = buffer.iter().position(|byte| *byte == b'\n') else {
        return Parsed::Incomplete;
    };

    let line = &buffer[..end];
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let parts = line
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_vec())
        .collect();

    Parsed::Complete(parts, end + 1)
}

fn parse_array(buffer: &[u8]) -> Parsed {
    let Some((header, mut offset)) = read_line(buffer, 0) else {
        return Parsed::Incomplete;
    };
    let Some(count) = parse_number(&header[1..]) else {
        return Parsed::Broken;
    };
    if count < 0 {
        return Parsed::Broken;
    }

    // count 를 그대로 믿고 with_capacity 를 쓰면, 큰 숫자 하나로 메모리를 왕창 잡을 수 있습니다.
    let mut parts = Vec::new();

    for _ in 0..count {
        let Some((header, after_header)) = read_line(buffer, offset) else {
            return Parsed::Incomplete;
        };
        if header.first() != Some(&b'$') {
            return Parsed::Broken;
        }
        let Some(length) = parse_number(&header[1..]) else {
            return Parsed::Broken;
        };
        let Ok(length) = usize::try_from(length) else {
            return Parsed::Broken;
        };

        let end = after_header + length;
        // 본문 뒤의 \r\n 까지 도착해야 이 조각이 완성된 것입니다.
        if buffer.len() < end + 2 {
            return Parsed::Incomplete;
        }
        parts.push(buffer[after_header..end].to_vec());
        offset = end + 2;
    }

    Parsed::Complete(parts, offset)
}

/// `from` 위치부터 `\r\n` 직전까지를 잘라내고, 그 다음 위치를 같이 돌려줍니다.
fn read_line(buffer: &[u8], from: usize) -> Option<(&[u8], usize)> {
    let rest = buffer.get(from..)?;
    let end = rest.windows(2).position(|pair| pair == b"\r\n")?;
    Some((&rest[..end], from + end + 2))
}

fn parse_number(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn build_reply(command: &[Vec<u8>]) -> Vec<u8> {
    // 빈 배열(`*0\r\n`)에는 진짜 Redis도 아무 응답을 하지 않습니다.
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

    unknown_command_reply(command)
}

fn bulk_string(value: &[u8]) -> Vec<u8> {
    let mut reply = format!("${}\r\n", value.len()).into_bytes();
    reply.extend_from_slice(value);
    reply.extend_from_slice(b"\r\n");
    reply
}

fn error_reply(message: &[u8]) -> Vec<u8> {
    let mut reply = vec![b'-'];
    reply.extend_from_slice(message);
    reply.extend_from_slice(b"\r\n");
    reply
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

    fn complete(buffer: &[u8]) -> (Vec<Vec<u8>>, usize) {
        match parse_command(buffer) {
            Parsed::Complete(command, consumed) => (command, consumed),
            _ => panic!("완성된 명령이어야 합니다"),
        }
    }

    #[test]
    fn resp_배열을_파싱한다() {
        let (command, consumed) = complete(b"*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n");
        assert_eq!(command, vec![b"PING".to_vec(), b"hello".to_vec()]);
        assert_eq!(consumed, 25);
    }

    #[test]
    fn 명령이_쪼개져_들어오면_기다린다() {
        assert!(matches!(
            parse_command(b"*2\r\n$4\r\nPING\r\n$5\r\nhel"),
            Parsed::Incomplete
        ));
    }

    #[test]
    fn 버퍼에_명령이_둘이면_앞의_것만_꺼낸다() {
        let buffer = b"*1\r\n$4\r\nPING\r\n*1\r\n$4\r\nPING\r\n";
        let (_, consumed) = complete(buffer);
        assert_eq!(consumed, 14);
        let (command, _) = complete(&buffer[consumed..]);
        assert_eq!(command, vec![b"PING".to_vec()]);
    }

    #[test]
    fn 인라인_명령도_받는다() {
        let (command, consumed) = complete(b"PING hello\r\n");
        assert_eq!(command, vec![b"PING".to_vec(), b"hello".to_vec()]);
        assert_eq!(consumed, 12);
    }

    #[test]
    fn 깨진_형식은_broken_이다() {
        assert!(matches!(parse_command(b"*1\r\n+PING\r\n"), Parsed::Broken));
    }

    #[test]
    fn ping_응답이_진짜_redis와_같다() {
        assert_eq!(build_reply(&[b"PING".to_vec()]), b"+PONG\r\n".to_vec());
        assert_eq!(build_reply(&[b"ping".to_vec()]), b"+PONG\r\n".to_vec());
        assert_eq!(
            build_reply(&[b"PING".to_vec(), b"hello".to_vec()]),
            b"$5\r\nhello\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&[b"PING".to_vec(), b"a".to_vec(), b"b".to_vec()]),
            b"-ERR wrong number of arguments for 'ping' command\r\n".to_vec()
        );
    }

    #[test]
    fn 모르는_커맨드_문구가_진짜_redis와_같다() {
        assert_eq!(
            build_reply(&[b"garbage".to_vec()]),
            b"-ERR unknown command 'garbage'\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&[b"foo".to_vec(), b"a".to_vec(), b"b".to_vec()]),
            b"-ERR unknown command 'foo', with args beginning with: 'a' 'b' \r\n".to_vec()
        );
    }
}

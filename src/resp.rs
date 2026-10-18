//! 클라이언트가 보낸 바이트를 명령(인자 목록)으로 바꿉니다. RESP 배열과 인라인 명령을 둘 다 받습니다.

/// 진짜 Redis의 proto-max-bulk-len 기본값(512MB)입니다.
/// 이 값을 넘는 길이를 선언하면 프로토콜 에러입니다.
const PROTO_MAX_BULK_LEN: i64 = 512 * 1024 * 1024;

/// 프로토콜 자체가 어긋난 경우입니다. 진짜 Redis는 문구를 보낸 뒤 연결을 끊습니다.
#[derive(Debug, PartialEq, Eq)]
pub enum ProtocolError {
    /// 배열 원소가 `$`로 시작하지 않습니다. 실제로 받은 바이트가 문구에 들어갑니다.
    ExpectedDollar(u8),
    InvalidMultibulkLength,
    InvalidBulkLength,
    UnbalancedQuotes,
}

impl ProtocolError {
    /// 에러 문구를 **바이트**로 돌려줍니다. `String`이 아닌 이유는 받은 바이트(`0xff` 등)를
    /// 그대로 넣어야 하기 때문입니다. `0xff as char`는 UTF-8 두 바이트가 되어 문구가 달라집니다.
    pub fn message(&self) -> Vec<u8> {
        match self {
            ProtocolError::ExpectedDollar(got) => {
                // 에러 줄 안에 \r \n 이 들어가면 줄이 깨지므로 공백으로 바꿉니다.
                // 진짜 Redis도 `*1\r\n\r\n`에 대해 `got ' '`로 응답합니다.
                let shown = if matches!(got, b'\r' | b'\n') {
                    b' '
                } else {
                    *got
                };
                let mut message = b"ERR Protocol error: expected '$', got '".to_vec();
                message.push(shown);
                message.push(b'\'');
                message
            }
            ProtocolError::InvalidMultibulkLength => {
                b"ERR Protocol error: invalid multibulk length".to_vec()
            }
            ProtocolError::InvalidBulkLength => b"ERR Protocol error: invalid bulk length".to_vec(),
            ProtocolError::UnbalancedQuotes => {
                b"ERR Protocol error: unbalanced quotes in request".to_vec()
            }
        }
    }
}

/// 버퍼 앞쪽에서 명령 하나를 꺼낸 결과입니다.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// 명령 하나와, 그 명령이 버퍼에서 차지한 바이트 수
    Complete(Vec<Vec<u8>>, usize),
    /// 아직 덜 왔습니다
    Incomplete,
    /// 프로토콜이 어긋났습니다
    Error(ProtocolError),
}

/// `*`로 시작하면 RESP 배열, **그 밖의 모든 것**은 인라인 명령입니다.
/// 진짜 Redis도 `*` 하나만 보고 가릅니다. 그래서 `$3\r\n`도 프로토콜 에러가 아니라
/// `unknown command '$3'`이 됩니다.
pub fn parse_command(buffer: &[u8]) -> Parsed {
    match buffer.first() {
        None => Parsed::Incomplete,
        Some(b'*') => parse_array(buffer),
        Some(_) => parse_inline(buffer),
    }
}

fn parse_array(buffer: &[u8]) -> Parsed {
    let Some((header, mut offset)) = read_line(buffer, 0) else {
        return Parsed::Incomplete;
    };
    let Some(count) = parse_number(&header[1..]) else {
        return Parsed::Error(ProtocolError::InvalidMultibulkLength);
    };
    if count > i32::MAX as i64 {
        return Parsed::Error(ProtocolError::InvalidMultibulkLength);
    }
    // `*0`과 `*-1`은 에러가 아닙니다. 응답 없이 그냥 넘어갑니다.
    if count <= 0 {
        return Parsed::Complete(Vec::new(), offset);
    }

    // count 를 그대로 믿고 with_capacity 를 쓰면, 큰 숫자 하나로 메모리를 왕창 잡을 수 있습니다.
    let mut parts = Vec::new();

    for _ in 0..count {
        // 줄이 다 올 때까지 기다린 **뒤에** 첫 바이트를 검사합니다. 진짜 Redis가 이 순서입니다.
        // `*1\r\n+PIN`처럼 줄이 덜 왔으면 틀린 글자가 보여도 에러를 내지 않고 기다립니다.
        let Some((header, after_header)) = read_line(buffer, offset) else {
            return Parsed::Incomplete;
        };
        match buffer.get(offset) {
            Some(b'$') => {}
            Some(other) => return Parsed::Error(ProtocolError::ExpectedDollar(*other)),
            // read_line 이 성공했으니 이 자리에는 항상 바이트가 있습니다.
            None => return Parsed::Incomplete,
        }
        let Some(length) = parse_number(&header[1..]) else {
            return Parsed::Error(ProtocolError::InvalidBulkLength);
        };
        if !(0..=PROTO_MAX_BULK_LEN).contains(&length) {
            return Parsed::Error(ProtocolError::InvalidBulkLength);
        }
        let length = length as usize;

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

/// `nc`나 `telnet`으로 그냥 친 줄입니다. 공백으로 쪼개고 따옴표를 풀어줍니다.
fn parse_inline(buffer: &[u8]) -> Parsed {
    // `\r` 없이 `\n`만 와도 한 줄로 인정합니다.
    let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') else {
        return Parsed::Incomplete;
    };
    let line = &buffer[..newline];
    let line = line.strip_suffix(b"\r").unwrap_or(line);

    match split_inline_args(line) {
        // 빈 줄이면 인자가 0개입니다. 응답하지 않고 넘어갑니다.
        Ok(args) => Parsed::Complete(args, newline + 1),
        Err(error) => Parsed::Error(error),
    }
}

/// 인라인 한 줄을 인자 목록으로 쪼갭니다.
///
/// - 공백이 여러 개여도 구분자 하나로 봅니다
/// - `"..."`와 `'...'` 안에서는 공백을 글자로 취급합니다
/// - 따옴표를 닫은 바로 뒤는 공백이거나 줄 끝이어야 합니다
///
/// 이스케이프(`\x41`, `\n` 등)는 S02 범위에서 뺐습니다. 설계 문서의 '알려진 차이' 참고.
fn split_inline_args(line: &[u8]) -> Result<Vec<Vec<u8>>, ProtocolError> {
    let mut args = Vec::new();
    let mut index = 0;

    while index < line.len() {
        while index < line.len() && line[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= line.len() {
            break;
        }

        let mut current = Vec::new();
        let mut quote = None;

        loop {
            let Some(&byte) = line.get(index) else {
                // 줄이 끝났는데 따옴표가 열려 있습니다.
                if quote.is_some() {
                    return Err(ProtocolError::UnbalancedQuotes);
                }
                break;
            };

            match quote {
                Some(open) if byte == open => {
                    // 닫는 따옴표 다음은 공백이거나 줄 끝이어야 합니다.
                    if line
                        .get(index + 1)
                        .is_some_and(|next| !next.is_ascii_whitespace())
                    {
                        return Err(ProtocolError::UnbalancedQuotes);
                    }
                    index += 1;
                    break;
                }
                Some(_) => {
                    current.push(byte);
                    index += 1;
                }
                None => {
                    if byte.is_ascii_whitespace() {
                        break;
                    }
                    if byte == b'"' || byte == b'\'' {
                        quote = Some(byte);
                        index += 1;
                        continue;
                    }
                    current.push(byte);
                    index += 1;
                }
            }
        }

        args.push(current);
    }

    Ok(args)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::build_reply;

    fn complete(buffer: &[u8]) -> (Vec<Vec<u8>>, usize) {
        match parse_command(buffer) {
            Parsed::Complete(command, consumed) => (command, consumed),
            other => panic!("완성된 명령이어야 합니다: {other:?}"),
        }
    }

    fn words(parts: &[&str]) -> Vec<Vec<u8>> {
        parts.iter().map(|part| part.as_bytes().to_vec()).collect()
    }

    #[test]
    fn resp_배열을_파싱한다() {
        let (command, consumed) = complete(b"*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n");
        assert_eq!(command, words(&["PING", "hello"]));
        assert_eq!(consumed, 25);
    }

    #[test]
    fn 명령이_쪼개져_들어오면_기다린다() {
        assert_eq!(
            parse_command(b"*2\r\n$4\r\nPING\r\n$5\r\nhel"),
            Parsed::Incomplete
        );
    }

    #[test]
    fn 버퍼에_명령이_둘이면_앞의_것만_꺼낸다() {
        let buffer = b"*1\r\n$4\r\nPING\r\n*2\r\n$4\r\nECHO\r\n$2\r\nhi\r\n";
        let (first, consumed) = complete(buffer);
        assert_eq!(first, words(&["PING"]));
        assert_eq!(consumed, 14);

        let (second, _) = complete(&buffer[consumed..]);
        assert_eq!(second, words(&["ECHO", "hi"]));
    }

    #[test]
    fn 빈_배열은_에러가_아니라_빈_명령이다() {
        assert_eq!(complete(b"*0\r\n"), (Vec::new(), 4));
        assert_eq!(complete(b"*-1\r\n"), (Vec::new(), 5));
        assert!(build_reply(&[]).is_empty());
    }

    #[test]
    fn 인라인_명령을_공백으로_쪼갠다() {
        assert_eq!(complete(b"PING\r\n"), (words(&["PING"]), 6));
        // \r 없이 \n 만 와도 됩니다
        assert_eq!(complete(b"PING\n"), (words(&["PING"]), 5));
        // 앞뒤 공백과 연속 공백은 구분자 하나로 봅니다
        assert_eq!(
            complete(b"   ECHO   hi   \r\n"),
            (words(&["ECHO", "hi"]), 17)
        );
        // 빈 줄은 인자 0개입니다
        assert_eq!(complete(b"\r\n"), (Vec::new(), 2));
    }

    #[test]
    fn 인라인_따옴표_안에서는_공백이_글자다() {
        assert_eq!(complete(b"ECHO \"a b\"\r\n").0, words(&["ECHO", "a b"]));
        assert_eq!(complete(b"ECHO 'a b'\r\n").0, words(&["ECHO", "a b"]));
        // 빈 따옴표는 빈 문자열 인자입니다
        assert_eq!(complete(b"ECHO \"\"\r\n").0, words(&["ECHO", ""]));
        // 따옴표가 두 쌍이면 인자가 둘입니다
        assert_eq!(
            complete(b"ECHO \"a\" \"b\"\r\n").0,
            words(&["ECHO", "a", "b"])
        );
    }

    #[test]
    fn 따옴표가_안_맞으면_프로토콜_에러다() {
        let unbalanced = Parsed::Error(ProtocolError::UnbalancedQuotes);
        // 닫히지 않음
        assert_eq!(parse_command(b"ECHO \"a\r\n"), unbalanced);
        // 닫은 뒤에 공백이 아닌 글자가 붙음
        assert_eq!(parse_command(b"ECHO \"a\"b\"\r\n"), unbalanced);
        // 토큰 중간에서 따옴표가 열림
        assert_eq!(parse_command(b"ECHO a\"b\r\n"), unbalanced);
    }

    #[test]
    fn 달러로_시작해도_인라인으로_읽는다() {
        // 프로토콜 에러가 아닙니다. RESP 배열은 `*` 로만 시작합니다.
        assert_eq!(complete(b"$3\r\n").0, words(&["$3"]));
    }

    #[test]
    fn 프로토콜_에러_문구가_진짜_redis와_같다() {
        assert_eq!(
            parse_command(b"*1\r\n+PING\r\n"),
            Parsed::Error(ProtocolError::ExpectedDollar(b'+'))
        );
        assert_eq!(
            ProtocolError::ExpectedDollar(b'+').message(),
            b"ERR Protocol error: expected '$', got '+'".to_vec()
        );
        assert_eq!(
            parse_command(b"*abc\r\n"),
            Parsed::Error(ProtocolError::InvalidMultibulkLength)
        );
        assert_eq!(
            parse_command(b"*2147483648\r\n"),
            Parsed::Error(ProtocolError::InvalidMultibulkLength)
        );
        assert_eq!(
            parse_command(b"*1\r\n$abc\r\n"),
            Parsed::Error(ProtocolError::InvalidBulkLength)
        );
        assert_eq!(
            parse_command(b"*1\r\n$-1\r\n"),
            Parsed::Error(ProtocolError::InvalidBulkLength)
        );
        assert_eq!(
            parse_command(b"*1\r\n$536870913\r\n"),
            Parsed::Error(ProtocolError::InvalidBulkLength)
        );
    }

    #[test]
    fn 줄이_덜_왔으면_첫_바이트가_틀려도_기다린다() {
        // 진짜 Redis는 줄이 완성된 뒤에 `$` 인지 검사합니다.
        assert_eq!(parse_command(b"*1\r\n+PIN"), Parsed::Incomplete);
        assert_eq!(parse_command(b"*1\r\nX"), Parsed::Incomplete);
        // 줄이 완성되면 그때 에러입니다.
        assert_eq!(
            parse_command(b"*1\r\n+PING\r\n"),
            Parsed::Error(ProtocolError::ExpectedDollar(b'+'))
        );
    }

    #[test]
    fn expected_dollar_문구는_받은_바이트를_그대로_담는다() {
        // 비ASCII 바이트는 UTF-8 두 바이트가 아니라 생 바이트 하나여야 합니다.
        assert_eq!(
            ProtocolError::ExpectedDollar(0xff).message(),
            b"ERR Protocol error: expected '$', got '\xff'".to_vec()
        );
        // \r \n 은 에러 줄을 깨뜨리므로 공백으로 바뀝니다.
        let space = b"ERR Protocol error: expected '$', got ' '".to_vec();
        assert_eq!(ProtocolError::ExpectedDollar(b'\r').message(), space);
        assert_eq!(ProtocolError::ExpectedDollar(b'\n').message(), space);
        // 빈 줄(`*1\r\n\r\n`)도 첫 바이트가 \r 인 expected '$' 에러입니다.
        assert_eq!(
            parse_command(b"*1\r\n\r\n"),
            Parsed::Error(ProtocolError::ExpectedDollar(b'\r'))
        );
    }

    #[test]
    fn 거대한_배열_개수는_에러가_아니라_대기다() {
        // 진짜 Redis도 INT_MAX 이하면 그만큼 기다립니다. 미리 할당하지 않는 이유입니다.
        assert_eq!(parse_command(b"*1048576\r\n"), Parsed::Incomplete);
    }
}

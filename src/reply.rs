//! 응답을 RESP 바이트로 만듭니다. 커맨드 처리와 커넥션 처리 양쪽에서 같이 씁니다.

pub fn bulk_string(value: &[u8]) -> Vec<u8> {
    let mut reply = format!("${}\r\n", value.len()).into_bytes();
    reply.extend_from_slice(value);
    reply.extend_from_slice(b"\r\n");
    reply
}

/// 값이 없다는 뜻의 Null Bulk String입니다. 빈 문자열(`$0\r\n\r\n`)과 다릅니다.
pub fn null_bulk_string() -> Vec<u8> {
    b"$-1\r\n".to_vec()
}

pub fn integer(value: i64) -> Vec<u8> {
    format!(":{value}\r\n").into_bytes()
}

pub fn error_reply(message: &[u8]) -> Vec<u8> {
    let mut reply = vec![b'-'];
    reply.extend_from_slice(message);
    reply.extend_from_slice(b"\r\n");
    reply
}

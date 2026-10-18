//! 응답을 RESP 바이트로 만듭니다. 커맨드 처리와 커넥션 처리 양쪽에서 같이 씁니다.

pub fn bulk_string(value: &[u8]) -> Vec<u8> {
    let mut reply = format!("${}\r\n", value.len()).into_bytes();
    reply.extend_from_slice(value);
    reply.extend_from_slice(b"\r\n");
    reply
}

pub fn error_reply(message: &[u8]) -> Vec<u8> {
    let mut reply = vec![b'-'];
    reply.extend_from_slice(message);
    reply.extend_from_slice(b"\r\n");
    reply
}

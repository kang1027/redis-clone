//! 커넥션 하나를 맡아 읽고, 명령을 처리하고, 응답을 씁니다.

use std::io::{Read, Write};
use std::net::TcpStream;

use crate::command::build_reply;
use crate::reply::error_reply;
use crate::resp::{Parsed, parse_command};

/// 커넥션 하나가 들고 다니는 상태입니다.
pub struct Connection {
    stream: TcpStream,
    // 읽은 바이트를 모아두는 버퍼입니다.
    // TCP는 메시지 단위가 아니라 바이트 흐름이라, 명령 하나가 여러 번에 나눠 도착할 수 있습니다.
    buffer: Vec<u8>,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Self {
        Connection {
            stream,
            buffer: Vec::new(),
        }
    }

    /// 상대가 연결을 닫거나 프로토콜 에러가 날 때까지 읽고, 처리하고, 응답합니다.
    pub fn run(&mut self) {
        loop {
            match self.read_more() {
                Ok(0) => return, // 상대가 연결을 닫았습니다
                Ok(_) => {}
                Err(error) => {
                    eprintln!("읽지 못했습니다: {error}");
                    return;
                }
            }

            let (replies, fatal) = self.process_buffer();

            // 응답이 비어 있으면(빈 배열, 빈 인라인 줄) write_all 은 아무것도 하지 않습니다.
            if let Err(error) = self.stream.write_all(&replies) {
                eprintln!("쓰지 못했습니다: {error}");
                return;
            }

            if fatal {
                return;
            }
        }
    }

    /// 소켓에서 한 번 읽어 버퍼 뒤에 붙입니다. 읽은 바이트 수가 0이면 상대가 연결을 닫은 것입니다.
    fn read_more(&mut self) -> std::io::Result<usize> {
        let mut chunk = [0u8; 1024];
        let read = self.stream.read(&mut chunk)?;
        self.buffer.extend_from_slice(&chunk[..read]);
        Ok(read)
    }

    /// 버퍼에 명령이 여러 개 쌓여 있을 수 있습니다(파이프라이닝).
    /// 꺼낼 수 있는 만큼 모두 처리하고, 모은 응답과 "연결을 끊어야 하는가"를 돌려줍니다.
    fn process_buffer(&mut self) -> (Vec<u8>, bool) {
        let mut replies = Vec::new();
        let mut cursor = 0;
        let mut fatal = false;

        loop {
            match parse_command(&self.buffer[cursor..]) {
                Parsed::Complete(command, consumed) => {
                    cursor += consumed;
                    replies.extend_from_slice(&build_reply(&command));
                }
                // 뒷부분이 아직 안 왔습니다. 더 읽습니다.
                Parsed::Incomplete => break,
                // 프로토콜 에러는 문구를 보낸 뒤 연결을 끊습니다.
                // 커맨드 에러(모르는 커맨드, 인자 개수)와 달리 복구할 수 없기 때문입니다.
                Parsed::Error(error) => {
                    replies.extend_from_slice(&error_reply(&error.message()));
                    fatal = true;
                    break;
                }
            }
        }

        // 명령마다 당기지 않고, 이번에 처리한 만큼 한 번만 당깁니다.
        self.buffer.drain(..cursor);

        (replies, fatal)
    }
}

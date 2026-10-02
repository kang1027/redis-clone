use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream}; // TcpListner: 서버 입구 관련, TcpStream: 통로 관련
use std::thread;

fn main() -> std::io::Result<()> {
    // Success: () / Fail: std::io::Error
    let port = std::env::args() // [0] main.rs [1] port [2] "6380"
        .nth(2)
        .unwrap_or_else(|| "6380".to_string());

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))?; // '?'의 의미 = 에러나면 현재 함수 바깥으로 전달

    println!("서버 시작: {port}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(e) = handle_client(stream) {
                        eprintln!("클라이언트 처리 오류: {e}");
                    }
                });
            }
            Err(e) => eprintln!("연결 오류: {e}"),
        }
    }

    Ok(())
}

fn handle_client(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream); // TcpStream을 편하게 읽기 위해 BufReader로 감쌈

    // >> PING hello
    // *2\r\n
    // $4\r\n
    // PING\r\n
    // $5\r\n
    // hello\r\n

    loop {
        // 1. 몇 개 읽기
        let mut line = String::new();

        if reader.read_line(&mut line)? == 0 {
            return Ok(()); // 성공 시 반환 값 없음
        }

        let count: usize = match line[1..].trim().parse() {
            // 1번째 바이트부터 슬라이싱 => \r\n 제거 => 파싱 => 숫자만 남음
            Ok(n) => n,
            Err(_) => return Ok(()), // Err 떴는데 왜 성공 상태를 반환??
        };

        let mut args = Vec::new(); // java의 ArrayList랑 유사, 명령 저장할 배열 (args = ["PING", "hello"])

        // 2. 각 인자 읽기
        for _ in 0..count {
            line.clear();
            reader.read_line(&mut line)?;

            let length: usize = match line[1..].trim().parse() {
                Ok(n) => n,
                Err(_) => return Ok(()),
            };

            let mut data = vec![0u8; length];
            reader.read_exact(&mut data)?; // 전달된 배열에 가득 채우기

            // 뒤의 \r\n 버리기
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf)?;

            args.push(data);
        }

        // 3. Ping 처리
        if !args[0].eq_ignore_ascii_case(b"PING") {
            // 대소문자 무시하고 PING 명령인지 확인
            return Ok(());
        }

        match args.len() {
            1 => {
                reader.get_mut().write_all(b"+PONG\r\n")?;
            }

            2 => {
                let message = &args[1];

                write!(reader.get_mut(), "${}\r\n", message.len())?;
                reader.get_mut().write_all(message)?;
                reader.get_mut().write_all(b"\r\n")?;
            }

            _ => {
                reader
                    .get_mut()
                    .write_all(b"-ERR wrong number of arguments for 'ping' command\r\n")?;
            }
        }
    }
}

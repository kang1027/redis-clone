//! 진짜 소켓으로 서버에 붙어서 보는 S07 테스트입니다.
//!
//! `scripts/difftest.sh`는 한 줄마다 redis-cli를 새로 띄워서 손님이 늘 하나입니다.
//! 기다리는 손님과 넣는 손님이 따로 있어야 하는 장면은 여기서 봅니다.
//! 기대하는 바이트는 전부 진짜 Redis 8.10.2에 같은 순서로 보내서 받은 값입니다.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// 테스트들은 스레드로 동시에 돕니다. 포트 번호를 하나씩 올려 가며 나눠 줘서, 두 테스트가 같은 포트를 집지 않게 합니다.
static NEXT_PORT: AtomicU16 = AtomicU16::new(26380);

/// 테스트마다 서버를 하나씩 따로 띄웁니다. 테스트가 끝나면(실패해도) `drop`에서 끕니다.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start() -> Server {
        let give_up = Instant::now() + Duration::from_secs(10);
        loop {
            let port = NEXT_PORT.fetch_add(1, Ordering::Relaxed);
            let child = Command::new(env!("CARGO_BIN_EXE_redis-clone"))
                .args(["--port", &port.to_string()])
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut server = Server { child, port };
            // 붙을 수 있을 때까지 기다립니다. 그 포트를 다른 프로그램이 쓰고 있으면 서버가 바로 꺼지므로,
            // 그때는 다음 번호로 다시 띄웁니다. (꺼진 서버는 `server`가 사라질 때 `drop`에서 정리됩니다)
            loop {
                assert!(Instant::now() < give_up, "서버가 뜨지 않았습니다");
                if server.child.try_wait().unwrap().is_some() {
                    break;
                }
                if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return server;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn connect(&self) -> Client {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        Client { stream }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    stream: TcpStream,
}

impl Client {
    /// 명령 여러 개를 한 번의 write로 붙여 보냅니다(파이프라이닝).
    fn send(&mut self, commands: &[&[&str]]) {
        let mut bytes = Vec::new();
        for command in commands {
            bytes.extend_from_slice(format!("*{}\r\n", command.len()).as_bytes());
            for part in *command {
                bytes.extend_from_slice(format!("${}\r\n{part}\r\n", part.len()).as_bytes());
            }
        }
        self.stream.write_all(&bytes).unwrap();
    }

    /// `expected`와 같은 길이만큼 읽어서 비교합니다. 2초 안에 안 오면 실패입니다.
    fn expect(&mut self, expected: &[u8]) {
        self.stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut actual = vec![0u8; expected.len()];
        if let Err(error) = self.stream.read_exact(&mut actual) {
            panic!(
                "{:?} 을(를) 기다렸는데 못 받았습니다: {error}",
                String::from_utf8_lossy(expected)
            );
        }
        assert_eq!(
            String::from_utf8_lossy(&actual),
            String::from_utf8_lossy(expected)
        );
    }

    /// `wait` 동안 아무 응답도 오지 않아야 합니다. 아직 자고 있다는 뜻입니다.
    fn expect_nothing(&mut self, wait: Duration) {
        self.stream.set_read_timeout(Some(wait)).unwrap();
        let mut byte = [0u8; 1];
        match self.stream.read(&mut byte) {
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            other => panic!("아무것도 안 와야 하는데 왔습니다: {other:?} {byte:?}"),
        }
    }
}

/// 서버가 BLPOP을 받아 명단에 올릴 시간을 줍니다. 소켓 밖에서는 명단을 볼 수 없어서 여기서만 sleep을 씁니다.
fn let_it_block() {
    thread::sleep(Duration::from_millis(150));
}

#[test]
fn 기다리던_손님이_넣자마자_받고_먼저_온_손님이_먼저_받는다() {
    let server = Server::start();
    let (mut a, mut b, mut c) = (server.connect(), server.connect(), server.connect());

    a.send(&[&["BLPOP", "q", "0"]]);
    let_it_block();
    b.send(&[&["BLPOP", "q", "0"]]);
    let_it_block();

    c.send(&[&["RPUSH", "q", "one"]]);
    c.expect(b":1\r\n");
    a.expect(b"*2\r\n$1\r\nq\r\n$3\r\none\r\n");
    b.expect_nothing(Duration::from_millis(200));

    c.send(&[&["RPUSH", "q", "two"]]);
    c.expect(b":1\r\n");
    b.expect(b"*2\r\n$1\r\nq\r\n$3\r\ntwo\r\n");
}

#[test]
fn 한_손님이_자는_동안_다른_손님은_막히지_않는다() {
    // 락을 쥔 채로 자면 여기서 SET의 응답이 오지 않습니다.
    let server = Server::start();
    let (mut sleeper, mut other) = (server.connect(), server.connect());

    sleeper.send(&[&["BLPOP", "q", "0"]]);
    let_it_block();
    other.send(&[&["SET", "k", "v"], &["GET", "k"], &["LLEN", "q"]]);
    other.expect(b"+OK\r\n$1\r\nv\r\n:0\r\n");
    sleeper.expect_nothing(Duration::from_millis(100));
}

#[test]
fn 시간이_다_되면_nil_배열을_받고_그_뒤에_넣은_값은_남는다() {
    let server = Server::start();
    let (mut waiter, mut pusher) = (server.connect(), server.connect());

    let start = Instant::now();
    waiter.send(&[&["BLPOP", "q", "0.3"]]);
    waiter.expect(b"*-1\r\n");
    assert!(start.elapsed() >= Duration::from_millis(300));

    pusher.send(&[&["RPUSH", "q", "late"], &["LRANGE", "q", "0", "-1"]]);
    pusher.expect(b":1\r\n*1\r\n$4\r\nlate\r\n");
}

#[test]
fn 자기_전에_앞_명령의_응답부터_보낸다() {
    let server = Server::start();
    let (mut waiter, mut pusher) = (server.connect(), server.connect());

    // PING과 BLPOP을 붙여 보내도 PONG은 바로 옵니다
    waiter.send(&[&["PING"], &["BLPOP", "q", "0"], &["ECHO", "after"]]);
    waiter.expect(b"+PONG\r\n");
    // BLPOP 뒤에 붙여 보낸 명령은 깨어난 다음에 처리됩니다
    waiter.expect_nothing(Duration::from_millis(200));

    pusher.send(&[&["LPUSH", "q", "job"]]);
    pusher.expect(b":1\r\n");
    waiter.expect(b"*2\r\n$1\r\nq\r\n$3\r\njob\r\n$5\r\nafter\r\n");
}

#[test]
fn 넣은_값은_붙여_보낸_다음_명령보다_대기자가_먼저_받는다() {
    let server = Server::start();
    let (mut waiter, mut pusher) = (server.connect(), server.connect());

    waiter.send(&[&["BRPOP", "q1", "q2", "0"]]);
    let_it_block();
    pusher.send(&[
        &["RPUSH", "q2", "a", "b", "c"],
        &["LPOP", "q2"],
        &["LPOP", "q2"],
        &["LPOP", "q2"],
        &["EXISTS", "q2"],
    ]);
    // 응답은 넣은 직후의 길이 3이고, 대기자가 뒤에서 c를 가져가서 세 번째 LPOP은 nil입니다
    pusher.expect(b":3\r\n$1\r\na\r\n$1\r\nb\r\n$-1\r\n:0\r\n");
    waiter.expect(b"*2\r\n$2\r\nq2\r\n$1\r\nc\r\n");
}

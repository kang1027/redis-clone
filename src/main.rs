mod command;
mod connection;
mod db;
mod reply;
mod resp;

use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use connection::Connection;
use db::Db;

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

    // 저장소는 서버에 하나만 있습니다.
    // 스레드는 main보다 오래 살 수 있어서 `&db`(빌림)를 넘길 수 없습니다. 그래서 `Arc`로 같이 소유합니다.
    let db = Arc::new(Db::default());

    for incoming in listener.incoming() {
        match incoming {
            // 커넥션 하나당 스레드 하나. 한 커넥션이 막혀도 다른 커넥션은 계속 처리됩니다.
            Ok(stream) => {
                // `Arc::clone`은 저장소를 복사하지 않습니다. 가리키는 쪽이 하나 늘었다는 숫자만 올립니다.
                let db = Arc::clone(&db);
                thread::spawn(move || Connection::new(stream, db).run());
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

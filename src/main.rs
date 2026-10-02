use std::{env, fmt, io, net::IpAddr};

use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
};

#[derive(Debug)]
enum Error {
    Io(io::Error),
    InvalidRequest,
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidRequest => write!(formatter, "잘못된 입력입니다"),
        }
    }
}

struct Config {
    bind: IpAddr,
    port: u16,
    max_message_bytes: usize,
    max_header_bytes: usize,
}

impl Config {
    fn from_args() -> Result<Self, Error> {
        let mut config = Self {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 6380,
            max_message_bytes: 1_048_576,
            max_header_bytes: 64,
        };

        let mut args = env::args().skip(1);
        while let Some(option) = args.next() {
            let value = args.next().ok_or(Error::InvalidRequest)?;
            match option.as_str() {
                "--bind" => config.bind = value.parse().map_err(|_| Error::InvalidRequest)?,
                "--port" => config.port = value.parse().map_err(|_| Error::InvalidRequest)?,
                "--max-message-bytes" => {
                    config.max_message_bytes = value.parse().map_err(|_| Error::InvalidRequest)?;
                }
                "--max-header-bytes" => {
                    config.max_header_bytes = value.parse().map_err(|_| Error::InvalidRequest)?;
                }
                _ => return Err(Error::InvalidRequest),
            }
        }

        if config.max_header_bytes == 0 {
            return Err(Error::InvalidRequest);
        }
        Ok(config)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let config = Config::from_args()?;
    let listener = TcpListener::bind((config.bind, config.port)).await?;

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("접속을 받지 못했습니다: {error}");
                continue;
            }
        };

        let max_message_bytes = config.max_message_bytes;
        let max_header_bytes = config.max_header_bytes;
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, max_message_bytes, max_header_bytes).await
            {
                eprintln!("{peer} 연결 종료: {error}");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    max_message_bytes: usize,
    max_header_bytes: usize,
) -> Result<(), Error> {
    let mut reader = BufReader::new(stream);

    while let Some(count) = read_length(&mut reader, b'*', max_header_bytes).await? {
        if count != 1 && count != 2 {
            return Err(Error::InvalidRequest);
        }

        let command = read_bulk(&mut reader, b"PING".len(), max_header_bytes).await?;
        if !command.eq_ignore_ascii_case(b"PING") {
            return Err(Error::InvalidRequest);
        }

        if count == 1 {
            reader.get_mut().write_all(b"+PONG\r\n").await?;
        } else {
            let message = read_bulk(&mut reader, max_message_bytes, max_header_bytes).await?;
            let header = format!("${}\r\n", message.len());
            let stream = reader.get_mut();
            stream.write_all(header.as_bytes()).await?;
            stream.write_all(&message).await?;
            stream.write_all(b"\r\n").await?;
        }
    }
    Ok(())
}

async fn read_length(
    reader: &mut BufReader<TcpStream>,
    prefix: u8,
    max_header_bytes: usize,
) -> Result<Option<usize>, Error> {
    let mut header = Vec::new();
    let bytes_read = reader
        .take(max_header_bytes as u64)
        .read_until(b'\n', &mut header)
        .await?;
    if bytes_read == 0 {
        return Ok(None);
    }
    if header.len() < 4 || header[0] != prefix || !header.ends_with(b"\r\n") {
        return Err(Error::InvalidRequest);
    }

    let digits = &header[1..header.len() - 2];
    if !digits.iter().all(u8::is_ascii_digit) {
        return Err(Error::InvalidRequest);
    }
    let length = std::str::from_utf8(digits)
        .map_err(|_| Error::InvalidRequest)?
        .parse()
        .map_err(|_| Error::InvalidRequest)?;
    Ok(Some(length))
}

async fn read_bulk(
    reader: &mut BufReader<TcpStream>,
    max_bytes: usize,
    max_header_bytes: usize,
) -> Result<Vec<u8>, Error> {
    let length = read_length(reader, b'$', max_header_bytes)
        .await?
        .ok_or(Error::InvalidRequest)?;
    if length > max_bytes {
        return Err(Error::InvalidRequest);
    }

    let mut data = Vec::new();
    data.try_reserve_exact(length)
        .map_err(|_| Error::InvalidRequest)?;
    data.resize(length, 0);
    reader.read_exact(&mut data).await?;
    let mut terminator = [0; 2];
    reader.read_exact(&mut terminator).await?;
    if terminator != *b"\r\n" {
        return Err(Error::InvalidRequest);
    }
    Ok(data)
}

mod http_req;
mod http_res;

use http_req::{Header, HeaderPos, MAX_HEADERS, ParseStatus};
use std::{
    io::Read,
    net::{TcpListener, TcpStream},
    time::Duration,
};

use crate::http_res::ResponseWriter;

const READ_TIMEOUT: Duration = Duration::from_secs(30);

fn handle_connection(stream: &mut TcpStream) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let mut header_positions = [HeaderPos::default(); MAX_HEADERS];
    let mut header_buf = [Header::default(); MAX_HEADERS];
    let mut read_bytes: usize = 0;

    loop {
        if read_bytes == buf.len() {
            return Err(std::io::Error::other("request too large for buffer"));
        }

        let n = stream.read(&mut buf[read_bytes..])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before complete request",
            ));
        }
        read_bytes += n;

        match http_req::parse_request(&buf[..read_bytes], &mut header_positions) {
            ParseStatus::Complete(positions) => {
                if read_bytes < positions.consumed + positions.body_len {
                    continue;
                }

                let request =
                    positions.resolve(&buf[..read_bytes], &header_positions, &mut header_buf);
                println!("{request} ({read_bytes} bytes read)");

                let res = b"<html><h1>Hello World</h1></html>";
                ResponseWriter::new(stream)
                    .status(200, b"OK")?
                    .write_header(b"Content-Type", b"text/html")?
                    .write_header(
                        b"Content-Length",
                        itoa::Buffer::new().format(res.len()).as_bytes(),
                    )?
                    .write_body(res)?
                    .end();

                return Ok(());
            }
            ParseStatus::Incomplete => {
                println!("Request incomplete");
            }
            ParseStatus::Error => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "parse error",
                ));
            }
        }
    }
}

fn main() {
    let listener = TcpListener::bind("0.0.0.0:9876").unwrap();

    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        println!("got connection from {:?}", stream.peer_addr());

        std::thread::spawn(move || {
            if let Err(e) = handle_connection(&mut stream) {
                eprintln!("connection error: {e}");
            }
        });
    }
}

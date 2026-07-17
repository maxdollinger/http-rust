mod http;

use http::{Header, HeaderPos, MAX_HEADERS, ParseStatus};
use std::{io::Read, net::TcpListener, time::Duration};

const READ_TIMEOUT: Duration = Duration::from_secs(30);

fn main() {
    let listener = TcpListener::bind("0.0.0.0:9876").unwrap();

    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        stream.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        println!("got connection from {:?}", stream.peer_addr());

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut header_positions = [HeaderPos::default(); MAX_HEADERS];
            let mut header_buf = [Header::default(); MAX_HEADERS];
            let mut read_bytes: usize = 0;

            loop {
                if read_bytes == buf.len() {
                    eprintln!("request too large for buffer");
                    return;
                }

                let n = match stream.read(&mut buf[read_bytes..]) {
                    Ok(0) => {
                        eprintln!("connection closed before complete request");
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("read error: {e}");
                        return;
                    }
                };
                read_bytes += n;

                match http::parse_request(&buf[..read_bytes], &mut header_positions) {
                    ParseStatus::Complete(positions) => {
                        if read_bytes < positions.consumed + positions.body_len {
                            continue;
                        }

                        let request = positions.resolve(
                            &buf[..read_bytes],
                            &header_positions,
                            &mut header_buf,
                        );
                        println!("{request} ({read_bytes} bytes read)");
                        break;
                    }
                    ParseStatus::Incomplete => println!("Request incomplete"),
                    ParseStatus::Error => {
                        println!("Parse Error");
                        break;
                    }
                };
            }
        });
    }
}

use http_req::{Header, HeaderPos, MAX_HEADERS, ParseStatus};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::fd::{AsRawFd, RawFd},
};

use crate::{
    epoll::{ConnState, Connection, Epoll},
    http_req,
    http_res::ResponseWriter,
};

enum ReadOutcome {
    Pending,
    Close,
    Response { buf: [u8; 8192], len: usize },
}

fn handle_read(
    stream: &mut TcpStream,
    buf: &mut [u8; 4096],
    read_bytes: &mut usize,
    header_positions: &mut [HeaderPos; MAX_HEADERS],
) -> ReadOutcome {
    loop {
        if *read_bytes == buf.len() {
            return ReadOutcome::Close;
        }

        match stream.read(&mut buf[*read_bytes..]) {
            Ok(0) => return ReadOutcome::Close,
            Ok(n) => *read_bytes += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return ReadOutcome::Pending,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return ReadOutcome::Close,
        }

        match http_req::parse_request(&buf[..*read_bytes], header_positions) {
            ParseStatus::Complete(positions) => {
                if *read_bytes < positions.consumed + positions.body_len {
                    continue;
                }
                let mut header_buf = [Header::default(); MAX_HEADERS];
                let request =
                    positions.resolve(&buf[..*read_bytes], &header_positions[..], &mut header_buf);
                return build_response(&request);
            }
            ParseStatus::Incomplete => continue,
            ParseStatus::Error => return ReadOutcome::Close,
        }
    }
}

fn build_response(request: &http_req::Request) -> ReadOutcome {
    let body = b"<html><h1>Hello World, with epoll</h1></html>";
    let mut buf = [0u8; 8192];
    let len = {
        let mut cursor = std::io::Cursor::new(&mut buf[..]);
        let result = ResponseWriter::new(&mut cursor)
            .status(200, b"OK")
            .and_then(|w| w.write_header(b"Content-Type", b"text/html"))
            .and_then(|w| {
                w.write_header(
                    b"Content-Length",
                    itoa::Buffer::new().format(body.len()).as_bytes(),
                )
            })
            .and_then(|w| w.write_body(body));
        if result.is_err() {
            return ReadOutcome::Close;
        }
        cursor.position() as usize
    };
    ReadOutcome::Response { buf, len }
}

fn handle_write(
    stream: &mut TcpStream,
    buf: &[u8; 8192],
    len: usize,
    written_bytes: &mut usize,
) -> ReadOutcome {
    loop {
        if *written_bytes == len {
            return ReadOutcome::Close;
        }

        match stream.write(&buf[*written_bytes..len]) {
            Ok(0) => return ReadOutcome::Close,
            Ok(n) => *written_bytes += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return ReadOutcome::Pending,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return ReadOutcome::Close,
        }
    }
}

fn handle_connection(fd: RawFd, epoll: &Epoll, connections: &mut [Option<Connection>]) {
    let Some(conn) = connections[fd as usize].as_mut() else {
        return;
    };

    let outcome = match &mut conn.state {
        ConnState::Reading {
            buf,
            read_bytes,
            header_positions,
        } => handle_read(&mut conn.stream, buf, read_bytes, header_positions),
        ConnState::Writing {
            buf,
            len,
            written_bytes,
        } => handle_write(&mut conn.stream, buf, *len, written_bytes),
    };

    match outcome {
        ReadOutcome::Pending => {}
        ReadOutcome::Close => connections[fd as usize] = None,
        ReadOutcome::Response { buf, len } => {
            conn.state = ConnState::Writing {
                buf,
                len,
                written_bytes: 0,
            };
            if epoll
                .modify(fd, (libc::EPOLLOUT | libc::EPOLLET) as u32)
                .is_err()
            {
                connections[fd as usize] = None;
            }
        }
    }
}

fn accept_all(
    listener: &TcpListener,
    epoll: &Epoll,
    connections: &mut [Option<Connection>],
) -> std::io::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                stream.set_nonblocking(true)?;
                let fd = stream.as_raw_fd();

                epoll.add(fd, (libc::EPOLLIN | libc::EPOLLET) as u32)?;

                connections[fd as usize] = Some(Connection {
                    stream,
                    state: ConnState::Reading {
                        buf: [0u8; 4096],
                        read_bytes: 0,
                        header_positions: [HeaderPos::default(); MAX_HEADERS],
                    },
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

pub fn run() -> std::io::Result<()> {
    let listener = TcpListener::bind("0.0.0.0:9876").unwrap();
    listener.set_nonblocking(true).unwrap();
    let listener_fd = listener.as_raw_fd();

    let epoll = Epoll::new()?;
    epoll.add(listener_fd, libc::EPOLLIN as u32)?;

    let capacity = max_fds()?;
    let mut connections: Vec<Option<Connection>> = (0..capacity).map(|_| None).collect();
    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 1024];

    loop {
        let n = epoll.wait(&mut events, -1)?;

        for event in events.iter().take(n) {
            let fd = event.u64 as RawFd;

            if fd == listener_fd {
                accept_all(&listener, &epoll, &mut connections)?;
            } else {
                handle_connection(fd, &epoll, &mut connections);
            }
        }
    }
}

fn max_fds() -> std::io::Result<usize> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let ret = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(limit.rlim_cur as usize)
}

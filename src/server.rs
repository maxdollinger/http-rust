use http_req::{Header, HeaderPos, MAX_HEADERS, ParseStatus};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

use crate::{
    epoll::{ConnState, Connection, Epoll},
    http_req,
    http_res::ResponseWriter,
};

enum ReadOutcome {
    Pending,
    Close,
    Reset,
    Response {
        buf: [u8; 8192],
        len: usize,
        keep_alive: bool,
    },
}

fn wants_keep_alive(request: &http_req::Request) -> bool {
    !request.headers.iter().any(|h| {
        h.name.eq_ignore_ascii_case(b"connection") && h.value.eq_ignore_ascii_case(b"close")
    })
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
                let keep_alive = wants_keep_alive(&request);
                return build_response(&request, keep_alive);
            }
            ParseStatus::Incomplete => continue,
            ParseStatus::Error => return ReadOutcome::Close,
        }
    }
}

fn build_response(_request: &http_req::Request, keep_alive: bool) -> ReadOutcome {
    let body = b"<html><h1>Hello World, with epoll</h1></html>";
    let connection_value: &[u8] = if keep_alive { b"keep-alive" } else { b"close" };
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
            .and_then(|w| w.write_header(b"Connection", connection_value))
            .and_then(|w| w.write_body(body));
        if result.is_err() {
            return ReadOutcome::Close;
        }
        cursor.position() as usize
    };
    ReadOutcome::Response {
        buf,
        len,
        keep_alive,
    }
}

fn handle_write(
    stream: &mut TcpStream,
    buf: &[u8; 8192],
    len: usize,
    written_bytes: &mut usize,
    keep_alive: bool,
) -> ReadOutcome {
    loop {
        if *written_bytes == len {
            return if keep_alive {
                ReadOutcome::Reset
            } else {
                ReadOutcome::Close
            };
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
            keep_alive,
        } => handle_write(&mut conn.stream, buf, *len, written_bytes, *keep_alive),
    };

    match outcome {
        ReadOutcome::Pending => {}
        ReadOutcome::Close => connections[fd as usize] = None,
        ReadOutcome::Reset => {
            conn.state = ConnState::Reading {
                buf: [0u8; 4096],
                read_bytes: 0,
                header_positions: [HeaderPos::default(); MAX_HEADERS],
            };
            if epoll
                .modify(fd, (libc::EPOLLIN | libc::EPOLLET) as u32)
                .is_err()
            {
                connections[fd as usize] = None;
            }
        }
        ReadOutcome::Response {
            buf,
            len,
            keep_alive,
        } => {
            conn.state = ConnState::Writing {
                buf,
                len,
                written_bytes: 0,
                keep_alive,
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

const MAX_CONNECTIONS: u64 = 4096;

fn set_sockopt(fd: RawFd, opt: libc::c_int) -> std::io::Result<()> {
    let value: libc::c_int = 1;
    let ret = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            opt,
            &value as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Builds a listening socket by hand via libc so `SO_REUSEPORT` can be set —
/// `std::net::TcpListener::bind` has no way to request it. The kernel load-balances
/// incoming connections across every socket bound this way to the same port.
fn reuseport_listener(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let SocketAddr::V4(addr) = addr else {
        return Err(std::io::Error::other("only IPv4 is supported"));
    };

    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };

    set_sockopt(fd, libc::SO_REUSEPORT)?;
    set_sockopt(fd, libc::SO_REUSEADDR)?;

    let sockaddr = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: addr.port().to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(addr.ip().octets()),
        },
        sin_zero: [0; 8],
    };

    let ret = unsafe {
        libc::bind(
            fd,
            &sockaddr as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }

    // libc::SOMAXCONN is a compile-time constant (128), not the kernel's actual
    // net.core.somaxconn sysctl — request a much larger backlog explicitly. The
    // kernel clamps this to whatever net.core.somaxconn actually allows, so
    // over-asking here is harmless.
    let ret = unsafe { libc::listen(fd, MAX_CONNECTIONS as i32) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(TcpListener::from(owned_fd))
}

fn run_event_loop(addr: SocketAddr) -> std::io::Result<()> {
    let listener = reuseport_listener(addr)?;
    listener.set_nonblocking(true)?;
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

pub fn run() -> std::io::Result<()> {
    set_max_fds(MAX_CONNECTIONS)?;

    let addr: SocketAddr = "0.0.0.0:9876".parse().unwrap();
    let num_threads = std::thread::available_parallelism()?.get();

    let handles: Vec<_> = (0..num_threads)
        .map(|_| std::thread::spawn(move || run_event_loop(addr)))
        .collect();

    for handle in handles {
        match handle.join() {
            Ok(Err(e)) => eprintln!("event loop error: {e}"),
            Err(_) => eprintln!("event loop thread panicked"),
            Ok(Ok(())) => {}
        }
    }

    Ok(())
}

fn set_max_fds(limit: u64) -> std::io::Result<()> {
    let rl = libc::rlimit {
        rlim_cur: limit,
        rlim_max: limit,
    };
    let ret = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &rl) };
    if ret == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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

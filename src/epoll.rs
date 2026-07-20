use std::os::fd::RawFd;
use std::{io, net::TcpStream};

use crate::http_req::{HeaderPos, MAX_HEADERS};

pub struct Epoll {
    fd: RawFd,
}

impl Epoll {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::epoll_create1(0) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { fd })
    }

    pub fn add(&self, fd: RawFd, events: u32) -> io::Result<()> {
        self.epoll_ctl(libc::EPOLL_CTL_ADD, fd, events)
    }

    pub fn modify(&self, fd: RawFd, events: u32) -> io::Result<()> {
        self.epoll_ctl(libc::EPOLL_CTL_MOD, fd, events)
    }

    pub fn delete(&self, fd: RawFd) -> io::Result<()> {
        self.epoll_ctl(libc::EPOLL_CTL_DEL, fd, 0)
    }

    pub fn wait(&self, events: &mut [libc::epoll_event], timeout_ms: i32) -> io::Result<usize> {
        loop {
            let ret = unsafe {
                libc::epoll_wait(
                    self.fd,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    timeout_ms,
                )
            };

            if ret == -1 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }

                return Err(err);
            }
            return Ok(ret as usize);
        }
    }

    fn epoll_ctl(&self, op: libc::c_int, fd: RawFd, events: u32) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events,
            u64: fd as u64,
        };

        let ret = unsafe { libc::epoll_ctl(self.fd, op, fd, &mut event) };

        if ret == -1 {
            return Err(io::Error::last_os_error());
        }

        Ok(())
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

pub enum ConnState {
    Reading {
        buf: [u8; 4096],
        read_bytes: usize,
        header_positions: [HeaderPos; MAX_HEADERS],
    },
    Writing {
        buf: [u8; 8192],
        len: usize,
        written_bytes: usize,
    },
}

pub struct Connection {
    pub stream: TcpStream,
    pub state: ConnState,
}

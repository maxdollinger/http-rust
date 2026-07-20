use std::{io::Write, net::TcpStream};

pub struct WantStatus;
pub struct WantHeaders;
pub struct WantBody;

pub struct ResponseWriter<'a, S> {
    stream: &'a mut TcpStream,
    _state: std::marker::PhantomData<S>,
}

impl<'a> ResponseWriter<'a, WantStatus> {
    pub fn new(stream: &'a mut TcpStream) -> Self {
        ResponseWriter {
            stream,
            _state: std::marker::PhantomData,
        }
    }

    pub fn status(
        self,
        code: u16,
        reason: &[u8],
    ) -> std::io::Result<ResponseWriter<'a, WantHeaders>> {
        write!(self.stream, "HTTP/1.1 {code} ")?;
        self.stream.write_all(reason)?;
        self.stream.write_all(b"\r\n")?;

        Ok(ResponseWriter {
            stream: self.stream,
            _state: std::marker::PhantomData,
        })
    }
}

impl<'a> ResponseWriter<'a, WantHeaders> {
    pub fn write_header(
        self,
        name: &[u8],
        value: &[u8],
    ) -> std::io::Result<ResponseWriter<'a, WantHeaders>> {
        write!(
            self.stream,
            "{}: {}\r\n",
            String::from_utf8_lossy(name),
            String::from_utf8_lossy(value)
        )?;

        Ok(self)
    }

    pub fn write_body(self, body: &[u8]) -> std::io::Result<ResponseWriter<'a, WantBody>> {
        self.stream.write_all(b"\r\n")?;
        self.stream.write_all(body)?;

        Ok(ResponseWriter {
            stream: self.stream,
            _state: std::marker::PhantomData,
        })
    }
}

impl<'a> ResponseWriter<'a, WantBody> {
    pub fn end(self) -> Option<std::io::Error> {
        self.stream.flush().err()
    }
}

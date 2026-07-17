pub const MAX_HEADERS: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
pub struct Header<'b> {
    pub name: &'b [u8],
    pub value: &'b [u8],
}

#[derive(Debug)]
pub struct Request<'b> {
    pub method: &'b [u8],
    pub path: &'b [u8],
    pub minor_version: u8,
    pub headers: &'b [Header<'b>],
    pub body: &'b [u8],
}

impl<'b> std::fmt::Display for Request<'b> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{} {} HTTP/1.{}",
            String::from_utf8_lossy(self.method),
            String::from_utf8_lossy(self.path),
            self.minor_version
        )?;

        for header in self.headers {
            writeln!(
                f,
                "{}: {}",
                String::from_utf8_lossy(header.name),
                String::from_utf8_lossy(header.value)
            )?;
        }

        let len = 20.min(self.body.len());
        writeln!(f, "{}", String::from_utf8_lossy(&self.body[..len]))?;

        Ok(())
    }
}

/// Byte offsets into the request buffer for one header's name/value. No
/// lifetime parameter — plain data, so it never entangles with `buf`'s
/// borrow across loop iterations.
#[derive(Clone, Copy, Default)]
pub struct HeaderPos {
    name: (usize, usize),
    value: (usize, usize),
}

/// Where everything was found in `buf`, as offsets. Also lifetime-free.
pub struct ParsedPositions {
    method: (usize, usize),
    path: (usize, usize),
    minor_version: u8,
    num_headers: usize,
    pub body_len: usize,
    pub consumed: usize,
}

impl ParsedPositions {
    /// Turn the recorded offsets into an actual borrowed `Request`. Call
    /// this once you know parsing is complete — not from the read loop's
    /// repeated call site.
    pub fn resolve<'b>(
        &self,
        buf: &'b [u8],
        header_positions: &[HeaderPos],
        headers: &'b mut [Header<'b>],
    ) -> Request<'b> {
        for (i, hp) in header_positions[..self.num_headers].iter().enumerate() {
            headers[i] = Header {
                name: &buf[hp.name.0..hp.name.1],
                value: &buf[hp.value.0..hp.value.1],
            };
        }

        let body = if self.body_len < (buf.len() - self.consumed) {
            &buf[self.consumed..self.consumed + self.body_len]
        } else {
            &buf[self.consumed..]
        };

        Request {
            method: &buf[self.method.0..self.method.1],
            path: &buf[self.path.0..self.path.1],
            minor_version: self.minor_version,
            headers: &headers[..self.num_headers],
            body,
        }
    }
}

pub enum ParseStatus {
    Complete(ParsedPositions),
    Incomplete,
    Error,
}

pub fn parse_request(buf: &[u8], header_positions: &mut [HeaderPos]) -> ParseStatus {
    let method_end = match buf.iter().position(|&b| b == b' ') {
        Some(i) => i,
        None => return ParseStatus::Incomplete,
    };

    let rest = &buf[method_end + 1..];
    let path_end = match rest.iter().position(|&b| b == b' ') {
        Some(i) => i,
        None => return ParseStatus::Incomplete,
    };
    let path_start = method_end + 1;

    let rest = &rest[path_end + 1..];
    if rest.len() < 10 {
        return ParseStatus::Incomplete;
    }
    if &rest[..5] != b"HTTP/" || rest[5] != b'1' || rest[6] != b'.' {
        return ParseStatus::Error;
    }
    let minor_version = match rest[7] {
        b'0' => 0,
        b'1' => 1,
        _ => return ParseStatus::Error,
    };
    if &rest[8..10] != b"\r\n" {
        return ParseStatus::Error;
    }

    let mut pos = path_start + path_end + 1 + 10;
    let mut num_headers = 0;

    let is_get = &buf[..method_end] == b"GET";
    let mut body_len: usize = 0;
    loop {
        let remaining = &buf[pos..];
        if remaining.len() < 2 {
            return ParseStatus::Incomplete;
        }
        if &remaining[..2] == b"\r\n" {
            pos += 2;
            break;
        }

        if num_headers == header_positions.len() {
            return ParseStatus::Error;
        }

        let name_end = match remaining.iter().position(|&b| b == b':') {
            Some(i) => i,
            None => return ParseStatus::Incomplete,
        };

        let mut value_start = name_end + 1;
        while value_start < remaining.len() && matches!(remaining[value_start], b' ' | b'\t') {
            value_start += 1;
        }

        let value_rest = &remaining[value_start..];
        let line_end = match value_rest.iter().position(|&b| b == b'\r') {
            Some(i) => i,
            None => return ParseStatus::Incomplete,
        };
        if line_end + 1 >= value_rest.len() {
            return ParseStatus::Incomplete;
        }
        if value_rest[line_end + 1] != b'\n' {
            return ParseStatus::Error;
        }

        if !is_get && remaining[..name_end].eq_ignore_ascii_case(b"content-length") {
            let value =
                std::str::from_utf8(&remaining[value_start..value_start + line_end]).unwrap_or("0");
            body_len = value.parse::<usize>().unwrap_or(0);
        }

        header_positions[num_headers] = HeaderPos {
            name: (pos, pos + name_end),
            value: (pos + value_start, pos + value_start + line_end),
        };
        num_headers += 1;
        pos += value_start + line_end + 2;
    }

    ParseStatus::Complete(ParsedPositions {
        method: (0, method_end),
        path: (path_start, path_start + path_end),
        minor_version,
        num_headers,
        body_len,
        consumed: pos,
    })
}

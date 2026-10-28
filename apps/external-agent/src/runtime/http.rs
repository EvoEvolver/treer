//! Deliberately single-request HTTP/1.1 subset. No chunking, upgrades or keepalive.
use super::BODY_LIMIT;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
pub(super) struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub peer: Option<std::net::IpAddr>,
}
impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}
pub(super) fn read(s: &mut TcpStream) -> Result<Request, ()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::with_capacity(1024);
    let end = loop {
        if bytes.len() >= 16384 || Instant::now() >= deadline {
            return Err(());
        }
        s.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .map_err(|_| ())?;
        // Read one byte until the header delimiter: never overread pipelined bodies.
        let mut b = [0; 1];
        match s.read(&mut b) {
            Ok(1) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            _ => return Err(()),
        }
        bytes.push(b[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
    };
    let mut hs = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut hs);
    if req.parse(&bytes).map_err(|_| ())? != httparse::Status::Complete(end)
        || req.version != Some(1)
    {
        return Err(());
    }
    let method = req.method.ok_or(())?.to_owned();
    if !matches!(method.as_str(), "GET" | "POST") {
        return Err(());
    }
    let path = req.path.ok_or(())?.to_owned();
    // Percent-encoded Console appearance hints are allowed only on GET /.
    // The router validates every root query key/value; API paths remain strict.
    let root_query = method == "GET"
        && (path.starts_with("/?")
            || path.starts_with("/v1/inbox?")
            || path.starts_with("/api/tree?"));
    if !path.starts_with('/')
        || path.contains(['#', '\\'])
        || (!root_query && path.contains('%'))
        || path.len() > if root_query { 2050 } else { 256 }
    {
        return Err(());
    }
    let mut headers = BTreeMap::new();
    for h in req.headers.iter() {
        let key = h.name.to_ascii_lowercase();
        let val = std::str::from_utf8(h.value).map_err(|_| ())?;
        if val.bytes().any(|b| b < 32 || b == 127) || headers.insert(key, val.to_owned()).is_some()
        {
            return Err(());
        }
    }
    if !headers.contains_key("host")
        || headers.contains_key("transfer-encoding")
        || headers.contains_key("expect")
        || headers.contains_key("upgrade")
    {
        return Err(());
    }
    let len = match headers.get("content-length") {
        None => 0,
        Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse::<usize>().map_err(|_| ())?
        }
        _ => return Err(()),
    };
    if len > BODY_LIMIT || method == "GET" && len != 0 {
        return Err(());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut body = vec![0; len];
    let mut pos = 0;
    while pos < len {
        if Instant::now() >= deadline {
            return Err(());
        }
        s.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .map_err(|_| ())?;
        let n = match s.read(&mut body[pos..]) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(_) => return Err(()),
        };
        if n == 0 {
            return Err(());
        }
        pos += n;
    }
    Ok(Request {
        method,
        path,
        headers,
        body,
        peer: s.peer_addr().ok().map(|p| p.ip()),
    })
}
pub(super) struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json(status: u16, v: serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec(&v).unwrap_or_default(),
        }
    }
    pub fn error(status: u16, code: &str) -> Self {
        Self::json(status, serde_json::json!({"error":{"code":code}}))
    }
}
pub(super) fn write(s: &mut TcpStream, r: Response, ancestors: &str) {
    let _ = s.set_write_timeout(Some(Duration::from_secs(3)));
    let header=format!("HTTP/1.1 {} Response\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nVary: Accept, User-Agent\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; frame-ancestors {}; base-uri 'none'; form-action 'none'\r\n\r\n",r.status,r.content_type,r.body.len(),ancestors);
    let _ = s.write_all(header.as_bytes());
    let _ = s.write_all(&r.body);
}

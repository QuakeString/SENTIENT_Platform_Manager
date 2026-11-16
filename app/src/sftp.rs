// Copyright © 2016-2026 The SENTIENT Authors

//! A small SFTP (version 3) client over a child process: `ssh -s sftp` with
//! the instance's own sign-in settings and known_hosts
//! (crate::remote::sftp_command), or the server's sftp-server through sudo
//! for "Open as administrator". Blocking; one request at a time except for
//! reads and writes, which keep several in flight.

use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};

const INIT: u8 = 1;
const VERSION: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const EXTENDED: u8 = 200;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;
const EXTENDED_REPLY: u8 = 201;

const A_SIZE: u32 = 0x1;
const A_UIDGID: u32 = 0x2;
const A_PERM: u32 = 0x4;
const A_TIME: u32 = 0x8;
const A_EXT: u32 = 0x8000_0000;

pub const O_READ: u32 = 0x1;
pub const O_WRITE: u32 = 0x2;
pub const O_CREAT: u32 = 0x8;
pub const O_TRUNC: u32 = 0x10;

/// Bytes per read or write request (sftp-server takes up to 256 KB).
pub const CHUNK: usize = 64 * 1024;
/// Requests in flight while copying.
const IN_FLIGHT: usize = 16;

#[derive(Debug, Clone, Default)]
pub struct Attrs {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub perm: Option<u32>,
    pub mtime: Option<u32>,
}

impl Attrs {
    pub fn is_dir(&self) -> bool {
        self.perm.is_some_and(|p| p & 0o170000 == 0o040000)
    }
    pub fn is_link(&self) -> bool {
        self.perm.is_some_and(|p| p & 0o170000 == 0o120000)
    }
}

/// One entry of a folder: its name, attributes and owner's name (from the
/// server's long listing).
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub attrs: Attrs,
    pub owner: String,
}

pub struct Sftp {
    child: Child,
    to: ChildStdin,
    from: BufReader<ChildStdout>,
    next: u32,
}

/// An error from the server, in plain words, with its status code (2: no
/// such file, 3: permission denied).
#[derive(Debug, Clone)]
pub struct Failure {
    pub code: u32,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<std::io::Error> for Failure {
    fn from(e: std::io::Error) -> Self {
        Failure { code: 99, message: format!("The connection to the server broke: {e}") }
    }
}

impl From<Failure> for String {
    fn from(f: Failure) -> String {
        f.message
    }
}

pub type R<T> = Result<T, Failure>;

fn fail(code: u32, message: impl Into<String>) -> Failure {
    Failure { code, message: message.into() }
}

struct Buf(Vec<u8>);

impl Buf {
    fn new(kind: u8, id: Option<u32>) -> Self {
        let mut b = Buf(vec![0, 0, 0, 0, kind]);
        if let Some(id) = id {
            b.u32(id);
        }
        b
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
        self
    }
    fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }
    fn attrs(&mut self, a: &Attrs) -> &mut Self {
        let mut flags = 0;
        if a.perm.is_some() {
            flags |= A_PERM;
        }
        self.u32(flags);
        if let Some(p) = a.perm {
            self.u32(p);
        }
        self
    }
    fn done(mut self) -> Vec<u8> {
        let n = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&n.to_be_bytes());
        self.0
    }
}

struct Rd<'a>(&'a [u8]);

impl Rd<'_> {
    fn u32(&mut self) -> R<u32> {
        if self.0.len() < 4 {
            return Err(fail(98, "The server's answer was cut short."));
        }
        let v = u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]]);
        self.0 = &self.0[4..];
        Ok(v)
    }
    fn u64(&mut self) -> R<u64> {
        Ok(((self.u32()? as u64) << 32) | self.u32()? as u64)
    }
    fn bytes(&mut self) -> R<Vec<u8>> {
        let n = self.u32()? as usize;
        if self.0.len() < n {
            return Err(fail(98, "The server's answer was cut short."));
        }
        let v = self.0[..n].to_vec();
        self.0 = &self.0[n..];
        Ok(v)
    }
    fn string(&mut self) -> R<String> {
        Ok(String::from_utf8_lossy(&self.bytes()?).to_string())
    }
    fn attrs(&mut self) -> R<Attrs> {
        let flags = self.u32()?;
        let mut a = Attrs::default();
        if flags & A_SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & A_UIDGID != 0 {
            a.uid = Some(self.u32()?);
            a.gid = Some(self.u32()?);
        }
        if flags & A_PERM != 0 {
            a.perm = Some(self.u32()?);
        }
        if flags & A_TIME != 0 {
            let _atime = self.u32()?;
            a.mtime = Some(self.u32()?);
        }
        if flags & A_EXT != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }
}

fn status_words(code: u32, msg: &str, path: &str) -> String {
    match code {
        2 => format!("{path} is not there."),
        3 => format!("Permission denied: {path}."),
        4 if !msg.is_empty() && msg != "Failure" => format!("{path}: {msg}."),
        4 => format!("{path}: the server refused it (a folder that is not empty, or a full disk)."),
        _ if !msg.is_empty() => format!("{path}: {msg}."),
        _ => format!("{path}: error {code}."),
    }
}

impl Sftp {
    /// Start the session on a spawned `ssh` (stdin and stdout piped).
    /// `password`: written first, for sudo in front of sftp-server.
    pub fn start(mut child: Child, password: Option<&str>) -> Result<Sftp, String> {
        let mut to = child.stdin.take().ok_or("no stdin")?;
        let from = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        if let Some(pw) = password {
            writeln!(to, "{pw}").map_err(|e| e.to_string())?;
        }
        let mut s = Sftp { child, to, from, next: 1 };
        let mut init = Buf::new(INIT, None);
        init.u32(3);
        if let Err(e) = s.send(init.done()).and_then(|_| s.recv()).and_then(|(t, _)| if t == VERSION { Ok(()) } else { Err(fail(98, "The server does not speak SFTP.")) }) {
            let err = s.stderr_text();
            let _ = s.child.kill();
            let _ = s.child.wait();
            return Err(if err.is_empty() { e.message } else { crate::remote::explain(&err).unwrap_or(err) });
        }
        Ok(s)
    }

    fn stderr_text(&mut self) -> String {
        let mut text = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = self.child.kill();
            let _ = e.read_to_string(&mut text);
        }
        text.trim().to_string()
    }

    fn send(&mut self, packet: Vec<u8>) -> R<()> {
        self.to.write_all(&packet)?;
        Ok(())
    }

    fn recv(&mut self) -> R<(u8, Vec<u8>)> {
        let mut len = [0u8; 4];
        self.from.read_exact(&mut len)?;
        let n = u32::from_be_bytes(len) as usize;
        if n == 0 || n > 4 * 1024 * 1024 {
            return Err(fail(98, "The server sent a broken answer."));
        }
        let mut body = vec![0u8; n];
        self.from.read_exact(&mut body)?;
        let kind = body[0];
        body.remove(0);
        Ok((kind, body))
    }

    fn id(&mut self) -> u32 {
        self.next = self.next.wrapping_add(1);
        self.next
    }

    /// One request, its answer (after the id).
    fn call(&mut self, b: Buf) -> R<(u8, Vec<u8>)> {
        self.send(b.done())?;
        let (t, body) = self.recv()?;
        Ok((t, body[4.min(body.len())..].to_vec()))
    }

    fn expect_status(&mut self, b: Buf, path: &str) -> R<()> {
        let (t, body) = self.call(b)?;
        let mut r = Rd(&body);
        match t {
            STATUS => {
                let code = r.u32()?;
                let msg = r.string().unwrap_or_default();
                if code == 0 { Ok(()) } else { Err(fail(code, status_words(code, &msg, path))) }
            }
            _ => Err(fail(98, "The server sent an unexpected answer.")),
        }
    }

    fn status_err(body: &[u8], path: &str) -> Failure {
        let mut r = Rd(body);
        let code = r.u32().unwrap_or(4);
        let msg = r.string().unwrap_or_default();
        fail(code, status_words(code, &msg, path))
    }

    fn handle(&mut self, b: Buf, path: &str) -> R<Vec<u8>> {
        let (t, body) = self.call(b)?;
        match t {
            HANDLE => Rd(&body).bytes(),
            STATUS => Err(Self::status_err(&body, path)),
            _ => Err(fail(98, "The server sent an unexpected answer.")),
        }
    }

    pub fn realpath(&mut self, path: &str) -> R<String> {
        let id = self.id();
        let mut b = Buf::new(REALPATH, Some(id));
        b.str(path);
        let (t, body) = self.call(b)?;
        match t {
            NAME => {
                let mut r = Rd(&body);
                r.u32()?;
                r.string()
            }
            STATUS => Err(Self::status_err(&body, path)),
            _ => Err(fail(98, "The server sent an unexpected answer.")),
        }
    }

    fn stat_kind(&mut self, kind: u8, path: &str) -> R<Attrs> {
        let id = self.id();
        let mut b = Buf::new(kind, Some(id));
        b.str(path);
        let (t, body) = self.call(b)?;
        match t {
            ATTRS => Rd(&body).attrs(),
            STATUS => Err(Self::status_err(&body, path)),
            _ => Err(fail(98, "The server sent an unexpected answer.")),
        }
    }

    /// Follows links (a link to a folder is a folder).
    pub fn stat(&mut self, path: &str) -> R<Attrs> {
        self.stat_kind(STAT, path)
    }

    pub fn lstat(&mut self, path: &str) -> R<Attrs> {
        self.stat_kind(LSTAT, path)
    }

    /// A folder's entries without "." and "..", links resolved to what
    /// they point at where they can be.
    pub fn list(&mut self, path: &str) -> R<Vec<DirEntry>> {
        let id = self.id();
        let mut b = Buf::new(OPENDIR, Some(id));
        b.str(path);
        let h = self.handle(b, path)?;
        let mut out = Vec::new();
        loop {
            let id = self.id();
            let mut b = Buf::new(READDIR, Some(id));
            b.bytes(&h);
            let (t, body) = self.call(b)?;
            match t {
                NAME => {
                    let mut r = Rd(&body);
                    for _ in 0..r.u32()? {
                        let name = r.string()?;
                        let long = r.string()?;
                        let attrs = r.attrs()?;
                        if name == "." || name == ".." {
                            continue;
                        }
                        let owner = long.split_whitespace().nth(2).unwrap_or("").to_string();
                        out.push(DirEntry { name, attrs, owner });
                    }
                }
                STATUS => {
                    let code = Rd(&body).u32()?;
                    if code == 1 {
                        break;
                    }
                    let _ = self.close(&h);
                    return Err(Self::status_err(&body, path));
                }
                _ => return Err(fail(98, "The server sent an unexpected answer.")),
            }
        }
        let _ = self.close(&h);
        for e in out.iter_mut().filter(|e| e.attrs.is_link()) {
            let p = join(path, &e.name);
            if let Ok(a) = self.stat(&p) {
                e.attrs = a;
            }
        }
        Ok(out)
    }

    fn close(&mut self, h: &[u8]) -> R<()> {
        let id = self.id();
        let mut b = Buf::new(CLOSE, Some(id));
        b.bytes(h);
        self.expect_status(b, "")
    }

    pub fn mkdir(&mut self, path: &str) -> R<()> {
        let id = self.id();
        let mut b = Buf::new(MKDIR, Some(id));
        b.str(path).attrs(&Attrs { perm: Some(0o755), ..Default::default() });
        self.expect_status(b, path)
    }

    pub fn rmdir(&mut self, path: &str) -> R<()> {
        let id = self.id();
        let mut b = Buf::new(RMDIR, Some(id));
        b.str(path);
        self.expect_status(b, path)
    }

    pub fn remove(&mut self, path: &str) -> R<()> {
        let id = self.id();
        let mut b = Buf::new(REMOVE, Some(id));
        b.str(path);
        self.expect_status(b, path)
    }

    pub fn rename(&mut self, from: &str, to: &str) -> R<()> {
        let id = self.id();
        let mut b = Buf::new(RENAME, Some(id));
        b.str(from).str(to);
        self.expect_status(b, from)
    }

    /// Free space where `path` is (statvfs@openssh.com), if the server says.
    pub fn free(&mut self, path: &str) -> Option<u64> {
        let id = self.id();
        let mut b = Buf::new(EXTENDED, Some(id));
        b.str("statvfs@openssh.com").str(path);
        let (t, body) = self.call(b).ok()?;
        if t != EXTENDED_REPLY {
            return None;
        }
        let mut r = Rd(&body);
        let _bsize = r.u64().ok()?;
        let frsize = r.u64().ok()?;
        let _blocks = r.u64().ok()?;
        let _bfree = r.u64().ok()?;
        let bavail = r.u64().ok()?;
        Some(bavail.saturating_mul(frsize))
    }

    pub fn open(&mut self, path: &str, flags: u32) -> R<Vec<u8>> {
        let id = self.id();
        let mut b = Buf::new(OPEN, Some(id));
        b.str(path).u32(flags).attrs(&Attrs { perm: (flags & O_CREAT != 0).then_some(0o644), ..Default::default() });
        self.handle(b, path)
    }

    pub fn close_handle(&mut self, h: &[u8]) -> R<()> {
        self.close(h)
    }

    /// Read the file from `offset` up to `size`, handing each piece to
    /// `sink` in order (false from it stops). Several requests stay in
    /// flight; a short piece before the end asks again from there.
    pub fn read_from(&mut self, h: &[u8], mut offset: u64, size: u64, sink: &mut dyn FnMut(&[u8]) -> bool, path: &str) -> R<()> {
        let mut pending: VecDeque<u32> = VecDeque::new();
        let mut asked = offset;
        let (mut resync, mut stop) = (false, false);
        loop {
            while !resync && !stop && pending.len() < IN_FLIGHT && asked < size {
                let id = self.id();
                let mut b = Buf::new(READ, Some(id));
                b.bytes(h).u64(asked).u32(CHUNK as u32);
                self.send(b.done())?;
                pending.push_back(id);
                asked += CHUNK as u64;
            }
            if pending.pop_front().is_none() {
                return Ok(());
            }
            let (t, body) = self.recv()?;
            let body = &body[4.min(body.len())..];
            match t {
                DATA if resync || stop => {}
                DATA => {
                    let data = Rd(body).bytes()?;
                    offset += data.len() as u64;
                    if !sink(&data) {
                        stop = true;
                    } else if data.len() < CHUNK && offset < size {
                        resync = true;
                    }
                }
                STATUS => {
                    let code = Rd(body).u32()?;
                    if code != 1 {
                        return Err(Self::status_err(body, path));
                    }
                    // The end came early (the file shrank).
                    stop = true;
                }
                _ => return Err(fail(98, "The server sent an unexpected answer.")),
            }
            if pending.is_empty() {
                if stop {
                    return Ok(());
                }
                if resync {
                    resync = false;
                    asked = offset;
                }
            }
        }
    }

    /// Write `source` at `offset` on, pieces from `next` until it gives
    /// none; several requests stay in flight. `next` returns Err to stop.
    pub fn write_from(&mut self, h: &[u8], mut offset: u64, next: &mut dyn FnMut() -> Result<Option<Vec<u8>>, String>, path: &str) -> R<u64> {
        let mut in_flight = 0usize;
        let mut done = false;
        let mut written = 0u64;
        loop {
            while !done && in_flight < IN_FLIGHT {
                match next().map_err(|e| fail(97, e))? {
                    Some(data) => {
                        let id = self.id();
                        let mut b = Buf::new(WRITE, Some(id));
                        b.bytes(h).u64(offset).bytes(&data);
                        self.send(b.done())?;
                        offset += data.len() as u64;
                        written += data.len() as u64;
                        in_flight += 1;
                    }
                    None => done = true,
                }
            }
            if in_flight == 0 {
                return Ok(written);
            }
            let (t, body) = self.recv()?;
            in_flight -= 1;
            let body = &body[4.min(body.len())..];
            if t != STATUS {
                return Err(fail(98, "The server sent an unexpected answer."));
            }
            let code = Rd(body).u32()?;
            if code != 0 {
                // Drain what is still in flight, then say why.
                let err = Self::status_err(body, path);
                for _ in 0..in_flight {
                    let _ = self.recv();
                }
                return Err(err);
            }
        }
    }
}

impl Drop for Sftp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `dir`/`name` on the server (forward slashes).
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

/// The folder above `path` on the server ("/" has none).
pub fn parent(path: &str) -> Option<String> {
    let t = path.trim_end_matches('/');
    if t.is_empty() {
        return None;
    }
    match t.rfind('/') {
        Some(0) => Some("/".into()),
        Some(i) => Some(t[..i].to_string()),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(join("/opt", "x"), "/opt/x");
        assert_eq!(join("/", "x"), "/x");
        assert_eq!(parent("/opt/sentient"), Some("/opt".into()));
        assert_eq!(parent("/opt"), Some("/".into()));
        assert_eq!(parent("/"), None);
    }

    #[test]
    fn packet_length() {
        let mut b = Buf::new(INIT, None);
        b.u32(3);
        assert_eq!(b.done(), vec![0, 0, 0, 5, INIT, 0, 0, 0, 3]);
    }
}

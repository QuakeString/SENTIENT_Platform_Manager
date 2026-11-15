// Copyright © 2016-2026 The SENTIENT Authors

//! A backup file on disk: one `.sentient-backup`, or, when it is larger than
//! the chosen part size, numbered parts `<name>.001`, `<name>.002`, … that
//! together hold the same bytes (copy them to a USB disk or a share one by
//! one). Beside it, `<name>.sha256` lists the SHA-256 of every file it is
//! made of, in `sha256sum` format, so it can be checked after copying
//! without the Platform Manager.
//!
//! A restore or a check takes the file, its first part, or the folder that
//! holds them.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::util::HashingWriter;
use crate::{Error, Result};

/// The default part size: 4 GB, under FAT32's file limit.
pub const DEFAULT_PART_SIZE: u64 = 4_000_000_000;

/// `<output>.001`, `<output>.002`, …
pub fn part_path(output: &Path, n: usize) -> PathBuf {
    let mut s = output.as_os_str().to_owned();
    s.push(format!(".{n:03}"));
    PathBuf::from(s)
}

/// `<output>.sha256`
pub fn sidecar_path(output: &Path) -> PathBuf {
    let mut s = output.as_os_str().to_owned();
    s.push(".sha256");
    PathBuf::from(s)
}

/// Writes into numbered parts of at most `part_size` bytes, hashing each.
pub struct PartWriter {
    output: PathBuf,
    part_size: u64,
    current: Option<HashingWriter<io::BufWriter<File>>>,
    written: u64,
    /// (file, sha256, bytes) of every finished part.
    done: Vec<(PathBuf, String, u64)>,
}

impl PartWriter {
    pub fn new(output: &Path, part_size: u64) -> Self {
        Self { output: output.to_path_buf(), part_size: part_size.max(1 << 20), current: None, written: 0, done: Vec::new() }
    }

    fn roll(&mut self) -> io::Result<()> {
        if let Some(w) = self.current.take() {
            let path = part_path(&self.output, self.done.len() + 1);
            let (sha, bytes) = w.finish();
            self.done.push((path, sha, bytes));
        }
        let path = part_path(&self.output, self.done.len() + 1);
        self.current = Some(HashingWriter::new(io::BufWriter::new(File::create(&path)?)));
        self.written = 0;
        Ok(())
    }

    /// Close the last part. One part only is renamed to the plain name.
    /// Returns the files written: (file, sha256, bytes).
    pub fn finish(mut self) -> io::Result<Vec<(PathBuf, String, u64)>> {
        if let Some(mut w) = self.current.take() {
            w.flush()?;
            let path = part_path(&self.output, self.done.len() + 1);
            let (sha, bytes) = w.finish();
            self.done.push((path, sha, bytes));
        }
        if self.done.len() == 1 {
            let (path, sha, bytes) = self.done.remove(0);
            std::fs::rename(&path, &self.output)?;
            return Ok(vec![(self.output.clone(), sha, bytes)]);
        }
        Ok(self.done)
    }
}

impl Write for PartWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.current.is_none() || self.written >= self.part_size {
            self.roll()?;
        }
        let room = (self.part_size - self.written) as usize;
        let n = buf.len().min(room);
        let w = self.current.as_mut().expect("a part is open");
        let n = w.write(&buf[..n])?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.current {
            Some(w) => w.flush(),
            None => Ok(()),
        }
    }
}

/// Write `<output>.sha256` for the files a backup is made of.
pub fn write_sidecar(output: &Path, files: &[(PathBuf, String, u64)]) -> Result<PathBuf> {
    let path = sidecar_path(output);
    let mut text = String::new();
    for (file, sha, _) in files {
        let name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        text.push_str(&format!("{sha}  {name}\n"));
    }
    std::fs::write(&path, text).map_err(|e| Error::msg(format!("write {}: {e}", path.display())))?;
    Ok(path)
}

/// The files a backup given as a file, its first part or its folder is made
/// of, in order; and the base name its sidecar and temp files go by.
pub fn resolve(input: &Path) -> Result<(PathBuf, Vec<PathBuf>)> {
    if input.is_dir() {
        // The one backup in the folder (its .001, or its plain file).
        let mut found: Vec<PathBuf> = std::fs::read_dir(input)
            .map_err(|e| Error::msg(format!("read {}: {e}", input.display())))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.to_string_lossy();
                n.ends_with(".sentient-backup") || n.ends_with(".sentient-backup.001")
            })
            .collect();
        found.sort();
        let first = found.into_iter().next().ok_or_else(|| Error::msg(format!("{} holds no SENTIENT backup", input.display())))?;
        return resolve(&first);
    }
    let s = input.to_string_lossy();
    let base = if s.ends_with(".001") { PathBuf::from(&s[..s.len() - 4]) } else { input.to_path_buf() };
    if base.is_file() {
        return Ok((base.clone(), vec![base]));
    }
    let mut parts = Vec::new();
    let mut n = 1;
    while part_path(&base, n).is_file() {
        parts.push(part_path(&base, n));
        n += 1;
    }
    if parts.is_empty() {
        return Err(Error::msg(format!("{} is not there", input.display())));
    }
    Ok((base, parts))
}

/// Reads the parts one after the other as one stream.
pub struct PartsReader {
    files: std::vec::IntoIter<PathBuf>,
    current: Option<File>,
}

impl Read for PartsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.current.is_none() {
                match self.files.next() {
                    Some(p) => self.current = Some(File::open(&p).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", p.display())))?),
                    None => return Ok(0),
                }
            }
            let n = self.current.as_mut().expect("open").read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            self.current = None;
        }
    }
}

/// Open a backup (file, first part or folder) as one stream.
pub fn open(input: &Path) -> Result<(PathBuf, PartsReader)> {
    let (base, files) = resolve(input)?;
    Ok((base, PartsReader { files: files.into_iter(), current: None }))
}

/// The total size of a backup's files.
pub fn total_size(input: &Path) -> u64 {
    resolve(input).map(|(_, files)| files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

/// Each file against the sidecar: (file name, matches). None without a sidecar.
pub fn check_sidecar(input: &Path) -> Result<Option<Vec<(String, bool)>>> {
    let (base, files) = resolve(input)?;
    let Ok(text) = std::fs::read_to_string(sidecar_path(&base)) else { return Ok(None) };
    let listed: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| l.split_once("  ").map(|(h, n)| (n.trim().to_string(), h.trim().to_lowercase())))
        .collect();
    let mut out = Vec::new();
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (got, _) = crate::util::sha256_file(&f)?;
        let ok = listed.iter().any(|(n, h)| *n == name && *h == got);
        out.push((name, ok));
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_split_into_parts_reads_back_whole_and_matches_its_sidecar() {
        let dir = std::env::temp_dir().join(format!("sbr-parts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("b.sentient-backup");
        let data: Vec<u8> = (0..(3 << 20) + 123).map(|i| (i % 251) as u8).collect();
        let mut w = PartWriter::new(&out, 1 << 20);
        w.write_all(&data).unwrap();
        let files = w.finish().unwrap();
        assert_eq!(files.len(), 4);
        write_sidecar(&out, &files).unwrap();
        // By the first part, by the base name and by the folder.
        for input in [part_path(&out, 1), out.clone(), dir.clone()] {
            let (_, mut r) = open(&input).unwrap();
            let mut back = Vec::new();
            r.read_to_end(&mut back).unwrap();
            assert_eq!(back, data);
        }
        assert!(check_sidecar(&out).unwrap().unwrap().iter().all(|(_, ok)| *ok));
        std::fs::write(part_path(&out, 2), b"damaged").unwrap();
        let checked = check_sidecar(&out).unwrap().unwrap();
        assert_eq!(checked.iter().filter(|(_, ok)| !ok).count(), 1);
        // A small backup is one plain file.
        let one = dir.join("c.sentient-backup");
        let mut w = PartWriter::new(&one, 1 << 20);
        w.write_all(b"small").unwrap();
        let files = w.finish().unwrap();
        assert_eq!(files[0].0, one);
        assert!(one.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

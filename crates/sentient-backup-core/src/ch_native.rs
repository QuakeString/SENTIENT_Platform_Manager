// Copyright © 2016-2026 The SENTIENT Authors

//! Reading ClickHouse's `Native` format without a ClickHouse server, for
//! restoring a ClickHouse installation's history into PostgreSQL.
//!
//! A backup holds ClickHouse's `ts_kv` as `SELECT * … FORMAT Native` (see
//! [`crate::clickhouse::export_all`]). That format is a series of blocks, each
//! `columns rows` (varints) and, per column, its name, its type and its
//! values, column by column. Only the types SENTIENT's `ts_kv` uses are read.
//! Rows come out as PostgreSQL `COPY … FROM STDIN` text lines.

use std::io::{BufRead, BufReader, Read};

use crate::error::{Error, Result};

/// The columns written per row, in this order: `ts_kv`'s, then
/// `inserted_at` (nanoseconds), which tells duplicates apart.
pub const COLUMNS: [&str; 9] = ["entity_id", "key", "ts", "bool_v", "str_v", "long_v", "dbl_v", "json_v", "inserted_at"];

#[derive(Debug, Clone, PartialEq)]
enum Type {
    Uuid,
    Int32,
    Int64,
    UInt8,
    Float64,
    String,
    /// DateTime64(precision[, tz]): an Int64 tick count.
    DateTime64,
    Nullable(Box<Type>),
}

fn parse_type(s: &str) -> Result<Type> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("Nullable(").and_then(|r| r.strip_suffix(')')) {
        return Ok(Type::Nullable(Box::new(parse_type(inner)?)));
    }
    if s.starts_with("DateTime64(") {
        return Ok(Type::DateTime64);
    }
    Ok(match s {
        "UUID" => Type::Uuid,
        "Int32" => Type::Int32,
        "Int64" => Type::Int64,
        "UInt8" => Type::UInt8,
        "Float64" => Type::Float64,
        "String" => Type::String,
        other => return Err(Error::msg(format!("ClickHouse column type {other} is not supported here"))),
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Uuid([u8; 16]),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
}

/// Reads varints, strings and fixed-size values; `None` at a clean end.
struct Input<R: BufRead>(R);

impl<R: BufRead> Input<R> {
    fn at_end(&mut self) -> Result<bool> {
        Ok(self.0.fill_buf()?.is_empty())
    }

    fn varint(&mut self) -> Result<u64> {
        let mut out = 0u64;
        for shift in (0..64).step_by(7) {
            let mut b = [0u8; 1];
            self.0.read_exact(&mut b)?;
            out |= u64::from(b[0] & 0x7f) << shift;
            if b[0] & 0x80 == 0 {
                return Ok(out);
            }
        }
        Err(Error::msg("ClickHouse data: varint too long"))
    }

    fn bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; n];
        self.0.read_exact(&mut v)?;
        Ok(v)
    }

    fn string(&mut self) -> Result<Vec<u8>> {
        let n = self.varint()? as usize;
        self.bytes(n)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut a = [0u8; N];
        self.0.read_exact(&mut a)?;
        Ok(a)
    }

    fn column(&mut self, ty: &Type, rows: usize) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(rows);
        match ty {
            Type::Nullable(inner) => {
                let nulls = self.bytes(rows)?;
                // Every row has a value in the nested column, nulls included.
                let values = self.column(inner, rows)?;
                for (null, v) in nulls.into_iter().zip(values) {
                    out.push(if null != 0 { Value::Null } else { v });
                }
            }
            _ => {
                for _ in 0..rows {
                    out.push(match ty {
                        Type::Uuid => {
                            // Two little-endian u64 halves, the high half first.
                            let raw = self.array::<16>()?;
                            let mut u = [0u8; 16];
                            for i in 0..8 {
                                u[i] = raw[7 - i];
                                u[8 + i] = raw[15 - i];
                            }
                            Value::Uuid(u)
                        }
                        Type::Int32 => Value::Int(i64::from(i32::from_le_bytes(self.array()?))),
                        Type::Int64 | Type::DateTime64 => Value::Int(i64::from_le_bytes(self.array()?)),
                        Type::UInt8 => Value::Int(i64::from(self.array::<1>()?[0])),
                        Type::Float64 => Value::Float(f64::from_le_bytes(self.array()?)),
                        Type::String => Value::Bytes(self.string()?),
                        Type::Nullable(_) => unreachable!(),
                    });
                }
            }
        }
        Ok(out)
    }
}

/// One value in PostgreSQL's COPY text format, for a column of `ts_kv`.
fn copy_text(column: &str, v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Null => out.extend_from_slice(b"\\N"),
        Value::Uuid(u) => {
            let hex: String = u.iter().map(|b| format!("{b:02x}")).collect();
            out.extend_from_slice(
                format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]).as_bytes(),
            );
        }
        // bool_v is UInt8 in ClickHouse, boolean in PostgreSQL.
        Value::Int(i) if column == "bool_v" => out.extend_from_slice(if *i != 0 { b"t" } else { b"f" }),
        Value::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Float(f) => {
            let s = if f.is_nan() {
                "NaN".to_string()
            } else if f.is_infinite() {
                if *f > 0.0 { "Infinity".into() } else { "-Infinity".into() }
            } else {
                // Rust prints the shortest text that reads back as the same double.
                f.to_string()
            };
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            for &c in b {
                match c {
                    b'\\' => out.extend_from_slice(b"\\\\"),
                    b'\t' => out.extend_from_slice(b"\\t"),
                    b'\n' => out.extend_from_slice(b"\\n"),
                    b'\r' => out.extend_from_slice(b"\\r"),
                    _ => out.push(c),
                }
            }
        }
    }
}

/// Convert a `Native` stream of `ts_kv` rows to COPY text lines with
/// [`COLUMNS`], handing them to `emit` a block at a time. Returns the rows.
pub fn to_copy_text(reader: impl Read, mut emit: impl FnMut(Vec<u8>) -> Result<()>) -> Result<u64> {
    let mut input = Input(BufReader::with_capacity(256 * 1024, reader));
    let mut total = 0u64;
    while !input.at_end()? {
        let columns = input.varint()? as usize;
        let rows = input.varint()? as usize;
        let mut by_name: Vec<(String, Vec<Value>)> = Vec::with_capacity(columns);
        for _ in 0..columns {
            let name = String::from_utf8_lossy(&input.string()?).into_owned();
            let ty = parse_type(&String::from_utf8_lossy(&input.string()?))?;
            let values = input.column(&ty, rows)?;
            by_name.push((name, values));
        }
        if rows == 0 {
            continue;
        }
        let cols: Vec<&Vec<Value>> = COLUMNS
            .iter()
            .map(|want| {
                by_name
                    .iter()
                    .find(|(name, _)| name == want)
                    .map(|(_, v)| v)
                    .ok_or_else(|| Error::msg(format!("ClickHouse data has no {want} column")))
            })
            .collect::<Result<_>>()?;
        let mut buf = Vec::with_capacity(rows * 96);
        for row in 0..rows {
            for (i, col) in cols.iter().enumerate() {
                if i > 0 {
                    buf.push(b'\t');
                }
                copy_text(COLUMNS[i], &col[row], &mut buf);
            }
            buf.push(b'\n');
        }
        emit(buf)?;
        total += rows as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn string(s: &[u8], out: &mut Vec<u8>) {
        varint(s.len() as u64, out);
        out.extend_from_slice(s);
    }

    /// One block of two rows, as ClickHouse writes it.
    fn block() -> Vec<u8> {
        let mut b = Vec::new();
        varint(9, &mut b);
        varint(2, &mut b);
        // UUID 00112233-4455-6677-8899-aabbccddeeff, twice.
        string(b"entity_id", &mut b);
        string(b"UUID", &mut b);
        for _ in 0..2 {
            b.extend([0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00]);
            b.extend([0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88]);
        }
        string(b"key", &mut b);
        string(b"Int32", &mut b);
        b.extend(7i32.to_le_bytes());
        b.extend((-1i32).to_le_bytes());
        string(b"ts", &mut b);
        string(b"Int64", &mut b);
        b.extend(1_700_000_000_000i64.to_le_bytes());
        b.extend(1_700_000_000_001i64.to_le_bytes());
        string(b"bool_v", &mut b);
        string(b"Nullable(UInt8)", &mut b);
        b.extend([0, 1, 1, 0]);
        string(b"str_v", &mut b);
        string(b"Nullable(String)", &mut b);
        b.extend([1, 0]);
        string(b"", &mut b);
        string(b"a\tb\\c\nd", &mut b);
        string(b"long_v", &mut b);
        string(b"Nullable(Int64)", &mut b);
        b.extend([0, 1]);
        b.extend((-42i64).to_le_bytes());
        b.extend(0i64.to_le_bytes());
        string(b"dbl_v", &mut b);
        string(b"Nullable(Float64)", &mut b);
        b.extend([0, 0]);
        b.extend(1.66f64.to_le_bytes());
        b.extend(f64::NAN.to_le_bytes());
        string(b"json_v", &mut b);
        string(b"Nullable(String)", &mut b);
        b.extend([0, 1]);
        string(br#"{"a":1}"#, &mut b);
        string(b"", &mut b);
        string(b"inserted_at", &mut b);
        string(b"DateTime64(9)", &mut b);
        b.extend(5i64.to_le_bytes());
        b.extend(6i64.to_le_bytes());
        b
    }

    #[test]
    fn reads_ts_kv_rows_as_copy_text() {
        let mut data = block();
        data.extend(block());
        let mut out = Vec::new();
        let rows = to_copy_text(&data[..], |b| {
            out.extend(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(rows, 4);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "00112233-4455-6677-8899-aabbccddeeff\t7\t1700000000000\tt\t\\N\t-42\t1.66\t{\"a\":1}\t5"
        );
        assert_eq!(
            lines[1],
            "00112233-4455-6677-8899-aabbccddeeff\t-1\t1700000000001\t\\N\ta\\tb\\\\c\\nd\t\\N\tNaN\t\\N\t6"
        );
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn rejects_types_it_cannot_read() {
        let mut b = Vec::new();
        varint(1, &mut b);
        varint(1, &mut b);
        string(b"x", &mut b);
        string(b"LowCardinality(String)", &mut b);
        assert!(to_copy_text(&b[..], |_| Ok(())).is_err());
    }
}

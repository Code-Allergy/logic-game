//! Memory contents from files: a ROM image, a RAM's initial contents
//! (`docs/mcp-server.md`, "Loading memories").
//!
//! ```json
//! {"file": "nestest.nes", "skip": 16, "length": 16384, "at": 0}
//! ```
//!
//! A binary file is cut into words of `word_bytes` bytes (by default
//! what the memory's width needs), least significant byte first unless
//! `big_endian`. A text file (`"format": "hex"`, or a name ending in
//! `.hex` or `.txt`) holds one hexadecimal word per token, separated by
//! white space or commas; `#` and `//` start a comment. Words may also be
//! given inline as `"data": [..]`.

use std::path::Path;

use serde_json::{Map, Value};

use crate::sim::source::parse_int_text;

/// Largest memory file read, in bytes.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// What to load and where.
#[derive(Debug, Clone, PartialEq)]
pub struct Load {
    /// The words, in order.
    pub words: Vec<u64>,
    /// Address of the first word.
    pub at: usize,
}

fn count(map: &mut Map<String, Value>, key: &str) -> Result<Option<usize>, String> {
    match map.shift_remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|v| Some(v as usize))
            .ok_or_else(|| format!("\"{key}\" must be a whole number")),
        Some(Value::String(s)) => parse_int_text(&s)
            .map(|v| Some(v as usize))
            .ok_or_else(|| format!("\"{key}\": \"{s}\" is not a number")),
        Some(_) => Err(format!("\"{key}\" must be a number")),
    }
}

/// Read a load description for a memory of words `width` bits wide.
/// Unknown fields are refused.
pub fn read(spec: &Value, width: u8) -> Result<Load, String> {
    let Value::Object(spec) = spec else {
        return Err("a load is an object: {\"file\": path} or {\"data\": [words]}".into());
    };
    let mut map = spec.clone();
    let at = count(&mut map, "at")?.unwrap_or(0);
    let skip = count(&mut map, "skip")?.unwrap_or(0);
    let length = count(&mut map, "length")?;
    let word_bytes = count(&mut map, "word_bytes")?;
    let big_endian = matches!(map.shift_remove("big_endian"), Some(Value::Bool(true)));
    let format = match map.shift_remove("format") {
        None | Some(Value::Null) => None,
        Some(Value::String(f)) if f == "bin" || f == "hex" => Some(f),
        Some(_) => return Err("\"format\" is \"bin\" or \"hex\"".into()),
    };
    let file = map.shift_remove("file");
    let data = map.shift_remove("data");
    if let Some(k) = map.keys().next() {
        return Err(format!(
            "unknown field \"{k}\" (a load has file or data, at, skip, length, word_bytes, \
             big_endian, format)"
        ));
    }
    let mut words = match (file, data) {
        (Some(Value::String(path)), None) => {
            let path = Path::new(&path);
            let hex = format.as_deref().map_or_else(
                || {
                    matches!(
                        path.extension().and_then(|e| e.to_str()),
                        Some("hex" | "txt")
                    )
                },
                |f| f == "hex",
            );
            let size = std::fs::metadata(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .len();
            if size > MAX_BYTES {
                return Err(format!(
                    "{} is {size} bytes; the limit is {MAX_BYTES}",
                    path.display()
                ));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
            if hex {
                hex_words(&String::from_utf8_lossy(&bytes))
                    .map_err(|e| format!("{}: {e}", path.display()))?
            } else {
                let n = word_bytes
                    .unwrap_or(usize::from(width).div_ceil(8))
                    .clamp(1, 8);
                bytes
                    .chunks(n)
                    .map(|chunk| {
                        chunk.iter().enumerate().fold(0u64, |word, (i, &b)| {
                            let shift = if big_endian { chunk.len() - 1 - i } else { i };
                            word | u64::from(b) << (8 * shift)
                        })
                    })
                    .collect()
            }
        }
        (None, Some(Value::Array(items))) => items
            .iter()
            .map(|v| match v {
                Value::Number(n) => n.as_u64().ok_or_else(|| format!("{n} is not a word")),
                Value::String(s) => {
                    parse_int_text(s).ok_or_else(|| format!("\"{s}\" is not a word"))
                }
                other => Err(format!("{other} is not a word")),
            })
            .collect::<Result<Vec<u64>, String>>()?,
        (Some(_), None) => return Err("\"file\" must be a path (a string)".into()),
        (None, Some(_)) => return Err("\"data\" must be an array of words".into()),
        (None, None) => return Err("a load needs \"file\" or \"data\"".into()),
        (Some(_), Some(_)) => return Err("give \"file\" or \"data\", not both".into()),
    };
    if skip > words.len() {
        return Err(format!(
            "\"skip\" is {skip} but there are only {} words",
            words.len()
        ));
    }
    words.drain(..skip);
    if let Some(length) = length {
        if length > words.len() {
            return Err(format!(
                "\"length\" is {length} but only {} words are left after \"skip\"",
                words.len()
            ));
        }
        words.truncate(length);
    }
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    if let Some(i) = words.iter().position(|w| *w & !mask != 0) {
        return Err(format!(
            "word {i} is 0x{:X}, wider than the memory's {width} bits (set \"word_bytes\"?)",
            words[i]
        ));
    }
    Ok(Load { words, at })
}

fn hex_words(text: &str) -> Result<Vec<u64>, String> {
    let mut words = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default();
        let line = line.split("//").next().unwrap_or_default();
        for token in line.split(|c: char| c.is_whitespace() || c == ',') {
            if token.is_empty() {
                continue;
            }
            let digits = token.trim_start_matches("0x").trim_start_matches("0X");
            let word = u64::from_str_radix(&digits.replace('_', ""), 16)
                .map_err(|_| format!("line {}: \"{token}\" is not a hex word", n + 1))?;
            words.push(word);
        }
    }
    Ok(words)
}

/// Put `load` into memory contents of `2^addr_bits` words (growing
/// `contents` as far as needed).
pub fn place(contents: &mut Vec<u64>, addr_bits: u8, load: &Load) -> Result<(), String> {
    let size = 1usize << addr_bits;
    let end = load.at + load.words.len();
    if end > size {
        return Err(format!(
            "{} words at {} end at {end}, past the memory's {size} words ({addr_bits} address \
             bits)",
            load.words.len(),
            load.at
        ));
    }
    if contents.len() < end {
        contents.resize(end, 0);
    }
    contents[load.at..end].copy_from_slice(&load.words);
    Ok(())
}

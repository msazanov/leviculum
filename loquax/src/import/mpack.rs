//! Reading the msgpack maps other messengers write: string keys, values of
//! whatever type their Python wrote, every field optional
//! (`docs/drafts/loquax-import-2026-10.md`, "Versions": no format here
//! carries a version, so a reader tolerates what it does not know).

use leviculum_lxmf::msgpack::{self, Error, Kind};

/// One map's entries: the key, a string or bytes, and the raw value.
pub struct Map<'a> {
    entries: Vec<(&'a [u8], &'a [u8])>,
}

impl<'a> Map<'a> {
    /// The map at the start of `d`. Keys that are neither strings nor bytes
    /// are kept as their raw encoding, which no lookup by name matches.
    pub fn read(d: &'a [u8]) -> Result<Self, Error> {
        let mut p = 0;
        Self::read_at(d, &mut p)
    }

    /// The map at `*p`, moving past it.
    pub fn read_at(d: &'a [u8], p: &mut usize) -> Result<Self, Error> {
        let count = msgpack::map_len(d, p)?;
        let mut entries = Vec::with_capacity(count.min(1024));
        for _ in 0..count {
            let key = text(d, p)?;
            let value = msgpack::raw(d, p)?;
            entries.push((key, value));
        }
        Ok(Self { entries })
    }

    /// The raw value under `key`.
    pub fn get(&self, key: &str) -> Option<&'a [u8]> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key.as_bytes())
            .map(|(_, v)| *v)
    }

    /// Every entry, key and raw value.
    pub fn entries(&self) -> impl Iterator<Item = (&'a [u8], &'a [u8])> + '_ {
        self.entries.iter().copied()
    }
}

/// A string or bytes value's bytes; any other value's raw encoding, so a
/// caller comparing names never matches it.
fn text<'a>(d: &'a [u8], p: &mut usize) -> Result<&'a [u8], Error> {
    match msgpack::peek_kind(d, *p)? {
        Kind::Str => msgpack::read_str(d, p).map(str::as_bytes),
        _ => {
            let start = *p;
            match msgpack::read_bin(d, p) {
                Ok(bytes) => Ok(bytes),
                Err(_) => {
                    *p = start;
                    msgpack::raw(d, p)
                }
            }
        }
    }
}

/// The value as bytes: a `bin`, or a `str`'s UTF-8.
pub fn bytes(value: &[u8]) -> Option<&[u8]> {
    let mut p = 0;
    match msgpack::peek_kind(value, 0).ok()? {
        Kind::Str => msgpack::read_str(value, &mut p).ok().map(str::as_bytes),
        _ => msgpack::read_bin(value, &mut p).ok(),
    }
}

/// The value as text, bytes taken as UTF-8 with the bad sequences replaced.
pub fn string(value: &[u8]) -> Option<String> {
    bytes(value).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// The value as an integer.
pub fn int(value: &[u8]) -> Option<i64> {
    msgpack::read_int(value, &mut 0).ok()
}

/// The value as a number, integer or float.
pub fn number(value: &[u8]) -> Option<f64> {
    msgpack::read_number_f64(value, &mut 0).ok()
}

/// The value as a boolean; `nil` and anything else is `None`.
pub fn boolean(value: &[u8]) -> Option<bool> {
    msgpack::read_bool(value, &mut 0).ok()
}

/// The value as an array's raw elements.
pub fn array(value: &[u8]) -> Option<Vec<&[u8]>> {
    let mut p = 0;
    let count = msgpack::array_len(value, &mut p).ok()?;
    let mut out = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        out.push(msgpack::raw(value, &mut p).ok()?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_as_strings_or_bytes_and_values_of_any_type_read_back() {
        let mut d = Vec::new();
        msgpack::map(&mut d, 4);
        msgpack::string(&mut d, "name");
        msgpack::string(&mut d, "Alice");
        msgpack::bin(&mut d, b"hash");
        msgpack::bin(&mut d, &[7; 16]);
        msgpack::string(&mut d, "n");
        msgpack::int(&mut d, -3);
        msgpack::int(&mut d, 5);
        msgpack::bool(&mut d, true);
        let map = Map::read(&d).expect("map");
        assert_eq!(map.get("name").and_then(string).as_deref(), Some("Alice"));
        assert_eq!(map.get("hash").and_then(bytes), Some(&[7u8; 16][..]));
        assert_eq!(map.get("n").and_then(int), Some(-3));
        assert_eq!(map.get("missing"), None);
        assert_eq!(map.entries().count(), 4);
    }
}

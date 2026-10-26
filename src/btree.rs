use std::io::{self, ErrorKind};

/// Page size used by every Passql on-disk format.
///
/// 4096 bytes aligns with most filesystem block sizes, balances IO
/// amplification against metadata overhead, and matches what SQLite, LMDB,
/// and most modern engines use.
pub const PAGE_SIZE: usize = 4096;

const PAGE_MAGIC: &[u8; 4] = b"PSL4";
const PAGE_HEADER_SIZE: usize = 16;
const RECORD_PAGE: u8 = 1;
const LEAF_PAGE: u8 = 2;
const INTERNAL_PAGE: u8 = 3;
pub(crate) const MAX_KEY_SIZE: usize = 1024;
pub(crate) const MAX_VALUE_SIZE: usize = PAGE_SIZE - PAGE_HEADER_SIZE - 6 - 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Node {
    Leaf(Vec<(String, u64)>),
    Internal {
        separators: Vec<String>,
        children: Vec<u64>,
    },
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn checksum(bytes: &[u8; PAGE_SIZE]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;

    for byte in bytes[..8].iter().chain(bytes[PAGE_HEADER_SIZE..].iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    hash
}

fn checked_key(key: &str) -> io::Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_SIZE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "key must contain 1 to 1024 bytes",
        ));
    }
    Ok(())
}

fn finish_page(page_type: u8, payload: &[u8]) -> io::Result<[u8; PAGE_SIZE]> {
    if payload.len() > PAGE_SIZE - PAGE_HEADER_SIZE {
        return Err(io::Error::new(
            ErrorKind::StorageFull,
            "B+ tree page contents exceed page capacity",
        ));
    }

    let mut page = [0u8; PAGE_SIZE];
    page[..4].copy_from_slice(PAGE_MAGIC);
    page[4] = page_type;
    page[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + payload.len()].copy_from_slice(payload);
    let hash = checksum(&page);
    page[8..16].copy_from_slice(&hash.to_le_bytes());
    Ok(page)
}

fn verify_page(page: &[u8; PAGE_SIZE]) -> io::Result<u8> {
    if &page[..4] != PAGE_MAGIC {
        return Err(invalid("invalid version 4 page signature"));
    }
    if page[5..8] != [0; 3] {
        return Err(invalid("invalid version 4 page reserved bytes"));
    }

    let stored = u64::from_le_bytes(
        page[8..16]
            .try_into()
            .map_err(|_| invalid("invalid version 4 page checksum field"))?,
    );
    if checksum(page) != stored {
        return Err(invalid("version 4 page checksum mismatch"));
    }

    Ok(page[4])
}

pub(crate) fn encode_record(key: &str, value: &[u8]) -> io::Result<[u8; PAGE_SIZE]> {
    checked_key(key)?;
    if value.len() > MAX_VALUE_SIZE || 6 + key.len() + value.len() > PAGE_SIZE - PAGE_HEADER_SIZE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "value exceeds version 4 record capacity",
        ));
    }

    let mut payload = Vec::with_capacity(6 + key.len() + value.len());
    payload.extend_from_slice(&(key.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
    payload.extend_from_slice(key.as_bytes());
    payload.extend_from_slice(value);
    finish_page(RECORD_PAGE, &payload)
}

pub(crate) fn decode_record(page: &[u8; PAGE_SIZE]) -> io::Result<(String, Vec<u8>)> {
    if verify_page(page)? != RECORD_PAGE {
        return Err(invalid("page is not a version 4 record"));
    }

    let payload = &page[PAGE_HEADER_SIZE..];
    let key_len = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let value_len = u32::from_le_bytes(
        payload[2..6]
            .try_into()
            .map_err(|_| invalid("invalid version 4 record length"))?,
    ) as usize;
    let end = 6usize
        .checked_add(key_len)
        .and_then(|length| length.checked_add(value_len))
        .ok_or_else(|| invalid("version 4 record length overflow"))?;

    if key_len == 0 || key_len > MAX_KEY_SIZE || value_len > MAX_VALUE_SIZE || end > payload.len() {
        return Err(invalid("invalid version 4 record lengths"));
    }
    if payload[end..].iter().any(|byte| *byte != 0) {
        return Err(invalid("nonzero padding in version 4 record page"));
    }

    let key_end = 6 + key_len;
    let key = std::str::from_utf8(&payload[6..key_end])
        .map_err(|_| invalid("version 4 key is not valid UTF 8"))?
        .to_owned();
    let value = payload[key_end..end].to_vec();
    Ok((key, value))
}

fn encode_node_payload(node: &Node) -> io::Result<(u8, Vec<u8>)> {
    let mut payload = Vec::new();
    match node {
        Node::Leaf(entries) => {
            let count = u16::try_from(entries.len())
                .map_err(|_| io::Error::new(ErrorKind::StorageFull, "too many leaf entries"))?;
            payload.extend_from_slice(&count.to_le_bytes());
            let mut previous: Option<&str> = None;
            for (key, record_id) in entries {
                checked_key(key)?;
                if *record_id == 0 {
                    return Err(invalid("record page zero is reserved"));
                }
                if previous.is_some_and(|prior| prior >= key.as_str()) {
                    return Err(invalid("B+ tree leaf keys are not ordered"));
                }
                previous = Some(key.as_str());
                payload.extend_from_slice(&(key.len() as u16).to_le_bytes());
                payload.extend_from_slice(key.as_bytes());
                payload.extend_from_slice(&record_id.to_le_bytes());
            }
            Ok((LEAF_PAGE, payload))
        }
        Node::Internal {
            separators,
            children,
        } => {
            if children.len() != separators.len() + 1 || children.is_empty() {
                return Err(invalid("invalid B+ tree child count"));
            }
            if children.contains(&0) {
                return Err(invalid("B+ tree child page zero is reserved"));
            }
            let count = u16::try_from(separators.len())
                .map_err(|_| io::Error::new(ErrorKind::StorageFull, "too many separators"))?;
            payload.extend_from_slice(&count.to_le_bytes());
            for child in children {
                payload.extend_from_slice(&child.to_le_bytes());
            }
            let mut previous: Option<&str> = None;
            for key in separators {
                checked_key(key)?;
                if previous.is_some_and(|prior| prior >= key.as_str()) {
                    return Err(invalid("B+ tree separators are not ordered"));
                }
                previous = Some(key.as_str());
                payload.extend_from_slice(&(key.len() as u16).to_le_bytes());
                payload.extend_from_slice(key.as_bytes());
            }
            Ok((INTERNAL_PAGE, payload))
        }
    }
}

pub(crate) fn encode_node(node: &Node) -> io::Result<[u8; PAGE_SIZE]> {
    let (page_type, payload) = encode_node_payload(node)?;
    finish_page(page_type, &payload)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| invalid("B+ tree node length overflow"))?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| invalid("truncated B+ tree node"))?;
        self.offset = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| invalid("invalid B+ tree integer"))?,
        ))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| invalid("invalid B+ tree page reference"))?,
        ))
    }

    fn key(&mut self) -> io::Result<String> {
        let length = usize::from(self.u16()?);
        if length == 0 || length > MAX_KEY_SIZE {
            return Err(invalid("invalid B+ tree key length"));
        }
        let key = std::str::from_utf8(self.take(length)?)
            .map_err(|_| invalid("B+ tree key is not valid UTF 8"))?
            .to_owned();
        checked_key(&key).map_err(|_| invalid("invalid B+ tree key"))?;
        Ok(key)
    }

    fn finish(self) -> io::Result<()> {
        if self.bytes[self.offset..].iter().any(|byte| *byte != 0) {
            return Err(invalid("nonzero padding in B+ tree node"));
        }
        Ok(())
    }
}

pub(crate) fn decode_node(page: &[u8; PAGE_SIZE]) -> io::Result<Node> {
    let page_type = verify_page(page)?;
    let mut cursor = Cursor::new(&page[PAGE_HEADER_SIZE..]);
    let count = usize::from(cursor.u16()?);

    let node = match page_type {
        LEAF_PAGE => {
            let mut entries = Vec::with_capacity(count);
            let mut previous: Option<String> = None;
            for _ in 0..count {
                let key = cursor.key()?;
                let record_id = cursor.u64()?;
                if record_id == 0 || previous.as_ref().is_some_and(|prior| prior >= &key) {
                    return Err(invalid("invalid B+ tree leaf ordering or record reference"));
                }
                previous = Some(key.clone());
                entries.push((key, record_id));
            }
            Node::Leaf(entries)
        }
        INTERNAL_PAGE => {
            let mut children = Vec::with_capacity(count + 1);
            for _ in 0..=count {
                let child = cursor.u64()?;
                if child == 0 {
                    return Err(invalid("B+ tree child page zero is reserved"));
                }
                children.push(child);
            }

            let mut separators = Vec::with_capacity(count);
            let mut previous: Option<String> = None;
            for _ in 0..count {
                let key = cursor.key()?;
                if previous.as_ref().is_some_and(|prior| prior >= &key) {
                    return Err(invalid("B+ tree separators are not ordered"));
                }
                previous = Some(key.clone());
                separators.push(key);
            }
            Node::Internal {
                separators,
                children,
            }
        }
        _ => return Err(invalid("unsupported version 4 page type")),
    };

    cursor.finish()?;
    Ok(node)
}

pub(crate) fn key_is_valid(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_and_detects_page_corruption() -> io::Result<()> {
        let encoded = encode_record("key", b"value")?;
        assert_eq!(
            decode_record(&encoded)?,
            ("key".to_owned(), b"value".to_vec())
        );

        let mut corrupt = encoded;
        corrupt[PAGE_HEADER_SIZE + 6] ^= 1;
        assert_eq!(
            decode_record(&corrupt).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        Ok(())
    }

    #[test]
    fn node_roundtrip_and_strict_ordering() -> io::Result<()> {
        let node = Node::Leaf(vec![("alpha".into(), 1), ("omega".into(), 2)]);
        assert_eq!(decode_node(&encode_node(&node)?)?, node);

        let invalid = Node::Leaf(vec![("z".into(), 1), ("a".into(), 2)]);
        assert_eq!(
            encode_node(&invalid).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        Ok(())
    }

    #[test]
    fn rejects_truncated_and_non_utf8_keys() -> io::Result<()> {
        let mut page = encode_node(&Node::Leaf(Vec::new()))?;
        page[PAGE_HEADER_SIZE..PAGE_HEADER_SIZE + 4].copy_from_slice(&[1, 0, 1, 0]);
        page[PAGE_HEADER_SIZE + 4] = 0xff;
        let hash = checksum(&page);
        page[8..16].copy_from_slice(&hash.to_le_bytes());
        assert_eq!(
            decode_node(&page).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        Ok(())
    }
}

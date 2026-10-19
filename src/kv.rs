use crate::{MAX_RECORD_SIZE, Record, Storage};
use std::io::{self, ErrorKind};

const MAGIC: &[u8; 4] = b"PKV\x01";
const HEADER_SIZE: usize = 8;
const MAX_KEY_SIZE: usize = 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn validate_key(key: &str) -> io::Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_SIZE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "key must contain 1 to 1024 bytes",
        ));
    }
    Ok(())
}

fn encode(key: &str, value: &[u8]) -> io::Result<Record> {
    validate_key(key)?;

    if HEADER_SIZE + key.len() + value.len() > MAX_RECORD_SIZE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "key and value exceed record capacity",
        ));
    }

    let mut data = Vec::with_capacity(HEADER_SIZE + key.len() + value.len());

    data.extend_from_slice(MAGIC);
    data.extend_from_slice(&(key.len() as u16).to_le_bytes());
    data.extend_from_slice(&(value.len() as u16).to_le_bytes());
    data.extend_from_slice(key.as_bytes());
    data.extend_from_slice(value);

    Ok(Record::new(data))
}

fn decode(data: &[u8]) -> io::Result<Option<(&[u8], &[u8])>> {
    if !data.starts_with(MAGIC) {
        return Ok(None);
    }

    if data.len() < HEADER_SIZE {
        return Err(invalid("truncated key-value header"));
    }

    let key_len = u16::from_le_bytes([data[4], data[5]]) as usize;
    let value_len = u16::from_le_bytes([data[6], data[7]]) as usize;

    if key_len == 0 || key_len > MAX_KEY_SIZE || HEADER_SIZE + key_len + value_len != data.len() {
        return Err(invalid("invalid key-value record lengths"));
    }

    let key_end = HEADER_SIZE + key_len;

    Ok(Some((&data[HEADER_SIZE..key_end], &data[key_end..])))
}

impl Storage {
    /// Store a new version of a key.
    /// Previous versions remain on disk.
    pub fn set(&mut self, key: &str, value: &[u8]) -> io::Result<u64> {
        let record = encode(key, value)?;
        self.append_record(&record)
    }

    /// Find the latest value by scanning pages in reverse order.
    pub fn lookup(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        validate_key(key)?;

        for page_id in (1..self.page_count()).rev() {
            let record = self.read_record(page_id)?;

            if let Some((stored_key, value)) = decode(&record.data)?
                && stored_key == key.as_bytes()
            {
                return Ok(Some(value.to_vec()));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-kv-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn encoding_roundtrip() -> io::Result<()> {
        let record = encode("user:1", b"Alice")?;
        let (key, value) =
            decode(&record.data)?.ok_or_else(|| invalid("missing key-value record"))?;

        assert_eq!(key, b"user:1");
        assert_eq!(value, b"Alice");
        Ok(())
    }

    #[test]
    fn validates_keys_and_sizes() {
        assert!(encode("", b"value").is_err());
        assert!(encode(&"k".repeat(1025), b"value").is_err());
        assert!(encode("key", &vec![0; MAX_RECORD_SIZE]).is_err());
    }

    #[test]
    fn updates_and_persists() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = Storage::create(&path)?;

            db.set("user:1", b"Alice")?;
            db.set("user:2", b"Bob")?;
            db.set("user:1", b"Alice Smith")?;

            assert_eq!(db.lookup("user:1")?, Some(b"Alice Smith".to_vec()));
            assert_eq!(db.lookup("user:2")?, Some(b"Bob".to_vec()));
            assert_eq!(db.lookup("missing")?, None);
        }

        {
            let mut db = Storage::open(&path)?;
            assert_eq!(db.lookup("user:1")?, Some(b"Alice Smith".to_vec()));
        }

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn preserves_binary_values() -> io::Result<()> {
        let path = test_path();

        let mut db = Storage::create(&path)?;
        let value = [0, 255, 1, 0, 42];

        db.set("binary", &value)?;
        assert_eq!(db.lookup("binary")?, Some(value.to_vec()));

        drop(db);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn ignores_existing_unkeyed_records() -> io::Result<()> {
        let path = test_path();
        let mut db = Storage::create(&path)?;

        db.append_record(&Record::new(b"legacy record".to_vec()))?;
        db.set("name", b"Passql")?;

        assert_eq!(db.lookup("name")?, Some(b"Passql".to_vec()));

        drop(db);
        std::fs::remove_file(path)?;
        Ok(())
    }
}

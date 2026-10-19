use crate::kv;
use crate::record::Record;
use crate::storage::PAGE_SIZE;
use crate::storage_v3::StorageV3;
use std::io::{self, ErrorKind};

const MAGIC: &[u8; 8] = b"pslcmmit";
const FORMAT_VERSION: u32 = 3;
pub const COMMIT_SIZE: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Commit {
    pub generation: u64,
    pub page_count: u64,
}

impl Commit {
    pub fn new(generation: u64, page_count: u64) -> io::Result<Self> {
        if generation == 0 || page_count == 0 {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "generation and page count must be positive",
            ));
        }

        Ok(Self {
            generation,
            page_count,
        })
    }

    pub fn encode(self) -> [u8; COMMIT_SIZE] {
        let mut bytes = [0u8; COMMIT_SIZE];

        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.generation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.page_count.to_le_bytes());

        let hash = checksum(&bytes[..32]);
        bytes[32..40].copy_from_slice(&hash.to_le_bytes());

        bytes
    }

    pub fn decode(bytes: &[u8; COMMIT_SIZE]) -> io::Result<Self> {
        if &bytes[..8] != MAGIC {
            return Err(invalid("invalid commit signature"));
        }

        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());

        if version != FORMAT_VERSION {
            return Err(invalid("unsupported commit format"));
        }

        if bytes[12..16] != [0; 4] {
            return Err(invalid("invalid reserved commit bytes"));
        }

        let stored = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        let actual = checksum(&bytes[..32]);

        if stored != actual {
            return Err(invalid("commit checksum mismatch"));
        }

        let generation = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let page_count = u64::from_le_bytes(bytes[24..32].try_into().unwrap());

        Self::new(generation, page_count).map_err(|_| invalid("invalid commit metadata"))
    }
}

pub fn latest(first: &[u8; COMMIT_SIZE], second: &[u8; COMMIT_SIZE]) -> io::Result<Commit> {
    match (Commit::decode(first), Commit::decode(second)) {
        (Ok(a), Ok(b)) => {
            if a.generation >= b.generation {
                Ok(a)
            } else {
                Ok(b)
            }
        }
        (Ok(commit), Err(_)) | (Err(_), Ok(commit)) => Ok(commit),
        (Err(_), Err(_)) => Err(invalid("no valid commit metadata")),
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

// FNV-1a 64-bit. Detects accidental corruption,
// not intentional modification.
fn checksum(data: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;

    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }

    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_roundtrip() -> io::Result<()> {
        let commit = Commit::new(1, 5)?;
        assert_eq!(Commit::decode(&commit.encode())?, commit);
        Ok(())
    }

    #[test]
    fn rejects_corruption() -> io::Result<()> {
        let mut bytes = Commit::new(1, 5)?.encode();
        bytes[24] ^= 0xff;

        assert!(Commit::decode(&bytes).is_err());
        Ok(())
    }

    #[test]
    fn selects_latest_generation() -> io::Result<()> {
        let a = Commit::new(1, 5)?.encode();
        let b = Commit::new(2, 8)?.encode();

        assert_eq!(latest(&a, &b)?.generation, 2);
        Ok(())
    }

    #[test]
    fn falls_back_when_newer_commit_is_corrupt() -> io::Result<()> {
        let a = Commit::new(1, 5)?.encode();
        let mut b = Commit::new(2, 8)?.encode();

        b[32] ^= 0xff;

        assert_eq!(latest(&a, &b)?, Commit::new(1, 5)?);
        Ok(())
    }

    #[test]
    fn rejects_two_invalid_commits() {
        let empty = [0u8; COMMIT_SIZE];

        assert!(latest(&empty, &empty).is_err());
    }

    #[test]
    fn rejects_zero_generation() {
        assert!(Commit::new(0, 1).is_err());
    }

    #[test]
    fn rejects_zero_page_count() {
        assert!(Commit::new(1, 0).is_err());
    }
}

pub struct Transaction<'a> {
    storage: &'a mut StorageV3,
    pages: Vec<[u8; PAGE_SIZE]>,
}

impl<'a> Transaction<'a> {
    fn new(storage: &'a mut StorageV3) -> Self {
        Self {
            storage,
            pages: Vec::new(),
        }
    }

    /// Stage a key-value write without modifying the database.
    pub fn set(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
        let record = kv::encode(key, value)?;
        self.pages.push(record.encode()?);
        Ok(())
    }

    /// Stage a binary record.
    pub fn append(&mut self, data: &[u8]) -> io::Result<()> {
        self.pages.push(Record::new(data).encode()?);
        Ok(())
    }

    pub fn pending(&self) -> usize {
        self.pages.len()
    }

    /// Publish all staged pages under one commit generation.
    ///
    /// If commit returns an I/O error, discard the database
    /// handle and reopen it for recovery before further writes.
    pub fn commit(self) -> io::Result<Vec<u64>> {
        let pages: Vec<&[u8]> = self.pages.iter().map(|page| page.as_slice()).collect();

        self.storage.append_batch(&pages)
    }

    /// Explicit rollback. No disk writes are performed.
    pub fn rollback(self) {
        // Staged pages are discarded when self is dropped.
    }
}

impl StorageV3 {
    /// Begin an append-only transaction.
    ///
    /// The mutable borrow prevents overlapping transactions
    /// through this particular database handle.
    pub fn begin(&mut self) -> Transaction<'_> {
        Transaction::new(self)
    }

    /// Look up the newest committed value for a key.
    ///
    /// This currently performs a reverse page scan.
    pub fn lookup(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        if key.is_empty() || key.len() > 1024 {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "key must contain 1 to 1024 bytes",
            ));
        }

        for id in (1..self.page_count()).rev() {
            let page = self.read_page(id)?;
            let record = Record::decode(&page)?;

            if let Some((stored_key, value)) = kv::decode(&record.data)?
                && stored_key == key.as_bytes()
            {
                return Ok(Some(value.to_vec()));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod transaction_api_tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!("passql-tx-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn commits_multiple_records() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;

            let mut tx = db.begin();
            tx.set("user:1", b"Alice")?;
            tx.set("user:2", b"Bob")?;

            assert_eq!(tx.pending(), 2);
            assert_eq!(tx.commit()?, vec![1, 2]);
            assert_eq!(db.generation(), 2);
        }

        {
            let mut db = StorageV3::open(&path)?;

            assert_eq!(db.lookup("user:1")?, Some(b"Alice".to_vec()));
            assert_eq!(db.lookup("user:2")?, Some(b"Bob".to_vec()));
        }

        std::fs::remove_file(path)
    }

    #[test]
    fn rollback_discards_changes() -> io::Result<()> {
        let path = test_path();
        let mut db = StorageV3::create(&path)?;

        {
            let mut tx = db.begin();
            tx.set("discarded", b"value")?;
            tx.rollback();
        }

        assert_eq!(db.page_count(), 1);
        assert_eq!(db.lookup("discarded")?, None);

        drop(db);
        std::fs::remove_file(path)
    }

    #[test]
    fn dropped_transaction_discards_changes() -> io::Result<()> {
        let path = test_path();
        let mut db = StorageV3::create(&path)?;

        {
            let mut tx = db.begin();
            tx.set("temporary", b"data")?;
        }

        assert_eq!(db.generation(), 1);
        assert_eq!(db.lookup("temporary")?, None);

        drop(db);
        std::fs::remove_file(path)
    }

    #[test]
    fn latest_committed_value_wins() -> io::Result<()> {
        let path = test_path();
        let mut db = StorageV3::create(&path)?;

        {
            let mut tx = db.begin();
            tx.set("name", b"Alice")?;
            tx.commit()?;
        }

        {
            let mut tx = db.begin();
            tx.set("name", b"Alice Smith")?;
            tx.commit()?;
        }

        assert_eq!(db.lookup("name")?, Some(b"Alice Smith".to_vec()));

        drop(db);
        std::fs::remove_file(path)
    }

    #[test]
    fn invalid_record_does_not_commit() -> io::Result<()> {
        let path = test_path();
        let mut db = StorageV3::create(&path)?;

        {
            let mut tx = db.begin();
            tx.set("valid", b"value")?;

            assert!(tx.set("", b"invalid").is_err());

            tx.rollback();
        }

        assert_eq!(db.page_count(), 1);
        assert_eq!(db.lookup("valid")?, None);

        drop(db);
        std::fs::remove_file(path)
    }
}

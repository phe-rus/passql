use crate::kv;
use crate::record::Record;
use crate::storage::PAGE_SIZE;
use crate::transaction::{COMMIT_SIZE, Commit};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: &[u8; 8] = b"passql3\0";
const FORMAT_VERSION: u32 = 3;
const SLOT_OFFSETS: [usize; 2] = [64, 104];

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// A read-only view of a committed version 3 database.
///
/// Multiple readers may coexist. Writers require exclusive access.
pub struct DatabaseReader {
    file: File,
    commit: Commit,
}

impl DatabaseReader {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).open(path)?;

        // Acquire a shared operating-system lock.
        // All Passql writers must respect the same locking protocol.
        file.try_lock_shared()?;

        let mut header = [0u8; PAGE_SIZE];
        file.read_exact(&mut header)?;

        if &header[..8] != MAGIC {
            return Err(invalid("invalid passql database signature"));
        }

        let version = u32::from_le_bytes(header[8..12].try_into().unwrap());

        let page_size = u32::from_le_bytes(header[12..16].try_into().unwrap());

        if version != FORMAT_VERSION {
            return Err(invalid("unsupported database version"));
        }

        if page_size != PAGE_SIZE as u32 {
            return Err(invalid("unsupported page size"));
        }

        let file_size = file.metadata()?.len();
        let mut selected: Option<Commit> = None;

        for offset in SLOT_OFFSETS {
            let bytes: &[u8; COMMIT_SIZE] =
                header[offset..offset + COMMIT_SIZE].try_into().unwrap();

            let Ok(candidate) = Commit::decode(bytes) else {
                continue;
            };

            let Some(required_size) = candidate.page_count.checked_mul(PAGE_SIZE as u64) else {
                continue;
            };

            if required_size > file_size {
                continue;
            }

            if selected
                .as_ref()
                .is_none_or(|current| candidate.generation > current.generation)
            {
                selected = Some(candidate);
            }
        }

        let commit = selected.ok_or_else(|| invalid("no readable commit"))?;

        Ok(Self { file, commit })
    }

    pub fn generation(&self) -> u64 {
        self.commit.generation
    }

    pub fn page_count(&self) -> u64 {
        self.commit.page_count
    }

    /// Verify every committed record page.
    ///
    /// Returns the number of successfully verified records.
    /// This does not validate indexes or prove crash durability.
    pub fn verify(&mut self) -> io::Result<u64> {
        let mut verified = 0u64;

        for id in 1..self.page_count() {
            self.read_record(id).map_err(|error| {
                io::Error::new(
                    ErrorKind::InvalidData,
                    format!("corrupt record at page {id}: {error}"),
                )
            })?;

            verified += 1;
        }

        Ok(verified)
    }

    pub fn read_page(&mut self, id: u64) -> io::Result<[u8; PAGE_SIZE]> {
        if id == 0 || id >= self.commit.page_count {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "page is not committed",
            ));
        }

        let offset = id
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("page offset overflow"))?;

        let mut page = [0u8; PAGE_SIZE];

        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut page)?;

        Ok(page)
    }

    pub fn read_record(&mut self, id: u64) -> io::Result<Record> {
        let page = self.read_page(id)?;
        Record::decode(&page)
    }

    pub fn lookup(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        if key.is_empty() || key.len() > 1024 {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "key must contain 1 to 1024 bytes",
            ));
        }

        for id in (1..self.page_count()).rev() {
            let record = self.read_record(id)?;

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
mod tests {
    use super::*;
    use crate::storage_v3::StorageV3;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> std::path::PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        std::env::temp_dir().join(format!("passql-reader-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn reads_committed_records() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            let mut tx = db.begin();

            tx.set("user:1", b"Alice")?;
            tx.set("user:2", b"Bob")?;
            tx.commit()?;
        }

        {
            let mut reader = DatabaseReader::open(&path)?;

            assert_eq!(reader.generation(), 2);
            assert_eq!(reader.page_count(), 3);
            assert_eq!(reader.lookup("user:1")?, Some(b"Alice".to_vec()));
            assert_eq!(reader.lookup("user:2")?, Some(b"Bob".to_vec()));
        }

        std::fs::remove_file(path)
    }

    #[test]
    fn allows_multiple_readers() -> io::Result<()> {
        let path = test_path();
        StorageV3::create(&path)?;

        let first = DatabaseReader::open(&path)?;
        let second = DatabaseReader::open(&path)?;

        assert_eq!(first.generation(), second.generation());

        // An exclusive writer cannot open while readers hold locks.
        assert!(StorageV3::open(&path).is_err());

        drop(first);
        drop(second);

        assert!(StorageV3::open(&path).is_ok());

        std::fs::remove_file(path)
    }

    #[test]
    fn opening_does_not_truncate_uncommitted_tail() -> io::Result<()> {
        let path = test_path();
        StorageV3::create(&path)?;

        {
            let mut file = OpenOptions::new().append(true).open(&path)?;
            file.write_all(&[42u8; PAGE_SIZE])?;
            file.sync_all()?;
        }

        let before = std::fs::metadata(&path)?.len();

        {
            let reader = DatabaseReader::open(&path)?;
            assert_eq!(reader.page_count(), 1);
        }

        let after = std::fs::metadata(&path)?.len();

        assert_eq!(before, after);
        assert_eq!(after, (PAGE_SIZE * 2) as u64);

        std::fs::remove_file(path)
    }

    #[test]
    fn verifies_committed_records() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            let mut tx = db.begin();

            tx.set("user:1", b"Alice")?;
            tx.set("user:2", b"Bob")?;
            tx.commit()?;
        }

        {
            let mut reader = DatabaseReader::open(&path)?;
            assert_eq!(reader.verify()?, 2);
        }

        std::fs::remove_file(path)
    }

    #[test]
    fn detects_corrupted_record_during_verification() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            let mut tx = db.begin();

            tx.set("name", b"Alice")?;
            tx.commit()?;
        }

        // Modify one payload byte in the committed record page.
        {
            let mut file = OpenOptions::new().write(true).open(&path)?;

            // Page 1 starts after the 4096-byte database header.
            // Byte 9 begins the record payload.
            file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 9))?;
            file.write_all(&[0xff])?;
            file.sync_all()?;
        }

        {
            let mut reader = DatabaseReader::open(&path)?;
            let error = reader.verify().unwrap_err();

            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert!(error.to_string().contains("page 1"));
        }

        std::fs::remove_file(path)
    }

    #[test]
    fn rejects_missing_committed_pages_without_writing() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            let mut tx = db.begin();
            tx.set("name", b"Alice")?;
            tx.commit()?;
        }

        {
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(PAGE_SIZE as u64)?;
        }

        let reader = DatabaseReader::open(&path)?;

        // The initial empty commit is still fully present.
        assert_eq!(reader.generation(), 1);
        assert_eq!(reader.page_count(), 1);

        drop(reader);

        // Unlike the writer's recovery path, the reader
        // must not invalidate the newer commit slot.
        let mut header = [0u8; PAGE_SIZE];

        {
            let mut file = File::open(&path)?;
            file.read_exact(&mut header)?;
        }

        let newer: &[u8; COMMIT_SIZE] = header[104..104 + COMMIT_SIZE].try_into().unwrap();

        assert_eq!(Commit::decode(newer)?.generation, 2);

        std::fs::remove_file(path)
    }
}

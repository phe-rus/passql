use crate::transaction::{COMMIT_SIZE, Commit};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

const PAGE_SIZE: usize = 4096;
const MAGIC: &[u8; 8] = b"passql3\0";
const FORMAT_VERSION: u32 = 3;
const SLOT_OFFSETS: [u64; 2] = [64, 104];

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

pub struct StorageV3 {
    file: File,
    commit: Commit,
    slot: usize,
    poisoned: bool,
}

impl StorageV3 {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;

        file.try_lock()?;
        let commit = Commit::new(1, 1)?;
        let mut header = [0u8; PAGE_SIZE];

        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
        header[64..64 + COMMIT_SIZE].copy_from_slice(&commit.encode());

        file.write_all(&header)?;
        file.sync_all()?;

        Ok(Self {
            file,
            commit,
            slot: 0,
            poisoned: false,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;

        file.try_lock()?;
        let mut header = [0u8; PAGE_SIZE];
        file.read_exact(&mut header)?;

        if &header[..8] != MAGIC {
            return Err(invalid("invalid version 3 database signature"));
        }

        let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let page_size = u32::from_le_bytes(header[12..16].try_into().unwrap());

        if version != FORMAT_VERSION || page_size != PAGE_SIZE as u32 {
            return Err(invalid("unsupported database format"));
        }

        let file_size = file.metadata()?.len();
        let mut selected: Option<(Commit, usize)> = None;

        for (slot, offset) in SLOT_OFFSETS.iter().enumerate() {
            let start = *offset as usize;

            let bytes: &[u8; COMMIT_SIZE] = header[start..start + COMMIT_SIZE].try_into().unwrap();

            if let Ok(commit) = Commit::decode(bytes) {
                let valid_size = commit
                    .page_count
                    .checked_mul(PAGE_SIZE as u64)
                    .is_some_and(|required| required <= file_size);

                let newer = selected
                    .as_ref()
                    .is_none_or(|(previous, _)| commit.generation > previous.generation);

                if valid_size && newer {
                    selected = Some((commit, slot));
                }
            }
        }

        let (commit, slot) = selected.ok_or_else(|| invalid("no recoverable commit"))?;

        let committed_size = commit
            .page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("committed size overflow"))?;

        // Invalidate rejected commit records before modifying the
        // file length. This prevents stale metadata from referring
        // to pages that may eventually contain different data.
        let mut invalidated = false;

        for (index, offset) in SLOT_OFFSETS.iter().enumerate() {
            if index == slot {
                continue;
            }

            let start = *offset as usize;
            let bytes: &[u8; COMMIT_SIZE] = header[start..start + COMMIT_SIZE].try_into().unwrap();

            if let Ok(candidate) = Commit::decode(bytes) {
                let stale = candidate.generation > commit.generation
                    || candidate.page_count > commit.page_count;

                if stale {
                    file.seek(SeekFrom::Start(*offset))?;
                    file.write_all(&[0u8; COMMIT_SIZE])?;
                    invalidated = true;
                }
            }
        }

        // Persist invalidation before truncating or reusing pages.
        if invalidated {
            file.sync_data()?;
        }

        if file_size > committed_size {
            file.set_len(committed_size)?;
            file.sync_all()?;
        }

        Ok(Self {
            file,
            commit,
            slot,
            poisoned: false,
        })
    }

    pub fn page_count(&self) -> u64 {
        self.commit.page_count
    }

    pub fn generation(&self) -> u64 {
        self.commit.generation
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn ensure_healthy(&self) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "database handle is poisoned; close and reopen for recovery",
            ));
        }

        Ok(())
    }

    pub fn read_page(&mut self, id: u64) -> io::Result<[u8; PAGE_SIZE]> {
        self.ensure_healthy()?;
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

    /// Append several pages and publish them as one batch.
    ///
    /// Requires exclusive single-process access.
    /// In-place page updates are not supported.
    pub fn append_batch(&mut self, pages: &[&[u8]]) -> io::Result<Vec<u64>> {
        self.ensure_healthy()?;
        if pages.is_empty() {
            return Ok(Vec::new());
        }

        if pages.iter().any(|page| page.len() > PAGE_SIZE) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "page exceeds 4096 bytes",
            ));
        }

        let number = u64::try_from(pages.len())
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "too many pages"))?;

        let count = self
            .commit
            .page_count
            .checked_add(number)
            .ok_or_else(|| invalid("page count overflow"))?;

        let generation = self
            .commit
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("generation overflow"))?;

        let next = Commit::new(generation, count)?;
        let start = self.commit.page_count;

        let offset = start
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("page offset overflow"))?;

        // From this point, an I/O failure may leave the file
        // in an uncertain state.
        self.poisoned = true;

        // Remove any incomplete tail from a previous failed append.
        self.file.set_len(offset)?;
        self.file.seek(SeekFrom::Start(offset))?;

        // Stage new pages.
        for data in pages {
            let mut page = [0u8; PAGE_SIZE];
            page[..data.len()].copy_from_slice(data);
            self.file.write_all(&page)?;
        }

        // Persist data before publishing commit metadata.
        self.file.sync_data()?;

        let next_slot = 1 - self.slot;

        self.file.seek(SeekFrom::Start(SLOT_OFFSETS[next_slot]))?;
        self.file.write_all(&next.encode())?;
        self.file.sync_data()?;

        // Publish the new state in memory only after sync succeeds.
        self.commit = next;
        self.slot = next_slot;
        self.poisoned = false;

        Ok((start..count).collect())
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

        std::env::temp_dir().join(format!("passql-v3-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn batch_persists() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;

            assert_eq!(db.append_batch(&[b"alice", b"bob"])?, vec![1, 2]);
            assert_eq!(db.generation(), 2);
        }

        let mut db = StorageV3::open(&path)?;

        assert_eq!(db.page_count(), 3);
        assert_eq!(&db.read_page(2)?[..3], b"bob");

        drop(db);
        std::fs::remove_file(path)
    }

    #[test]
    fn corrupt_newer_slot_falls_back() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            db.append_batch(&[b"first"])?;
            db.append_batch(&[b"second"])?;
        }

        // Generation 3 is stored in slot A.
        {
            let mut file = OpenOptions::new().write(true).open(&path)?;

            file.seek(SeekFrom::Start(SLOT_OFFSETS[0] + 32))?;
            file.write_all(&[0xff])?;
            file.sync_all()?;
        }

        let mut db = StorageV3::open(&path)?;

        assert_eq!(db.generation(), 2);
        assert_eq!(db.page_count(), 2);
        assert_eq!(&db.read_page(1)?[..5], b"first");

        drop(db);

        assert_eq!(std::fs::metadata(&path)?.len(), 2 * PAGE_SIZE as u64);

        std::fs::remove_file(path)
    }

    #[test]
    fn ignores_uncommitted_tail() -> io::Result<()> {
        let path = test_path();

        StorageV3::create(&path)?;

        {
            let mut file = OpenOptions::new().append(true).open(&path)?;
            file.write_all(&[7; PAGE_SIZE])?;
        }

        let db = StorageV3::open(&path)?;

        assert_eq!(db.page_count(), 1);
        drop(db);

        assert_eq!(std::fs::metadata(&path)?.len(), PAGE_SIZE as u64);

        std::fs::remove_file(path)
    }

    #[test]
    fn falls_back_when_newer_pages_are_missing() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            db.append_batch(&[b"one"])?;
        }

        // The earlier commit remains valid.
        {
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(PAGE_SIZE as u64)?;
        }

        let db = StorageV3::open(&path)?;

        assert_eq!(db.generation(), 1);
        assert_eq!(db.page_count(), 1);

        drop(db);
        std::fs::remove_file(path)
    }

    #[test]
    fn rejects_operations_on_poisoned_handle() -> io::Result<()> {
        let path = test_path();
        let mut db = StorageV3::create(&path)?;

        // Simulate an uncertain I/O failure.
        db.poisoned = true;

        assert!(db.is_poisoned());
        assert!(db.read_page(1).is_err());
        assert!(db.append_batch(&[b"data"]).is_err());

        drop(db);

        // Reopening performs the existing recovery procedure.
        let mut reopened = StorageV3::open(&path)?;

        assert!(!reopened.is_poisoned());
        assert_eq!(reopened.append_batch(&[b"recovered"])?, vec![1]);

        drop(reopened);
        std::fs::remove_file(path)
    }

    #[test]
    fn validation_error_does_not_poison_handle() -> io::Result<()> {
        let path = test_path();
        let mut db = StorageV3::create(&path)?;

        let oversized = vec![0u8; PAGE_SIZE + 1];

        assert!(db.append_batch(&[&oversized]).is_err());
        assert!(!db.is_poisoned());

        assert_eq!(db.append_batch(&[b"valid"])?, vec![1]);
        assert!(!db.is_poisoned());

        drop(db);
        std::fs::remove_file(path)
    }

    #[test]
    fn rejected_commit_cannot_become_valid_again() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;
            db.append_batch(&[b"first"])?;
        }

        // Simulate the committed data page becoming unavailable.
        {
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(PAGE_SIZE as u64)?;
        }

        // The engine must recover generation 1 and invalidate
        // generation 2 before allowing the old page position
        // to be reused.
        {
            let db = StorageV3::open(&path)?;

            assert_eq!(db.generation(), 1);
            assert_eq!(db.page_count(), 1);
        }

        // Simulate the file later growing to its former size.
        // This must not resurrect generation 2.
        {
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len((PAGE_SIZE * 2) as u64)?;
        }

        {
            let db = StorageV3::open(&path)?;

            assert_eq!(db.generation(), 1);
            assert_eq!(db.page_count(), 1);
        }

        std::fs::remove_file(path)
    }

    #[test]
    fn prevents_simultaneous_database_access() -> io::Result<()> {
        let path = test_path();

        let first = StorageV3::create(&path)?;

        // A second handle must not acquire an exclusive lock
        // while the first handle is alive.
        let second = StorageV3::open(&path);

        assert!(
            second.is_err(),
            "a second database handle acquired the lock"
        );

        // Closing the original handle releases its lock.
        drop(first);

        let reopened = StorageV3::open(&path)?;
        assert_eq!(reopened.generation(), 1);

        drop(reopened);
        std::fs::remove_file(path)
    }

    #[test]
    fn releases_lock_after_transaction() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = StorageV3::create(&path)?;

            let mut tx = db.begin();
            tx.set("name", b"Passql")?;
            tx.commit()?;

            // The database remains locked while the
            // original handle is alive.
            assert!(StorageV3::open(&path).is_err());
        }

        // The handle has been dropped, so another open
        // should succeed.
        let mut reopened = StorageV3::open(&path)?;

        assert_eq!(reopened.lookup("name")?, Some(b"Passql".to_vec()));

        drop(reopened);
        std::fs::remove_file(path)
    }
}

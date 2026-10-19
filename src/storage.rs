use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const PAGE_SIZE: usize = 4096;

const MAGIC: &[u8; 8] = b"passql\0\0";
const FORMAT_VERSION: u32 = 2;
const PAGE_COUNT_OFFSET: u64 = 16;
const HEADER_SIZE: usize = 24;

pub struct Storage {
    file: File,
    page_count: u64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

impl Storage {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;

        let mut header = [0u8; PAGE_SIZE];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
        header[16..24].copy_from_slice(&1u64.to_le_bytes());

        file.write_all(&header)?;
        file.sync_all()?;

        Ok(Self {
            file,
            page_count: 1,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;

        let mut header = [0u8; HEADER_SIZE];
        file.read_exact(&mut header)?;

        if &header[..8] != MAGIC {
            return Err(invalid("invalid passql database signature"));
        }

        let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let page_size = u32::from_le_bytes(header[12..16].try_into().unwrap());
        let page_count = u64::from_le_bytes(header[16..24].try_into().unwrap());

        if version != FORMAT_VERSION {
            return Err(invalid("unsupported passql format version"));
        }

        if page_size != PAGE_SIZE as u32 {
            return Err(invalid("unsupported database page size"));
        }

        if page_count == 0 {
            return Err(invalid("invalid database page count"));
        }

        let expected = page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("database size overflow"))?;

        let actual = file.metadata()?.len();

        if actual < expected {
            return Err(invalid("database contains truncated committed pages"));
        }

        // An interrupted append may leave data beyond the committed
        // page count. Discard that uncommitted tail.
        //
        // This assumes exclusive, single-process access.
        if actual > expected {
            file.set_len(expected)?;
            file.sync_all()?;
        }

        Ok(Self { file, page_count })
    }

    pub fn page_count(&self) -> u64 {
        self.page_count
    }

    pub fn append_page(&mut self, data: &[u8]) -> io::Result<u64> {
        if data.len() > PAGE_SIZE {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "data exceeds page size",
            ));
        }

        let next_count = self
            .page_count
            .checked_add(1)
            .ok_or_else(|| invalid("page count overflow"))?;

        let page_id = self.page_count;

        let offset = page_id
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("page offset overflow"))?;

        let mut page = [0u8; PAGE_SIZE];
        page[..data.len()].copy_from_slice(data);

        // Stage the new page on disk.
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&page)?;
        self.file.sync_data()?;

        // Publish the updated page count only after the page
        // has been written and synchronized.
        self.file.seek(SeekFrom::Start(PAGE_COUNT_OFFSET))?;
        self.file.write_all(&next_count.to_le_bytes())?;
        self.file.sync_data()?;

        self.page_count = next_count;

        Ok(page_id)
    }

    pub fn read_page(&mut self, page_id: u64) -> io::Result<[u8; PAGE_SIZE]> {
        if page_id == 0 || page_id >= self.page_count {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "page does not exist or is reserved",
            ));
        }

        let offset = page_id
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("page offset overflow"))?;

        let mut page = [0u8; PAGE_SIZE];

        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut page)?;

        Ok(page)
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

        std::env::temp_dir().join(format!("passql-storage-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn persists_pages_between_sessions() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = Storage::create(&path)?;
            assert_eq!(db.page_count(), 1);
            assert_eq!(db.append_page(b"Hello from Passql")?, 1);
            assert_eq!(db.page_count(), 2);
        }

        {
            let mut db = Storage::open(&path)?;
            let page = db.read_page(1)?;

            assert_eq!(&page[..17], b"Hello from Passql");
            assert_eq!(db.page_count(), 2);
        }

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn rejects_invalid_pages() -> io::Result<()> {
        let path = test_path();
        let mut db = Storage::create(&path)?;

        assert!(db.read_page(0).is_err());
        assert!(db.read_page(1).is_err());
        assert!(db.append_page(&vec![0; PAGE_SIZE + 1]).is_err());
        assert_eq!(db.page_count(), 1);

        drop(db);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn recovers_uncommitted_tail() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = Storage::create(&path)?;
            db.append_page(b"committed")?;
        }

        // Simulate an interrupted append where data was written,
        // but the header page count was not updated.
        {
            let mut file = OpenOptions::new().append(true).open(&path)?;
            file.write_all(&[42u8; PAGE_SIZE])?;
            file.sync_all()?;
        }

        assert_eq!(std::fs::metadata(&path)?.len(), (3 * PAGE_SIZE) as u64);

        {
            let mut db = Storage::open(&path)?;

            assert_eq!(db.page_count(), 2);
            assert_eq!(&db.read_page(1)?[..9], b"committed");
            assert_eq!(std::fs::metadata(&path)?.len(), (2 * PAGE_SIZE) as u64);

            // The recovered page position can be reused.
            assert_eq!(db.append_page(b"new data")?, 2);
        }

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn rejects_truncated_committed_pages() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = Storage::create(&path)?;
            db.append_page(b"important")?;
        }

        {
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(PAGE_SIZE as u64 + 10)?;
        }

        assert!(Storage::open(&path).is_err());

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn rejects_invalid_signature() -> io::Result<()> {
        let path = test_path();
        Storage::create(&path)?;

        {
            let mut file = OpenOptions::new().write(true).open(&path)?;
            file.write_all(b"invalid!")?;
            file.sync_all()?;
        }

        assert!(Storage::open(&path).is_err());

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn refuses_to_overwrite_existing_database() -> io::Result<()> {
        let path = test_path();

        Storage::create(&path)?;
        assert!(Storage::create(&path).is_err());

        std::fs::remove_file(path)?;
        Ok(())
    }
}

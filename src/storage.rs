use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const PAGE_SIZE: usize = 4096;
const MAGIC: &[u8; 8] = b"PASSQL\0\0";
const FORMAT_VERSION: u32 = 1;
const PAGE_COUNT_OFFSET: u64 = 16;

pub struct Storage {
    file: File,
    page_count: u64,
}

impl Storage {
    /// Create a new database without overwriting an existing one.
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;

        let mut header = [0u8; PAGE_SIZE];
        header[0..8].copy_from_slice(MAGIC);
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

    /// Open an existing PassQL database.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;

        let mut header = [0u8; 24];
        file.read_exact(&mut header)?;

        if &header[0..8] != MAGIC {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Invalid PassQL database signature",
            ));
        }

        let version = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let page_size = u32::from_le_bytes(header[12..16].try_into().unwrap());
        let page_count = u64::from_le_bytes(header[16..24].try_into().unwrap());

        if version != FORMAT_VERSION || page_size != PAGE_SIZE as u32 {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Unsupported PassQL database format",
            ));
        }

        let expected_length = page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "Invalid page count"))?;

        if page_count == 0 || file.metadata()?.len() < expected_length {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Truncated or invalid PassQL database",
            ));
        }

        Ok(Self { file, page_count })
    }

    pub fn page_count(&self) -> u64 {
        self.page_count
    }

    /// Append a new page and return its page number.
    pub fn append_page(&mut self, data: &[u8]) -> io::Result<u64> {
        if data.len() > PAGE_SIZE {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "Data exceeds page size",
            ));
        }

        let next_count = self
            .page_count
            .checked_add(1)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "Page count overflow"))?;

        let page_id = self.page_count;
        let offset = page_id
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "Page offset overflow"))?;

        let mut page = [0u8; PAGE_SIZE];
        page[..data.len()].copy_from_slice(data);

        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&page)?;
        self.file.sync_data()?;

        self.file.seek(SeekFrom::Start(PAGE_COUNT_OFFSET))?;
        self.file.write_all(&next_count.to_le_bytes())?;
        self.file.sync_data()?;

        self.page_count = next_count;
        Ok(page_id)
    }

    /// Read a page. Page zero is reserved for database metadata.
    pub fn read_page(&mut self, page_id: u64) -> io::Result<[u8; PAGE_SIZE]> {
        if page_id == 0 || page_id >= self.page_count {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "Page does not exist or is reserved",
            ));
        }

        let offset = page_id
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "Page offset overflow"))?;

        let mut page = [0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut page)?;
        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path() -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!("passql-{}-{unique}.passdb", std::process::id()))
    }

    #[test]
    fn persists_pages_between_sessions() -> io::Result<()> {
        let path = test_path();

        {
            let mut db = Storage::create(&path)?;
            assert_eq!(db.page_count(), 1);
            assert_eq!(db.append_page(b"Hello from PassQL")?, 1);
            assert_eq!(db.page_count(), 2);
        }

        {
            let mut db = Storage::open(&path)?;
            let page = db.read_page(1)?;
            assert_eq!(&page[..17], b"Hello from PassQL");
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
}

use crate::reader::DatabaseReader;
use crate::record::Record;
use crate::storage::PAGE_SIZE;
use crate::transaction::{COMMIT_SIZE, Commit};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

const LEGACY_MAGIC: &[u8; 8] = b"PASSQL\0\0";
const V2_MAGIC: &[u8; 8] = b"passql\0\0";
const V3_MAGIC: &[u8; 8] = b"passql3\0";
const SLOT_OFFSETS: [usize; 2] = [64, 104];
const HEADER_SIZE: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseFormat {
    LegacyV1,
    V2,
    V3,
    V4,
}

impl DatabaseFormat {
    pub fn version(self) -> u32 {
        match self {
            Self::LegacyV1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
            Self::V4 => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseInfo {
    pub format: DatabaseFormat,
    pub page_size: u32,
    pub page_count: u64,
    pub generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationReport {
    pub info: DatabaseInfo,
    pub pages_scanned: u64,
    pub records_verified: u64,
    pub pages_without_record_checksum: u64,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn read_exact(file: &mut File, bytes: &mut [u8], message: &str) -> io::Result<()> {
    file.read_exact(bytes).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            invalid(message)
        } else {
            error
        }
    })
}

fn checked_page_count(page_count: u64, file_size: u64) -> io::Result<()> {
    let required = page_count
        .checked_mul(PAGE_SIZE as u64)
        .ok_or_else(|| invalid("database size overflow"))?;

    if page_count == 0 || required > file_size {
        return Err(invalid(
            "database is truncated or has an invalid page count",
        ));
    }

    Ok(())
}

fn inspect_v3(file: &mut File) -> io::Result<DatabaseInfo> {
    file.try_lock_shared()?;
    let file_size = file.metadata()?.len();

    let mut header = [0u8; PAGE_SIZE];
    file.seek(SeekFrom::Start(0))?;
    read_exact(file, &mut header, "truncated version 3 database header")?;

    if &header[..8] != V3_MAGIC {
        return Err(invalid("invalid version 3 database signature"));
    }

    let version = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let page_size = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);

    if version != 3 {
        return Err(invalid("unsupported version 3 database format"));
    }

    if page_size != PAGE_SIZE as u32 {
        return Err(invalid("unsupported database page size"));
    }

    let mut selected: Option<Commit> = None;

    for offset in SLOT_OFFSETS {
        let bytes: &[u8; COMMIT_SIZE] = header[offset..offset + COMMIT_SIZE]
            .try_into()
            .map_err(|_| invalid("invalid commit slot boundaries"))?;
        let Ok(commit) = Commit::decode(bytes) else {
            continue;
        };
        let Some(required_size) = commit.page_count.checked_mul(PAGE_SIZE as u64) else {
            continue;
        };

        if required_size <= file_size
            && selected
                .as_ref()
                .is_none_or(|current| commit.generation > current.generation)
        {
            selected = Some(commit);
        }
    }

    let commit = selected.ok_or_else(|| invalid("no recoverable version 3 commit"))?;

    Ok(DatabaseInfo {
        format: DatabaseFormat::V3,
        page_size,
        page_count: commit.page_count,
        generation: Some(commit.generation),
    })
}

/// Inspect a Passql database from its binary signature without modifying it.
///
/// The `.psl` extension is not required. Version 1 is recognized for metadata
/// inspection, but its pages are not compatible with the current record API.
pub fn inspect(path: impl AsRef<Path>) -> io::Result<DatabaseInfo> {
    let mut file = File::open(&path)?;
    let file_size = file.metadata()?.len();
    let mut signature = [0u8; 8];
    read_exact(&mut file, &mut signature, "truncated database signature")?;

    if &signature == V3_MAGIC {
        return inspect_v3(&mut file);
    }
    if &signature == b"passql4\0" {
        let reader = crate::storage_v4::IndexReader::open(path)?;
        return Ok(reader.info());
    }

    if &signature != LEGACY_MAGIC && &signature != V2_MAGIC {
        return Err(invalid("unrecognized Passql database signature"));
    }

    let mut rest = [0u8; HEADER_SIZE - 8];
    read_exact(&mut file, &mut rest, "truncated database header")?;
    let version = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let page_size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    let page_count = u64::from_le_bytes([
        rest[8], rest[9], rest[10], rest[11], rest[12], rest[13], rest[14], rest[15],
    ]);

    let format = if version == 1 {
        DatabaseFormat::LegacyV1
    } else if version == 2 && &signature == V2_MAGIC {
        DatabaseFormat::V2
    } else {
        return Err(invalid("unsupported database format version"));
    };

    if page_size != PAGE_SIZE as u32 {
        return Err(invalid("unsupported database page size"));
    }

    checked_page_count(page_count, file_size)?;

    Ok(DatabaseInfo {
        format,
        page_size,
        page_count,
        generation: None,
    })
}

/// Check the metadata and record checksums that the selected format provides.
///
/// Older formats do not checksum every page. Their unchecksummed pages are
/// counted explicitly rather than being reported as fully verified.
pub fn verify(path: impl AsRef<Path>) -> io::Result<VerificationReport> {
    let path = path.as_ref();
    let info = inspect(path)?;

    if info.format == DatabaseFormat::V4 {
        let mut reader = crate::storage_v4::IndexReader::open(path)?;
        let records_verified = reader.verify()?;
        return Ok(VerificationReport {
            info: reader.info(),
            pages_scanned: reader.page_count().saturating_sub(1),
            records_verified,
            pages_without_record_checksum: 0,
        });
    }

    if info.format == DatabaseFormat::V3 {
        let mut reader = DatabaseReader::open(path)?;
        let records_verified = reader.verify()?;
        let info = DatabaseInfo {
            format: DatabaseFormat::V3,
            page_size: PAGE_SIZE as u32,
            page_count: reader.page_count(),
            generation: Some(reader.generation()),
        };

        return Ok(VerificationReport {
            pages_scanned: info.page_count.saturating_sub(1),
            info,
            records_verified,
            pages_without_record_checksum: 0,
        });
    }

    let mut file = File::open(path)?;
    let mut records_verified = 0;
    let mut pages_without_record_checksum = 0;
    let mut page = [0u8; PAGE_SIZE];

    for page_id in 1..info.page_count {
        let offset = page_id
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("page offset overflow"))?;
        file.seek(SeekFrom::Start(offset))?;
        read_exact(&mut file, &mut page, "truncated database page")?;

        if info.format == DatabaseFormat::V2 && Record::is_record_page(&page) {
            Record::decode(&page).map_err(|error| {
                io::Error::new(
                    ErrorKind::InvalidData,
                    format!("corrupt record at page {page_id}: {error}"),
                )
            })?;
            records_verified += 1;
        } else {
            pages_without_record_checksum += 1;
        }
    }

    Ok(VerificationReport {
        info,
        pages_scanned: info.page_count.saturating_sub(1),
        records_verified,
        pages_without_record_checksum,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use crate::storage_v3::StorageV3;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-format-{}-{id}.db", std::process::id()))
    }

    #[test]
    fn inspects_legacy_version_one_metadata() -> io::Result<()> {
        for magic in [LEGACY_MAGIC, V2_MAGIC] {
            let path = test_path();
            let mut header = [0u8; PAGE_SIZE];
            header[..8].copy_from_slice(magic);
            header[8..12].copy_from_slice(&1u32.to_le_bytes());
            header[12..16].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
            header[16..24].copy_from_slice(&1u64.to_le_bytes());
            std::fs::write(&path, header)?;

            let info = inspect(&path)?;
            assert_eq!(info.format, DatabaseFormat::LegacyV1);
            assert_eq!(info.generation, None);
            assert_eq!(info.page_count, 1);

            std::fs::remove_file(path)?;
        }

        Ok(())
    }

    #[test]
    fn inspects_v2_and_v3_without_truncating() -> io::Result<()> {
        let v2_path = test_path();
        let mut v2 = Storage::create(&v2_path)?;
        v2.append_page(b"raw page")?;
        drop(v2);

        assert_eq!(inspect(&v2_path)?.format, DatabaseFormat::V2);

        let v3_path = test_path();
        let v3 = StorageV3::create(&v3_path)?;
        drop(v3);
        {
            let mut file = OpenOptions::new().append(true).open(&v3_path)?;
            file.write_all(&[42u8; PAGE_SIZE])?;
        }

        let before = std::fs::metadata(&v3_path)?.len();
        let info = inspect(&v3_path)?;
        let after = std::fs::metadata(&v3_path)?.len();

        assert_eq!(info.format, DatabaseFormat::V3);
        assert_eq!(info.generation, Some(1));
        assert_eq!(before, after);

        std::fs::remove_file(v2_path)?;
        std::fs::remove_file(v3_path)
    }

    #[test]
    fn verify_reports_pages_without_checksums() -> io::Result<()> {
        let path = test_path();
        let mut db = Storage::create(&path)?;
        db.append_page(b"raw page")?;
        drop(db);

        let report = verify(&path)?;
        assert_eq!(report.pages_scanned, 1);
        assert_eq!(report.records_verified, 0);
        assert_eq!(report.pages_without_record_checksum, 1);

        std::fs::remove_file(path)
    }

    #[test]
    fn verify_checks_v2_records_and_v3_transactions() -> io::Result<()> {
        let v2_path = test_path();
        let mut v2 = Storage::create(&v2_path)?;
        v2.append_record(&Record::new(b"checked"))?;
        drop(v2);

        let report = verify(&v2_path)?;
        assert_eq!(report.records_verified, 1);
        assert_eq!(report.pages_without_record_checksum, 0);

        let v3_path = test_path();
        let mut v3 = StorageV3::create(&v3_path)?;
        let mut tx = v3.begin();
        tx.set("key", b"value")?;
        tx.commit()?;
        drop(v3);

        let report = verify(&v3_path)?;
        assert_eq!(report.info.format, DatabaseFormat::V3);
        assert_eq!(report.info.generation, Some(2));
        assert_eq!(report.records_verified, 1);

        std::fs::remove_file(v2_path)?;
        std::fs::remove_file(v3_path)
    }

    #[test]
    fn verify_detects_corrupt_v2_record_pages() -> io::Result<()> {
        let path = test_path();
        let mut db = Storage::create(&path)?;
        db.append_record(&Record::new(b"checked".to_vec()))?;
        drop(db);

        {
            let mut file = OpenOptions::new().write(true).open(&path)?;
            file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 9))?;
            file.write_all(&[0xff])?;
        }

        assert_eq!(verify(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        std::fs::remove_file(path)
    }

    #[test]
    fn inspects_and_verifies_version_four_indexes() -> io::Result<()> {
        let path = test_path();
        let mut database = crate::storage_v4::StorageV4::create(&path)?;
        database.set("key", b"value")?;
        drop(database);

        let info = inspect(&path)?;
        assert_eq!(info.format, DatabaseFormat::V4);
        assert_eq!(info.generation, Some(2));

        let report = verify(&path)?;
        assert_eq!(report.info, info);
        assert_eq!(report.records_verified, 1);

        std::fs::remove_file(path)
    }

    #[test]
    fn rejects_unknown_signatures() -> io::Result<()> {
        let path = test_path();
        std::fs::write(&path, [0u8; PAGE_SIZE])?;

        assert_eq!(inspect(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        std::fs::remove_file(path)
    }
}

//! Read-only database inspection.
//!
//! `inspect` reads a Passql database's signature and metadata without
//! modifying the file. `verify` walks the live B+ tree and validates every
//! reachable record checksum.

use crate::btree::PAGE_SIZE;
use crate::storage::IndexReader;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::path::Path;

/// Historical signature from Passql v1 (read-only inspection only).
/// Current writers cannot produce v1 files; current readers cannot open them.
const LEGACY_V1_MAGIC_UPPER: &[u8; 8] = b"PASSQL\0\0";
const LEGACY_V1_MAGIC_LOWER: &[u8; 8] = b"passql\0\0";
const V4_MAGIC: &[u8; 8] = b"passql4\0";
const HEADER_SIZE: usize = 24;

/// Passql on-disk formats. Only `V4` is openable for read and write.
/// `LegacyV1` is recognized solely for inspection of historical sample files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseFormat {
    /// Historical v1 file. Inspection reads the header only.
    LegacyV1,
    /// Current on-disk format: copy-on-write B+ tree with checksummed pages.
    V4,
}

impl DatabaseFormat {
    pub fn version(self) -> u32 {
        match self {
            Self::LegacyV1 => 1,
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

/// Inspect a Passql database from its binary signature without modifying it.
///
/// The `.psl` extension is conventional but not required — identification is
/// based on the binary signature at offset 0. Legacy v1 files are recognized
/// for metadata inspection only; their pages cannot be read or written by
/// the current engine.
pub fn inspect(path: impl AsRef<Path>) -> io::Result<DatabaseInfo> {
    let mut file = File::open(&path)?;
    let file_size = file.metadata()?.len();
    let mut signature = [0u8; 8];
    read_exact(&mut file, &mut signature, "truncated database signature")?;

    if &signature == V4_MAGIC {
        let reader = crate::storage::IndexReader::open(path)?;
        return Ok(reader.info());
    }

    if &signature != LEGACY_V1_MAGIC_UPPER && &signature != LEGACY_V1_MAGIC_LOWER {
        return Err(invalid("unrecognized Passql database signature"));
    }

    let mut rest = [0u8; HEADER_SIZE - 8];
    read_exact(&mut file, &mut rest, "truncated database header")?;
    let version = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let page_size = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
    let page_count = u64::from_le_bytes([
        rest[8], rest[9], rest[10], rest[11], rest[12], rest[13], rest[14], rest[15],
    ]);

    if version != 1 {
        return Err(invalid("unsupported database format version"));
    }
    if page_size != PAGE_SIZE as u32 {
        return Err(invalid("unsupported database page size"));
    }
    checked_page_count(page_count, file_size)?;

    Ok(DatabaseInfo {
        format: DatabaseFormat::LegacyV1,
        page_size,
        page_count,
        generation: None,
    })
}

/// Validate the metadata and record checksums of a Passql database.
///
/// For v4 databases this walks the entire B+ tree, validates every page
/// checksum, key order, and record reference, and returns the count of
/// reachable records. For legacy v1 files it returns metadata only — v1
/// pages have no record checksum contract.
pub fn verify(path: impl AsRef<Path>) -> io::Result<VerificationReport> {
    let path = path.as_ref();
    let info = inspect(path)?;

    if info.format == DatabaseFormat::V4 {
        let mut reader = IndexReader::open(path)?;
        let records_verified = reader.verify()?;
        return Ok(VerificationReport {
            info: reader.info(),
            pages_scanned: reader.page_count().saturating_sub(1),
            records_verified,
            pages_without_record_checksum: 0,
        });
    }

    // Legacy v1: no record checksum contract. Report metadata only.
    Ok(VerificationReport {
        info,
        pages_scanned: info.page_count.saturating_sub(1),
        records_verified: 0,
        pages_without_record_checksum: info.page_count.saturating_sub(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-format-{}-{id}.db", std::process::id()))
    }

    #[test]
    fn inspects_legacy_version_one_metadata() -> io::Result<()> {
        for magic in [LEGACY_V1_MAGIC_UPPER, LEGACY_V1_MAGIC_LOWER] {
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
    fn inspects_and_verifies_v4_database() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = Storage::create(&path)?;
            database.set("key", b"value")?;
            drop(database);
        }

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

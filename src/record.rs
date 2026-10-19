use crate::storage::{PAGE_SIZE, Storage};
use std::io::{self, ErrorKind};

const RECORD_TYPE: u8 = 1;
const HEADER_SIZE: usize = 9;
pub const MAX_RECORD_SIZE: usize = PAGE_SIZE - HEADER_SIZE;

pub struct Record {
    pub data: Vec<u8>,
}

impl Record {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self { data: data.into() }
    }

    pub fn encode(&self) -> io::Result<[u8; PAGE_SIZE]> {
        if self.data.len() > MAX_RECORD_SIZE {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "Record exceeds maximum page payload",
            ));
        }

        let mut page = [0u8; PAGE_SIZE];

        page[0] = RECORD_TYPE;
        page[1..5].copy_from_slice(&(self.data.len() as u32).to_le_bytes());
        page[5..9].copy_from_slice(&checksum(&self.data).to_le_bytes());
        page[HEADER_SIZE..HEADER_SIZE + self.data.len()].copy_from_slice(&self.data);

        Ok(page)
    }

    pub fn decode(page: &[u8; PAGE_SIZE]) -> io::Result<Self> {
        if page[0] != RECORD_TYPE {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Unsupported record page type",
            ));
        }

        let length = u32::from_le_bytes(page[1..5].try_into().unwrap()) as usize;

        if length > MAX_RECORD_SIZE {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Invalid record length",
            ));
        }

        let expected = u32::from_le_bytes(page[5..9].try_into().unwrap());

        let data = &page[HEADER_SIZE..HEADER_SIZE + length];

        if checksum(data) != expected {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "Record checksum mismatch",
            ));
        }

        Ok(Self::new(data.to_vec()))
    }
}

/// FNV-1a 32-bit checksum for detecting accidental corruption.
/// This is not a cryptographic integrity mechanism.
fn checksum(data: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;

    for byte in data {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }

    hash
}

impl Storage {
    pub fn append_record(&mut self, record: &Record) -> io::Result<u64> {
        let page = record.encode()?;
        self.append_page(&page)
    }

    pub fn read_record(&mut self, page_id: u64) -> io::Result<Record> {
        let page = self.read_page(page_id)?;
        Record::decode(&page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() -> io::Result<()> {
        let original = Record::new(b"Hello from Passql".to_vec());
        let encoded = original.encode()?;
        let decoded = Record::decode(&encoded)?;

        assert_eq!(original.data, decoded.data);
        Ok(())
    }

    #[test]
    fn preserves_binary_data() -> io::Result<()> {
        let original = Record::new(vec![0, 1, 0, 255, 42]);
        let decoded = Record::decode(&original.encode()?)?;

        assert_eq!(decoded.data, original.data);
        Ok(())
    }

    #[test]
    fn detects_corruption() -> io::Result<()> {
        let record = Record::new(b"important data".to_vec());
        let mut page = record.encode()?;

        page[HEADER_SIZE] ^= 0xff;

        assert!(Record::decode(&page).is_err());
        Ok(())
    }

    #[test]
    fn rejects_oversized_records() {
        let record = Record::new(vec![0; MAX_RECORD_SIZE + 1]);

        assert!(record.encode().is_err());
    }

    #[test]
    fn accepts_maximum_record_size() -> io::Result<()> {
        let record = Record::new(vec![42; MAX_RECORD_SIZE]);
        let decoded = Record::decode(&record.encode()?)?;

        assert_eq!(decoded.data.len(), MAX_RECORD_SIZE);
        Ok(())
    }
}

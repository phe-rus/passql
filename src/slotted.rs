use crate::PAGE_SIZE;
use std::io::{self, ErrorKind};

const MAGIC: &[u8; 4] = b"pspg";
const HEADER_SIZE: usize = 8;
const SLOT_SIZE: usize = 4;

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// A fixed-size page holding variable-length records.
///
/// Layout:
/// [header][slot directory -> ... free space ... <- record data]
pub struct SlottedPage {
    bytes: [u8; PAGE_SIZE],
}

impl Default for SlottedPage {
    fn default() -> Self {
        Self::new()
    }
}

impl SlottedPage {
    pub fn new() -> Self {
        let mut bytes = [0u8; PAGE_SIZE];

        bytes[..4].copy_from_slice(MAGIC);
        bytes[6..8].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());

        Self { bytes }
    }

    pub fn from_bytes(bytes: [u8; PAGE_SIZE]) -> io::Result<Self> {
        if &bytes[..4] != MAGIC {
            return Err(invalid("invalid slotted page signature"));
        }

        let page = Self { bytes };
        let count = page.slot_count();
        let start = page.data_start();

        let directory_end = HEADER_SIZE
            .checked_add(count * SLOT_SIZE)
            .ok_or_else(|| invalid("slot directory overflow"))?;

        if directory_end > start || start > PAGE_SIZE {
            return Err(invalid("invalid slotted page boundaries"));
        }

        // Check that live records stay inside the payload area.
        // Reject overlaps rather than interpreting ambiguous data.
        let mut ranges = Vec::new();

        for id in 0..count {
            let (offset, length) = page.slot(id);

            if length == 0 {
                continue;
            }

            let end = offset
                .checked_add(length)
                .ok_or_else(|| invalid("record offset overflow"))?;

            if offset < start || end > PAGE_SIZE {
                return Err(invalid("record outside page boundaries"));
            }

            ranges.push((offset, end));
        }

        ranges.sort_unstable();

        for pair in ranges.windows(2) {
            if pair[0].1 > pair[1].0 {
                return Err(invalid("overlapping records"));
            }
        }

        Ok(page)
    }

    pub fn slot_count(&self) -> usize {
        u16::from_le_bytes(self.bytes[4..6].try_into().unwrap()) as usize
    }

    fn data_start(&self) -> usize {
        u16::from_le_bytes(self.bytes[6..8].try_into().unwrap()) as usize
    }

    fn slot(&self, id: usize) -> (usize, usize) {
        let start = HEADER_SIZE + id * SLOT_SIZE;

        let offset = u16::from_le_bytes(self.bytes[start..start + 2].try_into().unwrap()) as usize;

        let length =
            u16::from_le_bytes(self.bytes[start + 2..start + 4].try_into().unwrap()) as usize;

        (offset, length)
    }

    pub fn free_space(&self) -> usize {
        let directory_end = HEADER_SIZE + self.slot_count() * SLOT_SIZE;
        self.data_start().saturating_sub(directory_end)
    }

    /// Insert one non-empty record, returning its slot identifier.
    pub fn insert(&mut self, data: &[u8]) -> io::Result<u16> {
        if data.is_empty() {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "empty records are not supported",
            ));
        }

        let required = data
            .len()
            .checked_add(SLOT_SIZE)
            .ok_or_else(|| invalid("record size overflow"))?;

        if required > self.free_space() {
            return Err(io::Error::new(
                ErrorKind::StorageFull,
                "slotted page is full",
            ));
        }

        let id = u16::try_from(self.slot_count())
            .map_err(|_| io::Error::new(ErrorKind::StorageFull, "too many page slots"))?;

        let offset = self.data_start() - data.len();
        let directory = HEADER_SIZE + usize::from(id) * SLOT_SIZE;

        self.bytes[offset..offset + data.len()].copy_from_slice(data);
        self.bytes[directory..directory + 2].copy_from_slice(&(offset as u16).to_le_bytes());
        self.bytes[directory + 2..directory + 4]
            .copy_from_slice(&(data.len() as u16).to_le_bytes());

        self.bytes[4..6].copy_from_slice(&(id + 1).to_le_bytes());
        self.bytes[6..8].copy_from_slice(&(offset as u16).to_le_bytes());

        Ok(id)
    }

    pub fn get(&self, id: u16) -> Option<&[u8]> {
        if usize::from(id) >= self.slot_count() {
            return None;
        }

        let (offset, length) = self.slot(usize::from(id));

        if length == 0 {
            return None;
        }

        Some(&self.bytes[offset..offset + length])
    }

    /// Delete the slot logically. Space is not yet reclaimed.
    pub fn delete(&mut self, id: u16) -> bool {
        if self.get(id).is_none() {
            return false;
        }

        let start = HEADER_SIZE + usize::from(id) * SLOT_SIZE;
        self.bytes[start + 2..start + 4].fill(0);

        true
    }

    pub fn into_bytes(self) -> [u8; PAGE_SIZE] {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_multiple_records() -> io::Result<()> {
        let mut page = SlottedPage::new();

        let a = page.insert(b"Alice")?;
        let b = page.insert(b"Bob")?;
        let c = page.insert(b"Charlie")?;

        assert_eq!(page.get(a), Some(b"Alice".as_slice()));
        assert_eq!(page.get(b), Some(b"Bob".as_slice()));
        assert_eq!(page.get(c), Some(b"Charlie".as_slice()));

        Ok(())
    }

    #[test]
    fn survives_serialization() -> io::Result<()> {
        let mut page = SlottedPage::new();
        page.insert(b"persistent data")?;

        let restored = SlottedPage::from_bytes(page.into_bytes())?;

        assert_eq!(restored.get(0), Some(b"persistent data".as_slice()));
        Ok(())
    }

    #[test]
    fn supports_logical_deletion() -> io::Result<()> {
        let mut page = SlottedPage::new();
        let id = page.insert(b"temporary")?;

        assert!(page.delete(id));
        assert!(page.get(id).is_none());
        assert!(!page.delete(id));

        Ok(())
    }

    #[test]
    fn refuses_oversized_records() {
        let mut page = SlottedPage::new();

        assert!(page.insert(&vec![0; PAGE_SIZE]).is_err());
        assert_eq!(page.slot_count(), 0);
    }

    #[test]
    fn rejects_corrupted_boundaries() {
        let mut page = SlottedPage::new();
        page.insert(b"data").unwrap();

        let mut bytes = page.into_bytes();

        // Make the payload boundary overlap the slot directory.
        bytes[6..8].copy_from_slice(&0u16.to_le_bytes());

        assert!(SlottedPage::from_bytes(bytes).is_err());
    }

    #[test]
    fn rejects_overlapping_records() -> io::Result<()> {
        let mut page = SlottedPage::new();
        page.insert(b"first")?;
        page.insert(b"second")?;

        let mut bytes = page.into_bytes();

        // Point the second slot at the first record's location.
        let (before_second_slot, second_slot_and_after) = bytes.split_at_mut(12);
        second_slot_and_after[..2].copy_from_slice(&before_second_slot[8..10]);

        assert!(SlottedPage::from_bytes(bytes).is_err());
        Ok(())
    }
}

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

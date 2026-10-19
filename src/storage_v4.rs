use crate::btree::{self, Node};
use crate::format::{DatabaseFormat, DatabaseInfo};
use crate::storage::PAGE_SIZE;
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"passql4\0";
const COMMIT_MAGIC: &[u8; 8] = b"pslcmv4\0";
const FORMAT_VERSION: u32 = 4;
const COMMIT_SIZE: usize = 48;
const SLOT_OFFSETS: [u64; 2] = [64, 112];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Commit {
    generation: u64,
    page_count: u64,
    root_page: u64,
}

impl Commit {
    fn new(generation: u64, page_count: u64, root_page: u64) -> io::Result<Self> {
        if generation == 0 || page_count == 0 || root_page >= page_count {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "invalid version 4 commit values",
            ));
        }
        Ok(Self {
            generation,
            page_count,
            root_page,
        })
    }

    fn encode(self) -> [u8; COMMIT_SIZE] {
        let mut bytes = [0u8; COMMIT_SIZE];
        bytes[..8].copy_from_slice(COMMIT_MAGIC);
        bytes[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.generation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.page_count.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.root_page.to_le_bytes());
        let checksum = checksum(&bytes[..40]);
        bytes[40..48].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8; COMMIT_SIZE]) -> io::Result<Self> {
        if &bytes[..8] != COMMIT_MAGIC
            || u32::from_le_bytes(bytes[8..12].try_into().unwrap()) != FORMAT_VERSION
            || bytes[12..16] != [0; 4]
        {
            return Err(invalid("invalid version 4 commit header"));
        }

        let stored = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        if checksum(&bytes[..40]) != stored {
            return Err(invalid("version 4 commit checksum mismatch"));
        }

        let generation = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let page_count = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let root_page = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        Self::new(generation, page_count, root_page)
            .map_err(|_| invalid("invalid version 4 commit values"))
    }

    fn byte_len(self) -> io::Result<u64> {
        self.page_count
            .checked_mul(PAGE_SIZE as u64)
            .ok_or_else(|| invalid("version 4 database size overflow"))
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn read_at(file: &mut File, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(bytes)
}

fn read_header(file: &mut File) -> io::Result<[u8; PAGE_SIZE]> {
    let mut header = [0u8; PAGE_SIZE];
    read_at(file, 0, &mut header).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            invalid("truncated version 4 database header")
        } else {
            error
        }
    })?;

    if &header[..8] != MAGIC {
        return Err(invalid("invalid version 4 database signature"));
    }
    if u32::from_le_bytes(header[8..12].try_into().unwrap()) != FORMAT_VERSION {
        return Err(invalid("unsupported version 4 database format"));
    }
    if u32::from_le_bytes(header[12..16].try_into().unwrap()) != PAGE_SIZE as u32 {
        return Err(invalid("unsupported version 4 page size"));
    }
    Ok(header)
}

fn commit_slots(header: &[u8; PAGE_SIZE]) -> [Option<Commit>; 2] {
    std::array::from_fn(|slot| {
        let start = SLOT_OFFSETS[slot] as usize;
        let bytes: &[u8; COMMIT_SIZE] = header[start..start + COMMIT_SIZE].try_into().ok()?;
        Commit::decode(bytes).ok()
    })
}

fn read_page(file: &mut File, commit: Commit, page_id: u64) -> io::Result<[u8; PAGE_SIZE]> {
    if page_id == 0 || page_id >= commit.page_count {
        return Err(invalid("version 4 page reference is outside the commit"));
    }
    let offset = page_id
        .checked_mul(PAGE_SIZE as u64)
        .ok_or_else(|| invalid("version 4 page offset overflow"))?;
    let mut page = [0u8; PAGE_SIZE];
    read_at(file, offset, &mut page)?;
    Ok(page)
}

fn read_page_staged(
    file: &mut File,
    committed: Commit,
    staged: &[[u8; PAGE_SIZE]],
    page_id: u64,
) -> io::Result<[u8; PAGE_SIZE]> {
    if page_id < committed.page_count {
        return read_page(file, committed, page_id);
    }
    let index = page_id
        .checked_sub(committed.page_count)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| invalid("invalid staged page reference"))?;
    staged
        .get(index)
        .copied()
        .ok_or_else(|| invalid("staged page reference is outside the transaction"))
}

fn validate_tree(file: &mut File, commit: Commit) -> io::Result<u64> {
    if commit.root_page == 0 {
        return Ok(0);
    }

    struct Validator<'a> {
        file: &'a mut File,
        commit: Commit,
        visited: HashSet<u64>,
        records: u64,
    }

    impl Validator<'_> {
        fn visit(
            &mut self,
            page_id: u64,
            lower: Option<&str>,
            upper: Option<&str>,
            depth: usize,
        ) -> io::Result<()> {
            if depth > 128 {
                return Err(invalid("version 4 B+ tree depth exceeds limit"));
            }
            if !self.visited.insert(page_id) {
                return Err(invalid(
                    "duplicate or cyclic version 4 B+ tree page reference",
                ));
            }

            let page = read_page(self.file, self.commit, page_id)?;
            match btree::decode_node(&page)? {
                Node::Leaf(entries) => {
                    for (key, record_id) in entries {
                        if lower.is_some_and(|bound| key.as_str() < bound)
                            || upper.is_some_and(|bound| key.as_str() >= bound)
                        {
                            return Err(invalid("B+ tree leaf key is outside its range"));
                        }
                        let record = read_page(self.file, self.commit, record_id)?;
                        let (stored_key, _) = btree::decode_record(&record)?;
                        if stored_key != key {
                            return Err(invalid("B+ tree key points to a different record"));
                        }
                        self.records = self
                            .records
                            .checked_add(1)
                            .ok_or_else(|| invalid("B+ tree record count overflow"))?;
                    }
                }
                Node::Internal {
                    separators,
                    children,
                } => {
                    for separator in &separators {
                        if lower.is_some_and(|bound| separator.as_str() < bound)
                            || upper.is_some_and(|bound| separator.as_str() >= bound)
                        {
                            return Err(invalid("B+ tree separator is outside its range"));
                        }
                    }
                    for (index, child) in children.into_iter().enumerate() {
                        let child_lower = index
                            .checked_sub(1)
                            .and_then(|previous| separators.get(previous))
                            .map(String::as_str)
                            .or(lower);
                        let child_upper = separators.get(index).map(String::as_str).or(upper);
                        self.visit(child, child_lower, child_upper, depth + 1)?;
                    }
                }
            }
            Ok(())
        }
    }

    let mut validator = Validator {
        file,
        commit,
        visited: HashSet::new(),
        records: 0,
    };
    validator.visit(commit.root_page, None, None, 0)?;
    Ok(validator.records)
}

fn select_commit(file: &mut File) -> io::Result<(Commit, usize)> {
    let header = read_header(file)?;
    let file_size = file.metadata()?.len();
    let slots = commit_slots(&header);
    let mut candidates: Vec<(Commit, usize)> = slots
        .into_iter()
        .enumerate()
        .filter_map(|(slot, commit)| commit.map(|commit| (commit, slot)))
        .filter(|(commit, _)| commit.byte_len().is_ok_and(|length| length <= file_size))
        .collect();
    candidates.sort_by_key(|(commit, slot)| (std::cmp::Reverse(commit.generation), *slot));

    for (commit, slot) in candidates {
        if validate_tree(file, commit).is_ok() {
            return Ok((commit, slot));
        }
    }
    Err(invalid("no recoverable version 4 commit"))
}

fn info(commit: Commit) -> DatabaseInfo {
    DatabaseInfo {
        format: DatabaseFormat::V4,
        page_size: PAGE_SIZE as u32,
        page_count: commit.page_count,
        generation: Some(commit.generation),
    }
}

pub struct StorageV4 {
    file: File,
    commit: Commit,
    slot: usize,
    poisoned: bool,
    #[cfg(test)]
    fail_before_publish: bool,
    #[cfg(test)]
    fail_commit_prefix: Option<usize>,
}

impl StorageV4 {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.try_lock()?;

        let commit = Commit::new(1, 1, 0)?;
        let mut header = [0u8; PAGE_SIZE];
        header[..8].copy_from_slice(MAGIC);
        header[8..12].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        header[12..16].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
        let encoded = commit.encode();
        let start = SLOT_OFFSETS[0] as usize;
        header[start..start + COMMIT_SIZE].copy_from_slice(&encoded);
        file.write_all(&header)?;
        file.sync_all()?;

        Ok(Self {
            file,
            commit,
            slot: 0,
            poisoned: false,
            #[cfg(test)]
            fail_before_publish: false,
            #[cfg(test)]
            fail_commit_prefix: None,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock()?;
        let header = read_header(&mut file)?;
        let file_size = file.metadata()?.len();
        let (commit, slot) = select_commit(&mut file)?;

        let mut invalidated = false;
        for (index, offset) in SLOT_OFFSETS.iter().enumerate() {
            if index == slot {
                continue;
            }
            let start = *offset as usize;
            let raw: &[u8; COMMIT_SIZE] = header[start..start + COMMIT_SIZE]
                .try_into()
                .map_err(|_| invalid("invalid version 4 commit slot"))?;
            let candidate = Commit::decode(raw).ok();
            let stale = match candidate {
                Some(other) => {
                    other.generation > commit.generation
                        || other.page_count > commit.page_count
                        || (other.generation == commit.generation && other != commit)
                }
                None => raw.iter().any(|byte| *byte != 0),
            };
            if stale {
                file.seek(SeekFrom::Start(*offset))?;
                file.write_all(&[0; COMMIT_SIZE])?;
                invalidated = true;
            }
        }
        if invalidated {
            file.sync_data()?;
        }

        let committed_size = commit.byte_len()?;
        if file_size > committed_size {
            file.set_len(committed_size)?;
            file.sync_all()?;
        }

        Ok(Self {
            file,
            commit,
            slot,
            poisoned: false,
            #[cfg(test)]
            fail_before_publish: false,
            #[cfg(test)]
            fail_commit_prefix: None,
        })
    }

    fn ensure_healthy(&self) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other(
                "database handle is poisoned; close and reopen for recovery",
            ));
        }
        Ok(())
    }

    pub fn generation(&self) -> u64 {
        self.commit.generation
    }

    pub fn page_count(&self) -> u64 {
        self.commit.page_count
    }

    pub fn info(&self) -> DatabaseInfo {
        info(self.commit)
    }

    pub fn get(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        self.ensure_healthy()?;
        validate_key(key)?;
        get_value(&mut self.file, self.commit, key)
    }

    pub fn lookup(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        self.get(key)
    }

    pub fn range(
        &mut self,
        start_inclusive: Option<&str>,
        end_exclusive: Option<&str>,
    ) -> io::Result<Vec<(String, Vec<u8>)>> {
        self.ensure_healthy()?;
        validate_range(start_inclusive, end_exclusive)?;
        collect_range(&mut self.file, self.commit, start_inclusive, end_exclusive)
    }

    pub fn verify(&mut self) -> io::Result<u64> {
        self.ensure_healthy()?;
        validate_tree(&mut self.file, self.commit)
    }

    pub fn begin(&mut self) -> io::Result<Transaction<'_>> {
        self.ensure_healthy()?;
        Ok(Transaction {
            database: self,
            mutations: BTreeMap::new(),
        })
    }

    pub fn insert(&mut self, key: &str, value: &[u8]) -> io::Result<u64> {
        let mut transaction = self.begin()?;
        transaction.insert(key, value)?;
        transaction.commit()
    }

    pub fn set(&mut self, key: &str, value: &[u8]) -> io::Result<u64> {
        let mut transaction = self.begin()?;
        transaction.set(key, value)?;
        transaction.commit()
    }

    pub fn delete(&mut self, key: &str) -> io::Result<u64> {
        let mut transaction = self.begin()?;
        transaction.delete(key)?;
        transaction.commit()
    }

    fn publish(&mut self, pages: &[[u8; PAGE_SIZE]], root_page: u64) -> io::Result<u64> {
        if pages.is_empty() {
            return Ok(self.commit.generation);
        }

        let count = self
            .commit
            .page_count
            .checked_add(pages.len() as u64)
            .ok_or_else(|| invalid("version 4 page count overflow"))?;
        let generation = self
            .commit
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("version 4 generation overflow"))?;
        let next = Commit::new(generation, count, root_page)?;
        let offset = self.commit.byte_len()?;

        self.poisoned = true;
        self.file.set_len(offset)?;
        self.file.seek(SeekFrom::Start(offset))?;
        for page in pages {
            self.file.write_all(page)?;
        }
        self.file.sync_data()?;

        #[cfg(test)]
        if self.fail_before_publish {
            self.fail_before_publish = false;
            return Err(io::Error::other(
                "injected failure before commit publication",
            ));
        }

        let next_slot = 1 - self.slot;
        self.file.seek(SeekFrom::Start(SLOT_OFFSETS[next_slot]))?;
        let encoded = next.encode();
        #[cfg(test)]
        if let Some(prefix_len) = self.fail_commit_prefix.take() {
            let prefix_len = prefix_len.min(COMMIT_SIZE);
            self.file.write_all(&encoded[..prefix_len])?;
            return Err(io::Error::other("injected partial commit slot write"));
        }
        self.file.write_all(&encoded)?;
        self.file.sync_data()?;

        self.commit = next;
        self.slot = next_slot;
        self.poisoned = false;
        Ok(generation)
    }
}

pub struct IndexReader {
    file: File,
    commit: Commit,
}

impl IndexReader {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let (commit, _) = select_commit(&mut file)?;
        Ok(Self { file, commit })
    }

    pub fn info(&self) -> DatabaseInfo {
        info(self.commit)
    }

    pub fn generation(&self) -> u64 {
        self.commit.generation
    }

    pub fn page_count(&self) -> u64 {
        self.commit.page_count
    }

    pub fn get(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        validate_key(key)?;
        get_value(&mut self.file, self.commit, key)
    }

    pub fn lookup(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        self.get(key)
    }

    pub fn range(
        &mut self,
        start_inclusive: Option<&str>,
        end_exclusive: Option<&str>,
    ) -> io::Result<Vec<(String, Vec<u8>)>> {
        validate_range(start_inclusive, end_exclusive)?;
        collect_range(&mut self.file, self.commit, start_inclusive, end_exclusive)
    }

    pub fn verify(&mut self) -> io::Result<u64> {
        validate_tree(&mut self.file, self.commit)
    }
}

pub struct Transaction<'a> {
    database: &'a mut StorageV4,
    mutations: BTreeMap<String, Option<Vec<u8>>>,
}

impl Transaction<'_> {
    pub fn get(&mut self, key: &str) -> io::Result<Option<Vec<u8>>> {
        validate_key(key)?;
        if let Some(value) = self.mutations.get(key) {
            return Ok(value.clone());
        }
        get_value(&mut self.database.file, self.database.commit, key)
    }

    pub fn insert(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
        validate_value(key, value)?;
        if self.get(key)?.is_some() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "key already exists",
            ));
        }
        self.mutations.insert(key.to_owned(), Some(value.to_vec()));
        Ok(())
    }

    pub fn set(&mut self, key: &str, value: &[u8]) -> io::Result<()> {
        validate_value(key, value)?;
        self.mutations.insert(key.to_owned(), Some(value.to_vec()));
        Ok(())
    }

    pub fn delete(&mut self, key: &str) -> io::Result<bool> {
        validate_key(key)?;
        let existed = self.get(key)?.is_some();
        if existed {
            self.mutations.insert(key.to_owned(), None);
        }
        Ok(existed)
    }

    pub fn pending(&self) -> usize {
        self.mutations.len()
    }

    pub fn commit(self) -> io::Result<u64> {
        let database = self.database;
        database.ensure_healthy()?;
        let mut pages = Vec::new();
        let mut root_page = database.commit.root_page;

        for (key, value) in self.mutations {
            match value {
                Some(value) => {
                    let previous = get_value_from_staged(
                        &mut database.file,
                        database.commit,
                        &pages,
                        root_page,
                        &key,
                    )?;
                    if previous.as_deref() == Some(value.as_slice()) {
                        continue;
                    }
                    let record_page = btree::encode_record(&key, &value)?;
                    let record_id = stage_page(database.commit, &mut pages, record_page)?;
                    root_page = insert_key(
                        &mut database.file,
                        database.commit,
                        &mut pages,
                        root_page,
                        &key,
                        record_id,
                    )?;
                }
                None => {
                    let (new_root, deleted) = delete_key(
                        &mut database.file,
                        database.commit,
                        &mut pages,
                        root_page,
                        &key,
                    )?;
                    if deleted {
                        root_page = new_root;
                    }
                }
            }
        }

        database.publish(&pages, root_page)
    }

    pub fn rollback(self) {}
}

fn validate_key(key: &str) -> io::Result<()> {
    if !btree::key_is_valid(key) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "key must contain 1 to 1024 bytes",
        ));
    }
    Ok(())
}

fn validate_value(key: &str, value: &[u8]) -> io::Result<()> {
    btree::encode_record(key, value).map(|_| ())
}

fn validate_range(start: Option<&str>, end: Option<&str>) -> io::Result<()> {
    if let Some(start) = start {
        validate_key(start)?;
    }
    if let Some(end) = end {
        validate_key(end)?;
    }
    if matches!((start, end), (Some(start), Some(end)) if start >= end) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "range start must be less than range end",
        ));
    }
    Ok(())
}

fn get_value(file: &mut File, commit: Commit, key: &str) -> io::Result<Option<Vec<u8>>> {
    let mut page_id = commit.root_page;
    let mut visited = HashSet::new();
    for _ in 0..=128 {
        if page_id == 0 {
            return Ok(None);
        }
        if !visited.insert(page_id) {
            return Err(invalid("cyclic B+ tree reference"));
        }
        match btree::decode_node(&read_page(file, commit, page_id)?)? {
            Node::Leaf(entries) => {
                let found = entries.binary_search_by(|(entry, _)| entry.as_str().cmp(key));
                let Ok(index) = found else {
                    return Ok(None);
                };
                let record_id = entries[index].1;
                let page = read_page(file, commit, record_id)?;
                let (stored_key, value) = btree::decode_record(&page)?;
                if stored_key != key {
                    return Err(invalid("B+ tree key points to a different record"));
                }
                return Ok(Some(value));
            }
            Node::Internal {
                separators,
                children,
            } => {
                let child = separators.partition_point(|separator| separator.as_str() <= key);
                page_id = *children
                    .get(child)
                    .ok_or_else(|| invalid("B+ tree child index is outside the node"))?;
            }
        }
    }
    Err(invalid("version 4 B+ tree depth exceeds limit"))
}

fn get_value_from_staged(
    file: &mut File,
    commit: Commit,
    staged: &[[u8; PAGE_SIZE]],
    root_page: u64,
    key: &str,
) -> io::Result<Option<Vec<u8>>> {
    let mut page_id = root_page;
    let mut visited = HashSet::new();
    for _ in 0..=128 {
        if page_id == 0 {
            return Ok(None);
        }
        if !visited.insert(page_id) {
            return Err(invalid("cyclic B+ tree reference"));
        }
        match btree::decode_node(&read_page_staged(file, commit, staged, page_id)?)? {
            Node::Leaf(entries) => {
                let found = entries.binary_search_by(|(entry, _)| entry.as_str().cmp(key));
                let Ok(index) = found else {
                    return Ok(None);
                };
                let record = read_page_staged(file, commit, staged, entries[index].1)?;
                let (stored_key, value) = btree::decode_record(&record)?;
                if stored_key != key {
                    return Err(invalid("B+ tree key points to a different record"));
                }
                return Ok(Some(value));
            }
            Node::Internal {
                separators,
                children,
            } => {
                let child = separators.partition_point(|separator| separator.as_str() <= key);
                page_id = *children
                    .get(child)
                    .ok_or_else(|| invalid("B+ tree child index is outside the node"))?;
            }
        }
    }
    Err(invalid("version 4 B+ tree depth exceeds limit"))
}

fn stage_page(
    commit: Commit,
    pages: &mut Vec<[u8; PAGE_SIZE]>,
    page: [u8; PAGE_SIZE],
) -> io::Result<u64> {
    let id = commit
        .page_count
        .checked_add(pages.len() as u64)
        .ok_or_else(|| invalid("version 4 page identifier overflow"))?;
    pages.push(page);
    Ok(id)
}

fn append_node(commit: Commit, pages: &mut Vec<[u8; PAGE_SIZE]>, node: Node) -> io::Result<u64> {
    stage_page(commit, pages, btree::encode_node(&node)?)
}

fn insert_key(
    file: &mut File,
    commit: Commit,
    staged: &mut Vec<[u8; PAGE_SIZE]>,
    root_page: u64,
    key: &str,
    record_id: u64,
) -> io::Result<u64> {
    if root_page == 0 {
        return append_node(
            commit,
            staged,
            Node::Leaf(vec![(key.to_owned(), record_id)]),
        );
    }

    let (left, split) = insert_node(file, commit, staged, root_page, key, record_id)?;
    match split {
        Some((separator, right)) => append_node(
            commit,
            staged,
            Node::Internal {
                separators: vec![separator],
                children: vec![left, right],
            },
        ),
        None => Ok(left),
    }
}

fn insert_node(
    file: &mut File,
    commit: Commit,
    staged: &mut Vec<[u8; PAGE_SIZE]>,
    page_id: u64,
    key: &str,
    record_id: u64,
) -> io::Result<(u64, Option<(String, u64)>)> {
    match btree::decode_node(&read_page_staged(file, commit, staged, page_id)?)? {
        Node::Leaf(mut entries) => {
            match entries.binary_search_by(|(entry, _)| entry.as_str().cmp(key)) {
                Ok(index) => entries[index].1 = record_id,
                Err(index) => entries.insert(index, (key.to_owned(), record_id)),
            }
            persist_leaf(commit, staged, entries)
        }
        Node::Internal {
            mut separators,
            mut children,
        } => {
            let child_index = separators.partition_point(|separator| separator.as_str() <= key);
            let child_page = *children
                .get(child_index)
                .ok_or_else(|| invalid("B+ tree child index is outside the node"))?;
            let (new_child, split) = insert_node(file, commit, staged, child_page, key, record_id)?;
            children[child_index] = new_child;
            if let Some((separator, new_right)) = split {
                separators.insert(child_index, separator);
                children.insert(child_index + 1, new_right);
            }
            persist_internal(commit, staged, separators, children)
        }
    }
}

fn persist_leaf(
    commit: Commit,
    staged: &mut Vec<[u8; PAGE_SIZE]>,
    entries: Vec<(String, u64)>,
) -> io::Result<(u64, Option<(String, u64)>)> {
    let node = Node::Leaf(entries.clone());
    if let Ok(page) = btree::encode_node(&node) {
        return Ok((stage_page(commit, staged, page)?, None));
    }
    if entries.len() < 2 {
        return Err(io::Error::new(
            ErrorKind::StorageFull,
            "a B+ tree entry does not fit on a page",
        ));
    }

    let middle = entries.len() / 2;
    let left = entries[..middle].to_vec();
    let right = entries[middle..].to_vec();
    let separator = right[0].0.clone();
    let left_page = append_node(commit, staged, Node::Leaf(left))?;
    let right_page = append_node(commit, staged, Node::Leaf(right))?;
    Ok((left_page, Some((separator, right_page))))
}

fn persist_internal(
    commit: Commit,
    staged: &mut Vec<[u8; PAGE_SIZE]>,
    separators: Vec<String>,
    children: Vec<u64>,
) -> io::Result<(u64, Option<(String, u64)>)> {
    let node = Node::Internal {
        separators: separators.clone(),
        children: children.clone(),
    };
    if let Ok(page) = btree::encode_node(&node) {
        return Ok((stage_page(commit, staged, page)?, None));
    }
    if separators.len() < 2 {
        return Err(io::Error::new(
            ErrorKind::StorageFull,
            "B+ tree internal node does not fit on a page",
        ));
    }

    let middle = separators.len() / 2;
    let promoted = separators[middle].clone();
    let left = Node::Internal {
        separators: separators[..middle].to_vec(),
        children: children[..middle + 1].to_vec(),
    };
    let right = Node::Internal {
        separators: separators[middle + 1..].to_vec(),
        children: children[middle + 1..].to_vec(),
    };
    let left_page = append_node(commit, staged, left)?;
    let right_page = append_node(commit, staged, right)?;
    Ok((left_page, Some((promoted, right_page))))
}

fn delete_key(
    file: &mut File,
    commit: Commit,
    staged: &mut Vec<[u8; PAGE_SIZE]>,
    root_page: u64,
    key: &str,
) -> io::Result<(u64, bool)> {
    if root_page == 0 {
        return Ok((0, false));
    }
    match delete_node(file, commit, staged, root_page, key)? {
        (_, false) => Ok((root_page, false)),
        (page_id, true) => {
            if page_id == 0 {
                Ok((0, true))
            } else {
                match btree::decode_node(&read_page_staged(file, commit, staged, page_id)?)? {
                    Node::Leaf(entries) if entries.is_empty() => Ok((0, true)),
                    _ => Ok((page_id, true)),
                }
            }
        }
    }
}

fn delete_node(
    file: &mut File,
    commit: Commit,
    staged: &mut Vec<[u8; PAGE_SIZE]>,
    page_id: u64,
    key: &str,
) -> io::Result<(u64, bool)> {
    match btree::decode_node(&read_page_staged(file, commit, staged, page_id)?)? {
        Node::Leaf(mut entries) => {
            let Ok(index) = entries.binary_search_by(|(entry, _)| entry.as_str().cmp(key)) else {
                return Ok((page_id, false));
            };
            entries.remove(index);
            if entries.is_empty() {
                return Ok((append_node(commit, staged, Node::Leaf(entries))?, true));
            }
            Ok((append_node(commit, staged, Node::Leaf(entries))?, true))
        }
        Node::Internal {
            separators,
            mut children,
        } => {
            let child_index = separators.partition_point(|separator| separator.as_str() <= key);
            let child_page = *children
                .get(child_index)
                .ok_or_else(|| invalid("B+ tree child index is outside the node"))?;
            let (new_child, deleted) = delete_node(file, commit, staged, child_page, key)?;
            if !deleted {
                return Ok((page_id, false));
            }
            if new_child == 0 {
                return Err(invalid("B+ tree internal node lost a child"));
            }
            children[child_index] = new_child;
            Ok((
                append_node(
                    commit,
                    staged,
                    Node::Internal {
                        separators,
                        children,
                    },
                )?,
                true,
            ))
        }
    }
}

fn collect_range(
    file: &mut File,
    commit: Commit,
    start: Option<&str>,
    end: Option<&str>,
) -> io::Result<Vec<(String, Vec<u8>)>> {
    struct RangeWalker<'file, 'range> {
        file: &'file mut File,
        commit: Commit,
        start: Option<&'range str>,
        end: Option<&'range str>,
        output: Vec<(String, Vec<u8>)>,
        visited: HashSet<u64>,
    }

    impl RangeWalker<'_, '_> {
        fn visit(
            &mut self,
            page_id: u64,
            lower: Option<&str>,
            upper: Option<&str>,
            depth: usize,
        ) -> io::Result<()> {
            if depth > 128 {
                return Err(invalid("version 4 B+ tree depth exceeds limit"));
            }
            if !self.visited.insert(page_id) {
                return Err(invalid(
                    "duplicate or cyclic version 4 B+ tree page reference",
                ));
            }
            if upper.is_some_and(|bound| self.start.is_some_and(|wanted| bound <= wanted))
                || lower.is_some_and(|bound| self.end.is_some_and(|wanted| bound >= wanted))
            {
                return Ok(());
            }

            match btree::decode_node(&read_page(self.file, self.commit, page_id)?)? {
                Node::Leaf(entries) => {
                    for (key, record_id) in entries {
                        if self.start.is_some_and(|bound| key.as_str() < bound)
                            || self.end.is_some_and(|bound| key.as_str() >= bound)
                        {
                            continue;
                        }
                        let (stored_key, value) =
                            btree::decode_record(&read_page(self.file, self.commit, record_id)?)?;
                        if stored_key != key {
                            return Err(invalid("B+ tree key points to a different record"));
                        }
                        self.output.push((key, value));
                    }
                }
                Node::Internal {
                    separators,
                    children,
                } => {
                    for (index, child) in children.into_iter().enumerate() {
                        let child_lower = index
                            .checked_sub(1)
                            .and_then(|previous| separators.get(previous))
                            .map(String::as_str)
                            .or(lower);
                        let child_upper = separators.get(index).map(String::as_str).or(upper);
                        self.visit(child, child_lower, child_upper, depth + 1)?;
                    }
                }
            }
            Ok(())
        }
    }

    let mut walker = RangeWalker {
        file,
        commit,
        start,
        end,
        output: Vec::new(),
        visited: HashSet::new(),
    };
    if commit.root_page != 0 {
        walker.visit(commit.root_page, None, None, 0)?;
    }
    Ok(walker.output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-v4-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn transactions_persist_ordered_index_and_range() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = StorageV4::create(&path)?;
            let mut transaction = database.begin()?;
            for key in ["m", "a", "z", "b", "x", "c", "y", "d"] {
                transaction.set(key, key.as_bytes())?;
            }
            assert_eq!(transaction.get("x")?, Some(b"x".to_vec()));
            assert_eq!(transaction.commit()?, 2);
            assert_eq!(database.get("m")?, Some(b"m".to_vec()));
            assert_eq!(
                database
                    .range(Some("b"), Some("y"))?
                    .iter()
                    .map(|(key, _)| key.as_str())
                    .collect::<Vec<_>>(),
                ["b", "c", "d", "m", "x"]
            );
            assert_eq!(database.verify()?, 8);
        }
        {
            let mut reader = IndexReader::open(&path)?;
            assert_eq!(reader.get("a")?, Some(b"a".to_vec()));
            assert_eq!(reader.get("missing")?, None);
            assert_eq!(reader.verify()?, 8);
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn dropped_and_rolled_back_transactions_publish_nothing() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = StorageV4::create(&path)?;
            let initial_generation = database.generation();
            {
                let mut transaction = database.begin()?;
                transaction.set("dropped", b"value")?;
            }
            {
                let mut transaction = database.begin()?;
                transaction.set("rolled-back", b"value")?;
                transaction.rollback();
            }
            assert_eq!(database.generation(), initial_generation);
            assert_eq!(database.get("dropped")?, None);
            assert_eq!(database.get("rolled-back")?, None);
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn root_grows_after_leaf_splits_and_reopens() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = StorageV4::create(&path)?;
            for index in 0..700 {
                database.set(&format!("key:{index:04}"), b"value")?;
            }
            assert_eq!(database.verify()?, 700);
            assert_eq!(database.get("key:0699")?, Some(b"value".to_vec()));
        }
        {
            let mut reader = IndexReader::open(&path)?;
            assert_eq!(reader.get("key:0000")?, Some(b"value".to_vec()));
            assert_eq!(reader.range(None, None)?.len(), 700);
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn splits_large_keys_without_exceeding_page_capacity() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = StorageV4::create(&path)?;
            let mut transaction = database.begin()?;
            for index in 0..20 {
                let key = format!("{index:02}{}", "k".repeat(1000));
                transaction.set(&key, b"value")?;
            }
            transaction.commit()?;
            assert_eq!(database.verify()?, 20);
        }
        {
            let mut reader = IndexReader::open(&path)?;
            assert_eq!(reader.range(None, None)?.len(), 20);
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn updates_deletes_and_insert_conflicts_are_transactional() -> io::Result<()> {
        let path = test_path();
        let mut database = StorageV4::create(&path)?;
        database.insert("name", b"old")?;
        assert_eq!(
            database.insert("name", b"duplicate").unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );

        let mut transaction = database.begin()?;
        transaction.set("name", b"new")?;
        assert_eq!(transaction.get("name")?, Some(b"new".to_vec()));
        assert!(transaction.delete("name")?);
        assert_eq!(transaction.get("name")?, None);
        transaction.set("name", b"final")?;
        transaction.set("second", b"value")?;
        transaction.commit()?;
        assert_eq!(database.get("name")?, Some(b"final".to_vec()));
        assert_eq!(database.get("second")?, Some(b"value".to_vec()));
        assert!(database.delete("name")? > 0);
        assert_eq!(database.get("name")?, None);
        assert!(!database.begin()?.delete("missing")?);
        drop(database);

        let mut reader = IndexReader::open(&path)?;
        assert_eq!(reader.get("name")?, None);
        assert_eq!(reader.get("second")?, Some(b"value".to_vec()));
        std::fs::remove_file(path)
    }

    #[test]
    fn failed_prepublication_write_recovers_previous_generation() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = StorageV4::create(&path)?;
            database.set("stable", b"old")?;
            database.fail_before_publish = true;
            let error = database.set("partial", b"uncommitted").unwrap_err();
            assert!(error.to_string().contains("injected failure"));
            assert!(database.get("stable").is_err());
        }
        {
            let mut database = StorageV4::open(&path)?;
            assert_eq!(database.generation(), 2);
            assert_eq!(database.get("stable")?, Some(b"old".to_vec()));
            assert_eq!(database.get("partial")?, None);
            assert_eq!(
                std::fs::metadata(&path)?.len(),
                database.page_count() * PAGE_SIZE as u64
            );
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn partial_commit_slot_is_cleared_before_reusing_its_pages() -> io::Result<()> {
        let path = test_path();
        {
            let mut database = StorageV4::create(&path)?;
            database.set("stable", b"old")?;
            database.fail_commit_prefix = Some(24);
            assert!(database.set("partial", b"uncertain").is_err());
        }
        {
            let mut database = StorageV4::open(&path)?;
            assert_eq!(database.generation(), 2);
            assert_eq!(database.get("partial")?, None);
            database.set("fresh", b"committed")?;
        }
        {
            let mut reader = IndexReader::open(&path)?;
            assert_eq!(reader.generation(), 3);
            assert_eq!(reader.get("fresh")?, Some(b"committed".to_vec()));
            assert_eq!(reader.get("partial")?, None);
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn corrupt_latest_record_falls_back_and_clears_stale_commit() -> io::Result<()> {
        let path = test_path();
        let older_page_count;
        {
            let mut database = StorageV4::create(&path)?;
            database.set("key", b"old")?;
            older_page_count = database.page_count();
            database.set("key", b"new")?;
        }

        {
            let mut file = OpenOptions::new().write(true).open(&path)?;
            let mut reader = IndexReader::open(&path)?;
            let info = reader.info();
            assert_eq!(info.generation, Some(3));
            let latest = reader.get("key")?;
            assert_eq!(latest, Some(b"new".to_vec()));
            drop(reader);
            file.seek(SeekFrom::Start(
                (info.page_count - 1) * PAGE_SIZE as u64 + 16 + 6,
            ))?;
            file.write_all(&[0xff])?;
        }

        {
            let mut database = StorageV4::open(&path)?;
            assert_eq!(database.generation(), 2);
            assert_eq!(database.get("key")?, Some(b"old".to_vec()));
            assert_eq!(database.page_count(), older_page_count);
        }
        std::fs::remove_file(path)
    }

    #[test]
    fn snapshot_reader_does_not_block_later_writer() -> io::Result<()> {
        let path = test_path();
        let mut database = StorageV4::create(&path)?;
        database.set("key", b"first")?;
        let mut snapshot = IndexReader::open(&path)?;
        database.set("key", b"second")?;

        assert_eq!(snapshot.get("key")?, Some(b"first".to_vec()));
        assert_eq!(database.get("key")?, Some(b"second".to_vec()));
        drop(snapshot);
        drop(database);
        std::fs::remove_file(path)
    }

    #[test]
    fn rejects_invalid_keys_and_ranges() -> io::Result<()> {
        let path = test_path();
        let mut database = StorageV4::create(&path)?;
        assert!(database.set("", b"value").is_err());
        assert!(database.set(&"k".repeat(1025), b"value").is_err());
        assert!(database.range(Some("z"), Some("a")).is_err());
        assert!(
            database
                .set("key", &vec![0; btree::MAX_VALUE_SIZE + 1])
                .is_err()
        );
        drop(database);
        std::fs::remove_file(path)
    }

    #[test]
    fn prevents_multiple_writers() -> io::Result<()> {
        let path = test_path();
        let writer = StorageV4::create(&path)?;
        assert!(StorageV4::open(&path).is_err());
        drop(writer);
        assert!(StorageV4::open(&path).is_ok());
        std::fs::remove_file(path)
    }

    #[test]
    fn operations_match_a_sorted_reference_model() -> io::Result<()> {
        let path = test_path();
        let mut expected = BTreeMap::new();
        let expected_rows;
        {
            let mut database = StorageV4::create(&path)?;
            for batch in 0..20 {
                let mut transaction = database.begin()?;
                for offset in 0..10 {
                    let number = (batch * 17 + offset * 13) % 101;
                    let key = format!("k:{number:03}");
                    if (batch + offset) % 4 == 0 {
                        transaction.delete(&key)?;
                        expected.remove(&key);
                    } else {
                        let value = format!("v:{batch}:{offset}").into_bytes();
                        transaction.set(&key, &value)?;
                        expected.insert(key, value);
                    }
                }
                transaction.commit()?;
            }

            expected_rows = expected.into_iter().collect::<Vec<_>>();
            let observed = database.range(None, None)?;
            assert_eq!(observed, expected_rows);
        }
        let mut reader = IndexReader::open(&path)?;
        assert_eq!(reader.range(None, None)?, expected_rows);
        std::fs::remove_file(path)
    }
}

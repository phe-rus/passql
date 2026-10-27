//! Document collection operations.
//!
//! A collection is a set of JSON documents stored under the key prefix
//! `c:<collection_name>:<id>` where `<id>` is a big-endian u64. The
//! collection's metadata (next id counter) lives at `__coll__:<name>`.

use crate::Storage;
use crate::document::filter::Filter;
use crate::document::path::Path;
use serde_json::Value;
use std::io;

/// Reserved key prefix for collection catalog entries.
const COLL_PREFIX: &str = "__coll__:";
/// Reserved key prefix for document data.
const DOC_PREFIX: &str = "c:";

/// A document id is a u64 stored in big-endian so that lexicographic key
/// ordering matches numeric ordering.
pub type DocumentId = u64;

/// Result of a document operation.
pub enum DocumentResult {
    Inserted { id: DocumentId },
    Found { id: DocumentId, document: Value },
    Updated { id: DocumentId, document: Value },
    Deleted { id: DocumentId, existed: bool },
    Find { matches: Vec<(DocumentId, Value)> },
}

/// Handle to a document collection. Borrows the underlying `Storage`.
pub struct Collection<'a> {
    name: String,
    storage: &'a mut Storage,
}

impl<'a> Collection<'a> {
    /// Open a collection by name. If the collection does not exist, it is
    /// created implicitly on the first insert.
    pub fn open(storage: &'a mut Storage, name: &str) -> Result<Self, CollectionError> {
        validate_name(name)?;
        Ok(Self {
            name: name.to_owned(),
            storage,
        })
    }

    /// Create the collection explicitly. Returns `AlreadyExists` if a
    /// collection with this name already exists.
    pub fn create(storage: &'a mut Storage, name: &str) -> Result<Self, CollectionError> {
        validate_name(name)?;
        let catalog_key = format!("{COLL_PREFIX}{name}");
        let mut tx = storage.begin()?;
        if tx.get(&catalog_key)?.is_some() {
            return Err(CollectionError::AlreadyExists(name.to_owned()));
        }
        // Initial counter value: 0. First document gets id 1.
        tx.set(&catalog_key, &0u64.to_le_bytes())?;
        tx.commit()?;
        Ok(Self {
            name: name.to_owned(),
            storage,
        })
    }

    /// Drop the collection and all its documents.
    pub fn drop_collection(&mut self) -> io::Result<bool> {
        let catalog_key = format!("{COLL_PREFIX}{}", self.name);
        if self.storage.get(&catalog_key)?.is_none() {
            return Ok(false);
        }
        let start = doc_start_key(&self.name);
        let end = doc_end_key(&self.name);
        let rows = self.storage.range(Some(&start), Some(&end))?;
        let mut tx = self.storage.begin()?;
        tx.delete(&catalog_key)?;
        for (key, _) in rows {
            tx.delete(&key)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Insert a new document. Auto-assigns the next id.
    pub fn insert(&mut self, document: &Value) -> Result<DocumentId, CollectionError> {
        let catalog_key = format!("{COLL_PREFIX}{}", self.name);
        let mut next_id: u64 = match self.storage.get(&catalog_key)? {
            Some(bytes) => {
                let arr: [u8; 8] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| CollectionError::CorruptCounter)?;
                u64::from_le_bytes(arr)
            }
            None => 0,
        };
        next_id = next_id.checked_add(1).ok_or(CollectionError::IdOverflow)?;

        let key = doc_key(&self.name, next_id);
        let payload = serde_json::to_vec(document)?;
        let mut tx = self.storage.begin()?;
        tx.set(&key, &payload)?;
        tx.set(&catalog_key, &next_id.to_le_bytes())?;
        tx.commit()?;
        Ok(next_id)
    }

    /// Fetch a document by id.
    pub fn get(&mut self, id: DocumentId) -> Result<Option<Value>, CollectionError> {
        let key = doc_key(&self.name, id);
        match self.storage.get(&key)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Replace a document entirely.
    pub fn replace(&mut self, id: DocumentId, document: &Value) -> Result<bool, CollectionError> {
        let key = doc_key(&self.name, id);
        if self.storage.get(&key)?.is_none() {
            return Ok(false);
        }
        let payload = serde_json::to_vec(document)?;
        let mut tx = self.storage.begin()?;
        tx.set(&key, &payload)?;
        tx.commit()?;
        Ok(true)
    }

    /// Patch a document by merging fields at the given paths.
    pub fn patch(
        &mut self,
        id: DocumentId,
        patches: &[(Path, Value)],
    ) -> Result<Option<Value>, CollectionError> {
        let key = doc_key(&self.name, id);
        let existing = match self.storage.get(&key)? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => return Ok(None),
        };
        let mut current = existing;
        for (path, value) in patches {
            current = path.set(current, value.clone());
        }
        let payload = serde_json::to_vec(&current)?;
        let mut tx = self.storage.begin()?;
        tx.set(&key, &payload)?;
        tx.commit()?;
        Ok(Some(current))
    }

    /// Delete a document by id.
    pub fn delete(&mut self, id: DocumentId) -> io::Result<bool> {
        let key = doc_key(&self.name, id);
        if self.storage.get(&key)?.is_none() {
            return Ok(false);
        }
        let mut tx = self.storage.begin()?;
        tx.delete(&key)?;
        tx.commit()?;
        Ok(true)
    }

    /// Find all documents matching a filter. Returns (id, document) pairs.
    pub fn find(&mut self, filter: &Filter) -> Result<Vec<(DocumentId, Value)>, CollectionError> {
        let start = doc_start_key(&self.name);
        let end = doc_end_key(&self.name);
        let entries = self.storage.range(Some(&start), Some(&end))?;
        let mut out = Vec::new();
        for (key, payload) in entries {
            let id = match parse_doc_id(&self.name, &key) {
                Some(id) => id,
                None => continue,
            };
            let document: Value = serde_json::from_slice(&payload)?;
            if filter.matches(&document) {
                out.push((id, document));
            }
        }
        Ok(out)
    }

    /// Iterate all documents in the collection (no filter).
    pub fn list(&mut self) -> Result<Vec<(DocumentId, Value)>, CollectionError> {
        self.find(&Filter::All)
    }

    /// Update documents matching a filter by applying patches.
    pub fn update_where(
        &mut self,
        filter: &Filter,
        patches: &[(Path, Value)],
    ) -> Result<u64, CollectionError> {
        let start = doc_start_key(&self.name);
        let end = doc_end_key(&self.name);
        let entries = self.storage.range(Some(&start), Some(&end))?;
        let mut tx = self.storage.begin()?;
        let mut updated = 0u64;
        for (key, payload) in entries {
            let mut document: Value = serde_json::from_slice(&payload)?;
            if !filter.matches(&document) {
                continue;
            }
            for (path, value) in patches {
                document = path.set(document, value.clone());
            }
            let new_payload = serde_json::to_vec(&document)?;
            tx.set(&key, &new_payload)?;
            updated += 1;
        }
        tx.commit()?;
        Ok(updated)
    }

    /// Delete documents matching a filter.
    pub fn delete_where(&mut self, filter: &Filter) -> Result<u64, CollectionError> {
        let start = doc_start_key(&self.name);
        let end = doc_end_key(&self.name);
        let entries = self.storage.range(Some(&start), Some(&end))?;
        let mut tx = self.storage.begin()?;
        let mut deleted = 0u64;
        for (key, payload) in entries {
            let document: Value = serde_json::from_slice(&payload)?;
            if !filter.matches(&document) {
                continue;
            }
            tx.delete(&key)?;
            deleted += 1;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// Count documents in the collection.
    pub fn count(&mut self) -> io::Result<u64> {
        let start = doc_start_key(&self.name);
        let end = doc_end_key(&self.name);
        Ok(self.storage.range(Some(&start), Some(&end))?.len() as u64)
    }
}

fn validate_name(name: &str) -> Result<(), CollectionError> {
    if name.is_empty() || name.len() > 64 {
        return Err(CollectionError::InvalidName(name.to_owned()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CollectionError::InvalidName(name.to_owned()));
    }
    Ok(())
}

fn doc_key(collection: &str, id: DocumentId) -> String {
    format!("{DOC_PREFIX}{collection}:{id:020x}")
}

fn doc_start_key(collection: &str) -> String {
    format!("{DOC_PREFIX}{collection}:")
}

fn doc_end_key(collection: &str) -> String {
    format!("{DOC_PREFIX}{collection};")
}

fn parse_doc_id(collection: &str, key: &str) -> Option<DocumentId> {
    let prefix = format!("{DOC_PREFIX}{collection}:");
    let rest = key.strip_prefix(&prefix)?;
    u64::from_str_radix(rest, 16).ok()
}

#[derive(Debug)]
pub enum CollectionError {
    Io(io::Error),
    Serde(serde_json::Error),
    InvalidName(String),
    AlreadyExists(String),
    CorruptCounter,
    IdOverflow,
}

impl From<io::Error> for CollectionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CollectionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serde(error)
    }
}

impl std::fmt::Display for CollectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::Serde(error) => write!(f, "json error: {error}"),
            Self::InvalidName(name) => write!(
                f,
                "invalid collection name '{name}' (must be 1-64 chars, [A-Za-z0-9_-])"
            ),
            Self::AlreadyExists(name) => write!(f, "collection '{name}' already exists"),
            Self::CorruptCounter => write!(f, "corrupt collection counter"),
            Self::IdOverflow => write!(f, "document id overflow"),
        }
    }
}

impl std::error::Error for CollectionError {}

impl From<CollectionError> for io::Error {
    fn from(error: CollectionError) -> Self {
        match error {
            CollectionError::Io(error) => error,
            other => io::Error::other(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-doc-{}-{id}.psl", std::process::id()))
    }

    #[test]
    fn insert_get_replace_delete() -> Result<(), CollectionError> {
        let path = test_path();
        let mut storage = Storage::create(&path)?;
        let mut coll = Collection::create(&mut storage, "users")?;

        let alice = json!({"name": "Alice", "age": 30});
        let id = coll.insert(&alice)?;
        assert_eq!(id, 1);

        let bob = json!({"name": "Bob", "age": 25});
        let id2 = coll.insert(&bob)?;
        assert_eq!(id2, 2);

        let got = coll.get(id)?;
        assert_eq!(got, Some(alice));

        let replaced = json!({"name": "Alice", "age": 31});
        assert!(coll.replace(id, &replaced)?);
        let got = coll.get(id)?;
        assert_eq!(got, Some(replaced));

        assert!(coll.delete(id2)?);
        assert_eq!(coll.get(id2)?, None);
        assert!(!coll.delete(id2)?);

        assert_eq!(coll.count()?, 1);
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn patch_and_find() -> Result<(), CollectionError> {
        let path = test_path();
        let mut storage = Storage::create(&path)?;
        let mut coll = Collection::create(&mut storage, "people")?;

        coll.insert(&json!({"name": "Alice", "age": 30, "city": "Nairobi"}))?;
        coll.insert(&json!({"name": "Bob", "age": 25, "city": "Lagos"}))?;
        coll.insert(&json!({"name": "Carol", "age": 35, "city": "Nairobi"}))?;

        // Patch one document.
        let patched = coll.patch(
            1,
            &[
                (Path::parse("age"), json!(31)),
                (Path::parse("city"), json!("Kampala")),
            ],
        )?;
        assert_eq!(
            patched,
            Some(json!({"name": "Alice", "age": 31, "city": "Kampala"}))
        );

        // Find by city.
        let filter = Filter::parse(&json!({"city": "Nairobi"})).unwrap();
        let matches = coll.find(&filter)?;
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].1["name"], json!("Carol"));

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn update_where_and_delete_where() -> Result<(), CollectionError> {
        let path = test_path();
        let mut storage = Storage::create(&path)?;
        let mut coll = Collection::create(&mut storage, "events")?;

        for i in 0..10 {
            coll.insert(&json!({"index": i, "kind": "click"}))?;
        }
        for i in 10..15 {
            coll.insert(&json!({"index": i, "kind": "scroll"}))?;
        }

        assert_eq!(coll.count()?, 15);

        // Update all clicks.
        let updated = coll.update_where(
            &Filter::parse(&json!({"kind": "click"})).unwrap(),
            &[(Path::parse("processed"), json!(true))],
        )?;
        assert_eq!(updated, 10);

        // Delete scrolls.
        let deleted = coll.delete_where(&Filter::parse(&json!({"kind": "scroll"})).unwrap())?;
        assert_eq!(deleted, 5);
        assert_eq!(coll.count()?, 10);

        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn invalid_names_rejected() {
        let path = test_path();
        let mut storage = Storage::create(&path).unwrap();
        assert!(Collection::create(&mut storage, "").is_err());
        assert!(Collection::create(&mut storage, "with space").is_err());
        assert!(Collection::create(&mut storage, "with/slash").is_err());
        assert!(Collection::create(&mut storage, &"x".repeat(65)).is_err());
        assert!(Collection::create(&mut storage, "valid_name-1").is_ok());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn drop_collection_removes_documents() -> Result<(), CollectionError> {
        let path = test_path();
        let mut storage = Storage::create(&path)?;
        let mut coll = Collection::create(&mut storage, "tmp")?;
        coll.insert(&json!({"x": 1}))?;
        coll.insert(&json!({"x": 2}))?;
        assert!(coll.drop_collection()?);
        // Re-create after drop should succeed.
        let mut coll2 = Collection::create(&mut storage, "tmp")?;
        assert_eq!(coll2.count()?, 0);
        std::fs::remove_file(path)?;
        Ok(())
    }
}

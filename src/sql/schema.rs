//! Table schema catalog.
//!
//! The catalog is stored in the v4 KV store under the reserved key prefix
//! `__schema__:<table_name>`. Each table's schema is encoded as:
//!
//! ```text
//!   u16 column_count
//!   for each column:
//!     u8 type_tag
//!     u16 name_len
//!     name bytes
//!     u8 nullable (0/1)
//!     u8 primary_key (0/1)
//! ```
//!
//! Table data is stored under `t:<table_name>:<rowid>` where `<rowid>` is a
//! big-endian u64 so that lexicographic key ordering matches numeric ordering.

use crate::Storage;
use crate::sql::types::{Value, ValueError};
use std::io::{self, ErrorKind};

/// Reserved key prefix for schema catalog entries.
pub(crate) const SCHEMA_PREFIX: &str = "__schema__:";

/// Reserved key prefix for table row data.
pub(crate) const TABLE_PREFIX: &str = "t:";

/// SQL column types supported by Passql.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnType {
    Integer,
    Real,
    Text,
    Blob,
}

impl ColumnType {
    pub fn type_tag(self) -> u8 {
        match self {
            Self::Integer => 1,
            Self::Real => 2,
            Self::Text => 3,
            Self::Blob => 4,
        }
    }

    pub fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Integer),
            2 => Some(Self::Real),
            3 => Some(Self::Text),
            4 => Some(Self::Blob),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integer => "INTEGER",
            Self::Real => "REAL",
            Self::Text => "TEXT",
            Self::Blob => "BLOB",
        }
    }
}

/// A single column definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub column_type: ColumnType,
    pub nullable: bool,
    pub primary_key: bool,
}

/// A table's full schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    pub table: String,
    pub columns: Vec<Column>,
}

impl Schema {
    pub fn primary_key_index(&self) -> Option<usize> {
        self.columns.iter().position(|column| column.primary_key)
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(self.columns.len() as u16).to_le_bytes());
        for column in &self.columns {
            bytes.push(column.column_type.type_tag());
            bytes.extend_from_slice(&(column.name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(column.name.as_bytes());
            bytes.push(if column.nullable { 1 } else { 0 });
            bytes.push(if column.primary_key { 1 } else { 0 });
        }
        bytes
    }

    pub fn decode(table: &str, payload: &[u8]) -> Result<Self, SchemaError> {
        if payload.len() < 2 {
            return Err(SchemaError::TruncatedHeader);
        }
        let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
        let mut offset = 2;
        let mut columns = Vec::with_capacity(count);
        for _ in 0..count {
            let type_tag = *payload.get(offset).ok_or(SchemaError::TruncatedColumn)?;
            offset += 1;
            let name_len = u16::from_le_bytes([
                *payload.get(offset).ok_or(SchemaError::TruncatedColumn)?,
                *payload
                    .get(offset + 1)
                    .ok_or(SchemaError::TruncatedColumn)?,
            ]) as usize;
            offset += 2;
            let name_bytes = payload
                .get(offset..offset + name_len)
                .ok_or(SchemaError::TruncatedColumn)?;
            let name =
                String::from_utf8(name_bytes.to_vec()).map_err(|_| SchemaError::InvalidUtf8)?;
            offset += name_len;
            let nullable = *payload.get(offset).ok_or(SchemaError::TruncatedColumn)? != 0;
            offset += 1;
            let primary_key = *payload.get(offset).ok_or(SchemaError::TruncatedColumn)? != 0;
            offset += 1;
            let column_type =
                ColumnType::from_tag(type_tag).ok_or(SchemaError::UnknownTypeTag(type_tag))?;
            columns.push(Column {
                name,
                column_type,
                nullable,
                primary_key,
            });
        }
        Ok(Self {
            table: table.to_owned(),
            columns,
        })
    }
}

/// In-memory + on-disk table catalog backed by the v4 KV store.
pub struct TableCatalog<'a> {
    storage: &'a mut Storage,
}

impl<'a> TableCatalog<'a> {
    pub fn new(storage: &'a mut Storage) -> Self {
        Self { storage }
    }

    pub fn create_table(&mut self, schema: &Schema) -> io::Result<()> {
        let key = format!("{SCHEMA_PREFIX}{}", schema.table);
        if self.storage.get(&key)?.is_some() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                format!("table '{}' already exists", schema.table),
            ));
        }
        self.storage.set(&key, &schema.encode())?;
        Ok(())
    }

    pub fn drop_table(&mut self, table: &str) -> io::Result<bool> {
        let key = format!("{SCHEMA_PREFIX}{table}");
        if self.storage.get(&key)?.is_none() {
            return Ok(false);
        }
        // Delete the schema entry and every row in the table.
        let start = format!("{TABLE_PREFIX}{table}:");
        let end = format!("{TABLE_PREFIX}{table};"); // ';' is the byte after ':'
        let rows = self.storage.range(Some(&start), Some(&end))?;
        let mut transaction = self.storage.begin()?;
        transaction.delete(&key)?;
        for (row_key, _) in rows {
            transaction.delete(&row_key)?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn load(&mut self, table: &str) -> io::Result<Option<Schema>> {
        let key = format!("{SCHEMA_PREFIX}{table}");
        match self.storage.get(&key)? {
            Some(payload) => {
                let schema = Schema::decode(table, &payload)
                    .map_err(|error| io::Error::new(ErrorKind::InvalidData, error.to_string()))?;
                Ok(Some(schema))
            }
            None => Ok(None),
        }
    }

    pub fn list_tables(&mut self) -> io::Result<Vec<String>> {
        let start = SCHEMA_PREFIX;
        let end = "__schema__;"; // ';' is the byte after ':'
        let entries = self.storage.range(Some(start), Some(end))?;
        let prefix_len = SCHEMA_PREFIX.len();
        Ok(entries
            .into_iter()
            .filter_map(|(key, _)| key.get(prefix_len..).map(|name| name.to_owned()))
            .collect())
    }
}

/// Encode a row's values into a single binary payload.
///
/// Each value is `(type_tag: u8, value_len: u32, value_bytes)`. NULL has a
/// zero-length payload and the type tag is the only byte.
pub fn encode_row(values: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(values.len() as u16).to_le_bytes());
    for value in values {
        let payload = value.encode();
        bytes.push(value.type_name_tag());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    bytes
}

/// Decode a row's values from a binary payload.
pub fn decode_row(payload: &[u8]) -> Result<Vec<Value>, ValueError> {
    if payload.len() < 2 {
        return Err(ValueError::InvalidIntegerLength(payload.len()));
    }
    let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    let mut offset = 2;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = *payload.get(offset).ok_or(ValueError::UnknownTypeTag(0))?;
        offset += 1;
        let len_bytes: [u8; 4] = payload
            .get(offset..offset + 4)
            .ok_or(ValueError::InvalidIntegerLength(payload.len() - offset))?
            .try_into()
            .map_err(|_| ValueError::InvalidIntegerLength(payload.len() - offset))?;
        let len = u32::from_le_bytes(len_bytes) as usize;
        offset += 4;
        let value_bytes = payload
            .get(offset..offset + len)
            .ok_or(ValueError::InvalidIntegerLength(payload.len() - offset))?;
        offset += len;
        values.push(Value::decode(tag, value_bytes)?);
    }
    Ok(values)
}

impl Value {
    fn type_name_tag(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Integer(_) => 1,
            Self::Real(_) => 2,
            Self::Text(_) => 3,
            Self::Blob(_) => 4,
        }
    }
}

/// Encode a rowid as a big-endian u64 so that lexicographic key ordering
/// matches numeric ordering.
pub fn row_key(table: &str, rowid: u64) -> String {
    format!("{TABLE_PREFIX}{table}:{rowid:020x}")
}

/// Lexicographically smallest key for a table (for range scans).
pub fn table_start_key(table: &str) -> String {
    format!("{TABLE_PREFIX}{table}:")
}

/// Lexicographic upper bound for a table (first byte after ':').
pub fn table_end_key(table: &str) -> String {
    format!("{TABLE_PREFIX}{table};")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    TruncatedHeader,
    TruncatedColumn,
    InvalidUtf8,
    UnknownTypeTag(u8),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TruncatedHeader => write!(f, "truncated schema header"),
            Self::TruncatedColumn => write!(f, "truncated schema column"),
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 in column name"),
            Self::UnknownTypeTag(tag) => write!(f, "unknown column type tag: {tag}"),
        }
    }
}

impl std::error::Error for SchemaError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("passql-schema-{}-{id}.psl", std::process::id()))
    }

    fn sample_schema() -> Schema {
        Schema {
            table: "users".to_owned(),
            columns: vec![
                Column {
                    name: "id".to_owned(),
                    column_type: ColumnType::Integer,
                    nullable: false,
                    primary_key: true,
                },
                Column {
                    name: "name".to_owned(),
                    column_type: ColumnType::Text,
                    nullable: false,
                    primary_key: false,
                },
                Column {
                    name: "score".to_owned(),
                    column_type: ColumnType::Real,
                    nullable: true,
                    primary_key: false,
                },
            ],
        }
    }

    #[test]
    fn schema_roundtrips_through_encode_decode() {
        let original = sample_schema();
        let bytes = original.encode();
        let decoded = Schema::decode("users", &bytes).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn row_roundtrips_through_encode_decode() {
        let row = vec![
            Value::Integer(42),
            Value::Text("Alice".to_owned()),
            Value::Real(2.5),
            Value::Null,
            Value::Blob(vec![0, 255, 1]),
        ];
        let bytes = encode_row(&row);
        let decoded = decode_row(&bytes).unwrap();
        assert_eq!(row, decoded);
    }

    #[test]
    fn catalog_persists_tables_across_reopen() -> io::Result<()> {
        let path = test_path();
        {
            let mut storage = Storage::create(&path)?;
            let mut catalog = TableCatalog::new(&mut storage);
            catalog.create_table(&sample_schema())?;
            let tables = catalog.list_tables()?;
            assert_eq!(tables, vec!["users".to_owned()]);
        }
        {
            let mut storage = Storage::open(&path)?;
            let mut catalog = TableCatalog::new(&mut storage);
            let loaded = catalog.load("users")?.unwrap();
            assert_eq!(loaded, sample_schema());
        }
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn catalog_rejects_duplicate_table() -> io::Result<()> {
        let path = test_path();
        let mut storage = Storage::create(&path)?;
        let mut catalog = TableCatalog::new(&mut storage);
        catalog.create_table(&sample_schema())?;
        assert!(catalog.create_table(&sample_schema()).is_err());
        std::fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn row_keys_are_lexicographically_ordered_by_rowid() {
        let k1 = row_key("users", 1);
        let k100 = row_key("users", 100);
        let k_max = row_key("users", u64::MAX);
        assert!(k1 < k100);
        assert!(k100 < k_max);
    }
}

# Passql roadmap

## Vision

Passql is intended to become an embedded database with a dependable storage
core and two interfaces: practical SQL for relational data and an ergonomic
document API for structured data. Both interfaces share transactions, pages,
allocation, and persistent indexes.

The strategic position: **SQLite-class embeddable + first-class documents +
edge-native + Rust safety**. The wedge is that no existing database ships
this combination cleanly today — SQLite treats JSON as an afterthought,
MongoDB has no SQL, D1 is locked to Cloudflare, LibSQL is a SQLite fork
with no document model.

This roadmap describes work that is not yet complete. It does not claim SQL,
document, ORM, edge, or production support beyond what is implemented.

## What is done today

- **Storage core** (`src/storage.rs`, `src/btree.rs`, `src/format.rs`):
  - Rust 2024 crate and `passql` command
  - 4096-byte pages, copy-on-write B+ tree
  - Checksummed record and index pages (64-bit FNV-1a over the page envelope)
  - Atomic root publication via alternating 48-byte commit slots
  - Snapshot readers that do not block writers
  - Transactional API: insert, update, delete, point lookup, range scans
  - Read-only format inspection and full-tree verification
  - 18 passing tests, clean formatting, Clippy with warnings denied
- **CLI**: `create`, `set`, `batch`, `lookup`, `delete`, `range`, `info`,
  `verify`
- **On-disk format v4**: see [`docs/file-format.md`](docs/file-format.md)

## What is missing

Grouped by stage. Each stage has a concrete deliverable.

### Stage 2 — SQL executor (next)

- `sqlparser-rs` integration for parsing
- AST → logical plan → executor on top of the v4 KV store
- Tables = key-prefix encoding (SQLite rowid scheme)
- DDL: `CREATE TABLE`, `DROP TABLE`
- DML: `INSERT`, `SELECT`, `UPDATE`, `DELETE` with `WHERE`, `ORDER BY`, `LIMIT`
- Types: `INTEGER`, `TEXT`, `BLOB`, `REAL`
- Prepared statements + parameter binding
- Schema catalog (table definitions stored as system keys)
- CLI `query <sql>` command

### Stage 3 — Documents

- JSON value type (use `serde_json` as a starting point)
- Collections as a special table type
- Field paths (`a.b.c`), filters, projections, upsert, find, update
- Indexed fields (commit atomically with their documents)
- SQL access to documents where it can be specified without weakening SQL

### Stage 4 — Hardening

- Free-page allocator + page reclamation around snapshot lifetimes
- B+ tree rebalancing on delete (merge / redistribute underfull nodes)
- Slotted page persistence (multiple records per page — closes the
  ~95% storage overhead vs SQLite for small rows)
- Open-time O(1) (cache the root, validate lazily on first read)
- Failure injection at every sync boundary
- Property-based tests + fuzzing
- Page-level checksum strengthening (CRC32C instead of FNV-1a)

### Stage 5 — Edge & bindings

- `Page`/`StorageBackend` trait so file / in-memory / WASM / IndexedDB
  backends can plug in
- WASM target with IndexedDB-backed storage adapter
- TypeScript client via `napi-rs` (Node), API-compatible with `better-sqlite3`
- SQLite wire-protocol server (HTTP/2 framing, libsql-compatible)
- ORM adapter tests: Drizzle, Prisma, Kysely
- Benchmarks vs SQLite / LibSQL / redb — point lookup, range, insert, open

### Stage 6 — Production readiness

- Backup / restore / migration tools
- Interactive shell with query history
- VS Code binary file inspector (no SQL rendering of arbitrary bytes)
- Real-world field testing
- Continuous fuzzing via OSS-Fuzz

## Format compatibility

- **Version 1** — historical, read-only inspection only. Cannot be opened or
  written by the current engine.
- **Version 4** — current. Copy-on-write B+ tree, checksummed pages,
  alternating commit slots. Still has known limits (no page reclamation,
  open is O(index size), no proven power-loss safety).

Legacy v2/v3 formats were deleted in this stage. They were experimental,
had no shared API with v4, no migration path, and no production users.
If a v2/v3 file needs to be read in the future, an export tool can be
written against the historical git history.

## Handoff

Before starting Stage 2, read [`docs/file-format.md`](docs/file-format.md)
for the v4 on-disk layout and the test suite in `src/storage.rs` for the
transaction contract. The B+ tree and transaction path exist and pass
injected-failure tests, but real power-loss safety is still unproven.

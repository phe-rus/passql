# Passql roadmap

## Vision

Passql is intended to become an embedded database with a dependable storage
core and two interfaces: practical SQL for relational data and an ergonomic
document API for structured data. Both interfaces should share transactions,
pages, allocation, and persistent indexes. Native storage comes first. Other
platforms must use honest storage adapters that state what durability they can
provide.

This roadmap describes work that is not yet complete. It does not claim SQL,
document, ORM, edge, or production support. Version 4 has an experimental
persistent key index.

## Current architecture

The crate uses Rust 2024 and the standard library only. `src/lib.rs` exposes
separate modules for version 2 storage, version 3 storage, version 4 indexed
storage, transactions, records, key and value encoding, slotted pages,
readers, and format inspection. The CLI is `src/bin/passql.rs`.

Version 2 writes a single page count into its header and appends fixed size
pages. Version 3 appends records and publishes batches through alternating
commit slots. Version 4 appends checksummed records and copy on write B+ tree
nodes, then publishes a new root in an alternate commit slot. All formats use
4096 byte pages. Detailed limitations are in
[`docs/architecture/audit.md`](docs/architecture/audit.md).

## Completed and verified

* Rust 2024 crate and `passql` command.
* Version 2 page creation, append, read, simple records, and key lookup.
* Experimental version 3 append batches, commit metadata, and transactions.
* Version 3 read only access, recovery selection, and an integrity scan for
  records.
* In memory slotted page encoding with multiple variable sized records and
  logical deletion.
* Signature based metadata inspection for historical version 1 and versions
  2, 3, and 4.
* CLI `info` and checksum aware `verify` behavior.
* Version 4 checksummed record and index pages, a persistent ordered B+ tree,
  leaf and internal splits, root growth, point lookup, range reads, update,
  delete, and reopen persistence.
* Version 4 copy on write transactions publish record changes and the new
  index root together. Recovery rejects corrupted newer roots and retains a
  valid earlier generation. Snapshot readers do not block later writers.
* CLI commands for version 4 create, set, batch, lookup, range, and delete.
* 71 passing tests, clean formatting, and Clippy with warnings denied.

The slotted page implementation is present in the working tree but was not
tracked when this roadmap was written. Preserve it and review its ownership
before treating it as a completed project change.

## Partial work

* Version 3 commit slots and write ordering are an experiment, not a complete
  proof of crash consistency.
* Version 3 transactions stage multiple records but do not provide staged
  reads, updates, deletes, or indexes.
* Slotted pages can encode and delete records in memory, but are not persisted
  or integrated with an allocator. Deleted space is not compacted.
* Format verification can check version 2 record pages and version 3 records.
  It cannot establish integrity for raw or unchecksummed pages.
* The CLI has direct developer commands, not a shell, SQL query command, or
  migration tool.

## Missing work and known risks

* A complete format specification and explicit policy for legacy version 1 files.
* Version 4 copy on write and checksums do not prove crash safety across
  arbitrary filesystems. Failure injection covers failure before publication
  and a partial commit slot write, not actual power loss or every page write
  and synchronization boundary.
* Version 4 does not reclaim obsolete pages. Deletes leave underfull nodes and
  old records in the file.
* Version 4 open and verification walk the full tree and referenced records,
  so startup validation is linear in the index size.
* There is no persistent free page allocator or slotted page compaction.
* Failure injection is still needed for each page write and synchronization
  boundary, plus commit synchronization failures.
* SQL parser, AST, planner, executor, tables, constraints, prepared
  statements, and transaction semantics.
* Structured documents, nested value paths, projections, validation, and
  indexed fields.
* TypeScript bindings and actual ORM integration tests.
* In memory and portable storage adapters for WASM and edge runtimes.
* Backups, migration tools, format inspection in an editor, and benchmarks.

## Prioritized phases

### 1. Harden version 4 transactions

Add failure injection at each page write, sync, partial commit write, and
commit sync boundary. Test that recovery publishes either the old or the full
new tree. Add process level locking tests and verify that snapshot readers
continue to read their original generation.

### 2. Specify and freeze all formats

Write golden format fixtures and define which versions are supported for
inspection, reading, writing, and migration. Never change the meaning of an
existing version. Add migration only as an explicit copy to a new file with
validation. Document the limits of version 4 sync and directory entry
durability.

### 3. Improve B+ tree deletion and storage reuse

Add redistribution or merging for underfull nodes, stable record references,
free page tracking, and compaction. Design reclamation around reader snapshots
so no page can be reused while an older reader still references it.

### 4. Unify storage and transactions

Retain old readers as compatibility code while moving new writes to one
transaction manager and allocator. Add read only open, writer coordination,
recovery, staged reads, update and delete semantics, and an in memory backend.

### 5. Persist slotted pages and allocation

Integrate validated slotted pages with the storage layer. Add slot generations
or another stale reference defense, free space tracking, reclamation, and
compaction. Test corrupt offsets, overlap, reuse, and interrupted updates.

### 6. Measure the B+ tree

Add reproducible benchmarks against a linear scan, including open time,
point lookup, update, and range workloads. Do not claim superiority over
SQLite without comparable measurements.

### 7. Add relational SQL

Use a maintained SQL parser and a structured AST. Start with tables, basic
types, inserts, selects, updates, deletes, predicates, ordering, limits,
prepared parameters, primary keys, and transaction tests. Add joins,
aggregates, schema changes, and planning only after semantics are covered.

### 8. Add document operations

Use a safe JSON value representation and the same storage, transactions, and
indexes. Add collections, identifiers, nested paths, filters, projections,
upsert, validation, and SQL access where it can be specified without weakening
SQL semantics.

### 9. Add client and platform integrations

Define a stable Rust API first. Build a TypeScript client and dedicated
adapters only after SQL statement, binding, result, error, and lifecycle
contracts exist. Test Bun, Node.js, Drizzle, and any other claimed integration
directly. Add WASM or edge storage only through adapters that expose their
actual durability limits.

### 10. Improve CLI and editor support

Add an interactive shell, query history, migrations, export, backups, and a
VS Code binary editor only when each operation can safely recognize and
inspect database files without rendering arbitrary bytes as SQL text.

## Format compatibility

* Version 1 local files use uppercase `PASSQL` or lowercase `passql`
  signatures and are only recognized for metadata inspection by the current
  code.
* Version 2 uses the `passql` signature and is the current `Storage` format.
* Version 3 uses the `passql3` signature and remains experimental.
* Version 4 uses the `passql4` signature and remains experimental.
* The version 1, version 2, version 3, and version 4 formats do not share one
  read or write API. No migration command exists.
* Local `.psl` samples are ignored by Git. Preserve them as user data.

## Handoff

Before starting the next phase, read its architecture note and the findings in
[`docs/architecture/audit.md`](docs/architecture/audit.md). The B+ tree and
transaction path now exist in version 4. Do not add SQL or documents until
write and synchronization failure behavior is tested more completely.

# Passql

Passql is an embedded, edge-native database engine written in Rust. It targets
a single storage core with two interfaces: practical SQL for relational data
and an ergonomic document API for structured data — both sharing the same
transactions, page allocator, and persistent indexes.

The vision is **SQLite-class embeddable + first-class documents + edge-native
+ Rust safety** — a combination that no existing database ships cleanly today.

The native database extension is `.psl`. Passql identifies files by their
binary signature, not by their file name.

## Current status

The crate uses Rust 2024 and currently has **one** external dependency:
[`sqlparser`](https://crates.io/crates/sqlparser) (added in stage 2).

The **storage core** is implemented:

- Fixed 4096-byte pages
- Copy-on-write B+ tree with checksummed record and index pages
- Atomic root publication via alternating commit slots
- Snapshot readers that do not block writers
- Transactional API supporting insert, update, delete, point lookup, and
  ordered range scans
- Read-only format inspection (`info`) and full-tree verification (`verify`)

The following are **not yet implemented**:

- SQL parsing, execution, prepared statements, schema catalog
- Document collections, JSON values, nested field paths, projections
- Slotted page persistence and page reclamation (file grows monotonically
  today — see [the roadmap](ROADMAP.md))
- Crash-safety testing under real power loss
- TypeScript bindings, WASM target, SQLite wire-protocol compatibility
- Benchmarks vs SQLite / LibSQL / redb

**Do not use Passql as the only copy of important data.** The current engine
is research-grade and has not been hardened against power loss or fuzzed.

## Build and test

You need a current stable Rust toolchain with Cargo.

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Command line

Run the CLI with `cargo run --bin passql --`. Creating a database fails
rather than overwriting an existing file.

```sh
passql create  app.psl
passql set     app.psl user:1 Alice
passql batch   app.psl user:2 Bob user:3 Carol
passql lookup  app.psl user:1
passql range   app.psl user:1 user:3
passql delete  app.psl user:2
passql info    app.psl
passql verify  app.psl
```

`info` recognizes the current v4 signature plus historical v1 files for
read-only inspection. `verify` walks the live B+ tree, validates every page
checksum, key order, and record reference, and reports the count of
reachable records.

## Rust library

```rust
use passql::Storage;

let mut database = Storage::create("example.psl")?;
let mut transaction = database.begin()?;
transaction.set("user:1", b"Alice")?;
transaction.set("user:2", b"Bob")?;
assert_eq!(transaction.get("user:1")?, Some(b"Alice".to_vec()));
transaction.commit()?;

let value = database.get("user:1")?;
# Ok::<(), std::io::Error>(())
```

A read-only snapshot reader does not block the writer:

```rust
use passql::IndexReader;

let mut reader = IndexReader::open("example.psl")?;
// reader sees a stable generation even if a writer commits new data
let value = reader.get("user:1")?;
# Ok::<(), std::io::Error>(())
```

`passql::format::inspect` reads format metadata without modifying the file.
`passql::format::verify` walks the tree and validates every page.

## Architecture and direction

Passql aims to use **one storage and transaction core** for both relational
SQL and structured documents. The implementation has not reached either
interface yet. The planned order is:

1. ✅ Storage core (v4 B+ tree, CoW transactions, checksums)
2. 🚧 SQL executor on top of the KV store (stage 2)
3. 🚧 Document layer (stage 3)
4. 🚧 Free-page allocator + slotted page persistence (stage 4)
5. 🚧 WASM target + TypeScript bindings + SQLite wire compat (stage 5)

See [the roadmap](ROADMAP.md) for the full phased plan and
[the file format notes](docs/file-format.md) for the on-disk layout.

The current engine depends on native file operations and operating system
locks for writers. Snapshot readers use immutable pages and do not block
later writers. Browser, WebAssembly, Workers, mobile, and remote storage
support are future work, gated behind platform adapters that state the
durability they can actually provide.

## Project links

Source repository: <https://github.com/phe-rus/passql>

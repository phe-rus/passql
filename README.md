# Passql

Passql is an experimental, embedded database written in Rust. It explores a
small page based storage engine and an SQL oriented database that can also
support structured documents. The current implementation is a storage
prototype, not a relational SQL engine or a production database.

The native database extension is `.psl`. Passql identifies files by their
binary signature and format metadata, not by the file name.

## Current status

The crate uses Rust 2024 and has no external dependencies. It currently
provides fixed size pages, binary records, versioned key and value storage,
append based transactions in experimental format version 3, and a
copy on write persistent B+ tree in experimental format version 4. Format
inspection recognizes historical version 1 and versions 2, 3, and 4.

The current test suite has 71 tests. Run it with `cargo test`.

Passql does not yet implement SQL parsing or execution, document collections,
slotted page compaction, page reclamation, or a TypeScript client. Version 4
integrity validation reads the full index on open. It has no benchmarks and
makes no performance comparison with SQLite. Do not use it as the only copy
of important data.

## Build and test

You need a current stable Rust toolchain with Cargo.

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Command line

Run the CLI with `cargo run --bin passql --`. Creating a database uses the
current version 2 storage API and does not overwrite an existing file.

```sh
cargo run --bin passql -- create app.psl
cargo run --bin passql -- info app.psl
cargo run --bin passql -- put app.psl "hello"
cargo run --bin passql -- get app.psl 1
cargo run --bin passql -- set app.psl greeting "hello"
cargo run --bin passql -- lookup app.psl greeting
cargo run --bin passql -- verify app.psl
```

`info` recognizes version 1, 2, 3, and 4 files by signature, even if the file
has another extension. It does not change the file. `verify` checks record
checksums where the format has them and reports older pages that have no
record checksum. It does not claim that unchecksummed data is intact.

Version 3 commands remain explicitly experimental:

```sh
cargo run --bin passql -- create3 app-v3.psl
cargo run --bin passql -- batch3 app-v3.psl user:1 Alice user:2 Bob
cargo run --bin passql -- lookup3 app-v3.psl user:1
cargo run --bin passql -- info3 app-v3.psl
```

`create3` uses create only semantics. It fails rather than replacing an
existing file. The version 3 writer holds an exclusive operating system lock.
The version 3 reader permits shared readers, but no server or cross platform
locking contract is promised.

Version 4 adds an experimental persistent ordered key index and transactions:

```sh
cargo run --bin passql -- create4 app-v4.psl
cargo run --bin passql -- batch4 app-v4.psl user:2 Bob user:1 Alice
cargo run --bin passql -- lookup4 app-v4.psl user:1
cargo run --bin passql -- range4 app-v4.psl user:1 user:3
cargo run --bin passql -- delete4 app-v4.psl user:2
cargo run --bin passql -- verify app-v4.psl
```

Version 4 uses copy on write tree pages and publishes a new root in one
checksummed commit slot. It supports a single writer, read only snapshot
handles, ordered UTF 8 string keys, binary values, insert, update, delete,
point lookup, and range reads. It is not yet a general crash safety guarantee.
See [the version 4 format notes](docs/file-format.md).

## Rust library

The existing low level API is intended for experiments. A version 2 example:

```rust
use passql::{Record, Storage};

let mut database = Storage::create("example.psl")?;
let page = database.append_record(&Record::new(b"hello".to_vec()))?;
let record = database.read_record(page)?;
assert_eq!(record.data, b"hello");
# Ok::<(), std::io::Error>(())
```

Version 3 transactions can stage several key and value records before one
commit:

```rust
use passql::storage_v3::StorageV3;

let mut database = StorageV3::create("example-v3.psl")?;
let mut transaction = database.begin();
transaction.set("user:1", b"Alice")?;
transaction.set("user:2", b"Bob")?;
transaction.commit()?;
# Ok::<(), std::io::Error>(())
```

Version 4 transactions stage indexed writes and deletes, and support reads
from their own staged changes:

```rust
use passql::StorageV4;

let mut database = StorageV4::create("example-v4.psl")?;
let mut transaction = database.begin()?;
transaction.set("user:1", b"Alice")?;
assert_eq!(transaction.get("user:1")?, Some(b"Alice".to_vec()));
transaction.commit()?;
let value = database.get("user:1")?;
# Ok::<(), std::io::Error>(())
```

`passql::format::inspect` reads format metadata without modifying the file.
`passql::format::verify` reports the records it could check and any scanned
pages without a record checksum.

## Storage formats

The repository has multiple historical formats. They are not interchangeable.
Format version 2 is used by `Storage`; version 3 is an experimental API with a
separate writer and reader; version 4 is an experimental indexed key and value
store. See [the file format guide](docs/file-format.md) and
[the compatibility notes](docs/compatibility.md) before handling existing
files. Passql does not silently convert or rewrite a database to another
version.

## Architecture and direction

Passql aims to use one storage and transaction core for both relational SQL
and structured documents. The implementation has not reached either interface
yet. The planned order is to make page integrity and transaction recovery
reliable, then add allocation and indexing, and only then add query and
document layers.

The current engine depends on native file operations and operating system
locks for writers. Version 4 snapshot readers use immutable pages. Browser,
WebAssembly, Workers, mobile, and remote storage support are future work. See
[the architecture overview](docs/architecture/overview.md) and the
[roadmap](ROADMAP.md).

## ORM and client compatibility

There is no TypeScript client or SQL driver. Drizzle, Prisma, Kysely, SQLite
drivers, and migration tools are not supported. SQLite like syntax or a `.psl`
extension alone would not satisfy their driver contracts. See
[compatibility](docs/compatibility.md) for the current status and required
integration work.

## Development

Start with [development instructions](docs/development.md). The repository
contains local `.psl` sample files that are ignored by Git. Do not overwrite,
delete, or treat them as disposable test data. Tests should create uniquely
named temporary files and remove only the files they create.

Contributions should preserve existing format meanings, add regression tests
for storage changes, and document any format compatibility effect. Run the
format, test, and lint commands above before submitting changes.

## Project links

Source repository: <https://github.com/phe-rus/passsql>

Optional desktop association status is documented in
[file associations](docs/file-associations.md). No system setting is changed
by Passql.

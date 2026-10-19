# Compatibility

## Database formats

| Version | Signature | Current support | Notes |
|---|---|---|---|
| 1 | `PASSQL` or `passql` plus zero bytes | Metadata inspection and structural page scan | Historical writer support exists in Git history. Current write and read APIs do not open this version. Page data has no current record checksum contract. |
| 2 | `passql` plus zero bytes | Current `Storage` create, open, append, read, and key lookup | Header page count is updated in place. The format has no general page checksum or locking. Opening a writer may truncate an uncommitted tail. |
| 3 | `passql3` plus zero byte | Experimental writer, transactions, reader, info, and record verification | Uses alternating checksummed commit slots and operating system locks. Full crash consistency is not proven. |
| 4 | `passql4` plus zero byte | Experimental indexed key and value store, B+ tree, transactions, snapshot reader, and verification | Uses immutable appended pages, page checksums, and a root pointer in alternating commit slots. It does not reclaim pages. Tests cover failures before publication and a partial commit slot write, not real power loss or all filesystems. |

Inspection is based on signature and metadata, not the file extension.
Inspection does not migrate or modify the database. No migration command is
implemented.

## Rust API

The library exposes format specific APIs. `Storage` handles version 2.
`StorageV3`, `Transaction`, and `DatabaseReader` handle version 3. These are
not interchangeable. `StorageV4`, `TransactionV4`, and `IndexReaderV4`
handle version 4. There is no version neutral transaction API.

## SQL clients and ORMs

There is no SQL implementation, SQLite wire protocol, SQLite file format, or
SQLite driver compatibility. Drizzle, Drizzle Kit, Prisma, Kysely, Bun SQL
drivers, and Node.js SQLite drivers are not supported.

A Passql adapter would need to implement the actual target contract, including
prepared statement lifecycle, parameter binding, result shape, column
metadata, affected row counts, transactions, errors, schema introspection,
migrations, and connection lifecycle. It must pass integration tests with the
claimed ORM before the support status changes.

## TypeScript and platforms

There is no TypeScript client, native binding, FFI, WASM target, Cloudflare
Workers adapter, Durable Object storage, R2 adapter, or Android binding.
Native file locks and synchronous file operations cannot be assumed available
in those environments.

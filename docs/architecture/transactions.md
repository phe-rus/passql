# Transaction architecture

## Existing version 3 transaction

`StorageV3::begin` mutably borrows one writer handle. A transaction can stage
key and value records or binary records in memory. Commit appends all staged
pages, synchronizes them, then writes the next generation into the alternate
commit slot and synchronizes again. Dropping or rolling back a transaction
discards its staged pages without writing them.

This provides a simple batch publication model for a single writer handle. It
does not provide staged reads, update or delete operations, isolation between
multiple transactions, a journal, or atomic changes to records and indexes.
The staged page vector stores complete 4096 byte arrays, so memory use grows
with transaction size.

## Recovery limitations

Opening version 3 selects the newest checksum valid commit whose declared file
length is present. It does not verify all referenced page contents before
selecting that commit. The reader can detect bad record checksums later, but
does not fall back to an earlier generation for corrupt pages. A checksummed
commit slot is not equivalent to a durable transaction protocol.

Writable recovery may invalidate a rejected newer slot before truncating its
tail so that a later append cannot accidentally make stale metadata valid
again. This behavior is specific to version 3. Read only open does not repair
the file.

## Version 4 copy on write transactions

Version 4 leaves versions 1 through 3 unchanged. A transaction gathers
mutations, writes new record pages and copy on write B+ tree path pages, then
synchronizes all appended pages before publishing one new checksummed root
commit. A reader keeps the selected root and page count as a snapshot. Since
committed pages are immutable, a later commit does not change that snapshot.

Writable open validates the committed tree and referenced records. If a newer
commit is invalid, it clears that slot and synchronizes the invalidation before
truncating the uncommitted tail. A failure before root publication leaves the
previous generation selected. A failure during or after slot publication has
an uncertain return result, so the handle is poisoned and the caller must
reopen to discover whether the old or new complete generation was published.

This is not a claim of universal crash durability. The version 4 format uses
FNV 1a checksums for accidental corruption detection, does not sync the parent
directory on creation, and has not been tested under real power loss or every
filesystem.

## Required transaction contract

Before adding SQL or document transactions, define:

* Which reads see staged writes.
* Whether writes are atomic and durable after commit returns.
* What happens when any write, sync, or metadata publication fails.
* How rollback and handle poisoning work.
* How readers hold a stable generation during concurrent commits.
* How indexes, records, allocator metadata, and schema changes commit
  together.
* How recovery validates pages and decides whether to fall back.

Test every write and sync failure boundary, not only successful reopen.

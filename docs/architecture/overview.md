# Architecture overview

Passql is a Rust library and a small command line tool. The current design is
a storage experiment with multiple incompatible on disk versions, not yet a
complete database.

## Current module map

| Module | Responsibility today |
|---|---|
| `storage` | Version 2 page files and append operations |
| `storage_v3` | Experimental version 3 creation, writer locking, recovery, and page batches |
| `storage_v4` | Experimental version 4 indexed storage, copy on write transactions, and recovery |
| `btree` | Version 4 checksummed record and ordered B+ tree page encoding |
| `transaction` | Version 3 commit encoding and append only transaction staging |
| `reader` | Read only version 3 reader, lookup, and record verification |
| `record` | Fixed page record serialization and payload checksum |
| `kv` | Key and value record serialization and reverse scan lookup |
| `slotted` | In memory slotted page encoding |
| `format` | Read only signature detection, metadata inspection, and checksum aware verification |
| `bin/passql` | CLI commands |

Version 4 is the first persistent index and transaction path. It does not yet
share one public storage API with versions 2 and 3, and it does not reclaim
pages. SQL rows and documents should be different data interfaces over that core, not separate
engines. The present code is not ready to provide that shared core because
format, transaction, and allocation contracts are still experimental.

## Boundaries to preserve

* File format version is data. Never reinterpret an existing version.
* Inspection must not change files. Recovery belongs to a writable open path.
* A published index entry must never reference a record from a failed
  transaction.
* A platform adapter must state the durability and locking it can provide.
* SQL and document operations should share transactions, allocation, and
  indexing, while retaining their own validation and result semantics.

## Why the current files stay separate

Version 2 and version 3 are not two implementations of the same format. They
have different signatures, commit models, open behavior, and locking. Merging
them under a single public `Storage` name before specifying compatibility
would hide important differences. Small internal interfaces can be introduced
once a new format is chosen.

There are no SQL or document modules yet. Create them when the first working
parser and operation need a meaningful boundary, not as empty scaffolding.

## Next structural work

Keep old format behavior fixed. Harden version 4's copy on write publication
with failure tests, then add safe page reclamation around snapshot readers.

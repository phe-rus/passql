# Passql continuation note

## Current state

Passql uses Rust 2024 and has no external crate dependencies. Formats 1
through 3 remain separate from the experimental version 4 implementation.
The native file extension is `.psl`, and the code identifies database files
from their binary signature.

Version 4 is the current development focus. It uses copy on write B+ tree
pages, checksummed records and index pages, staged transaction reads, updates,
deletes, point lookups, ordered range reads, recovery selection, and snapshot
readers. A transaction appends replacement pages, synchronizes them, then
publishes the new root in an alternate commit slot.

The latest validation passed with 71 tests. Formatting, Clippy with warnings
denied, build, documentation generation, and `git diff --check` also passed.
A temporary database also passed the version 4 CLI create, batch, lookup,
range, delete, and verify workflow. Tests do not establish real power loss
safety or durability on every filesystem.

## Work in the repository

* `src/btree.rs` encodes version 4 records and ordered leaf and internal
  B+ tree pages. Pages include a checksum and reserved byte validation.
* `src/storage_v4.rs` implements the version 4 writer, snapshot reader,
  transactions, append only copy on write updates, recovery, and tests.
* `src/format.rs` inspects and verifies known format signatures without
  changing the file.
* `src/bin/passql.rs` exposes version 4 create, batch, set, lookup, range,
  and delete commands.
* `src/slotted.rs` remains an in memory experiment. It is not used by
  version 4 and does not have durable compaction.
* `docs/architecture/audit.md`, `docs/architecture/`, `docs/file-format.md`,
  `docs/compatibility.md`, `README.md`, and `ROADMAP.md` describe verified
  behavior and current limits.

Do not change or remove unrelated work in the existing worktree. Local `.psl`
samples are ignored by Git. They must remain untouched.

## Known limits

* Version 4 appends pages and does not reclaim obsolete records or tree nodes.
* Deletion does not rebalance underfull nodes.
* Opening and verifying the index traverses the full reachable tree.
* Checksums detect accidental corruption, not intentional changes.
* Tests cover injected failures before publication and a partial commit slot
  write. They do not simulate actual power loss or every write and sync
  failure.
* Creation does not synchronize the parent directory entry.
* There is no SQL engine, document API, shared version neutral transaction
  API, TypeScript client, ORM integration, or benchmark.

## Recommended next engineering task

Harden version 4 transaction recovery. Add failure injection around every
page write, page sync, commit slot write, and commit sync. After each injected
failure, reopen the file and prove that recovery selects either the old
complete generation or the new complete generation. Add process level writer
locking tests. Do not claim general crash safety.

After that, make a safe page reclamation design for snapshot readers, then
balance B+ tree deletion. Start SQL only after the storage and transaction
contracts have stronger failure coverage.

## Validation commands

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build
cargo doc --no-deps
git diff --check
```

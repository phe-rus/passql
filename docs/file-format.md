# Passql file format

All current page sizes are **4096 bytes**. The `.psl` extension is
conventional; inspection uses the binary signature at offset 0.

**Do not change a format version's meaning.** These notes describe observed
layouts, not a stable public specification until Stage 6.

## Version 1 (legacy — inspection only)

The historical signature is eight bytes: `PASSQL` or `passql`, followed by
two zero bytes. Local version 1 samples may contain both capitalization
variants. The header stores version 1 at offset 8, page size at offset 12,
and page count at offset 16, all little endian. The first page is the
metadata page.

Current code can inspect this header and scan the declared file length, but
**cannot decode version 1 records or write this format**. There is no
migration path — write an export tool against historical git history if a
v1 file needs to be read.

## Version 4 (current)

The signature is `passql4` followed by a zero byte. The header stores
version 4 and the 4096-byte page size at offsets 8 and 12.

### Commit slots

Two **48-byte** commit slots begin at offsets 64 and 112. A slot stores:

| Offset | Length | Field |
|---|---|---|
| 0 | 8 | `pslcmv4\0` magic |
| 8 | 4 | format version (4) |
| 12 | 4 | reserved zeros |
| 16 | 8 | generation (u64) |
| 24 | 8 | page count (u64) |
| 32 | 8 | root page identifier (u64) |
| 40 | 8 | 64-bit FNV-1a checksum over bytes 0..40 |

Page zero is the header. The initial empty tree uses page zero as its root.

### Page envelope

Each non-header page starts with a 16-byte envelope:

| Offset | Length | Field |
|---|---|---|
| 0 | 4 | `PSL4` magic |
| 4 | 1 | page type: `1` = record, `2` = leaf, `3` = internal |
| 5 | 3 | reserved zeros |
| 8 | 8 | 64-bit FNV-1a checksum over the full page (excluding the checksum field itself) |

The checksum covers the signature, type, reserved bytes, and full page
payload.

### Page types

- **Record page** (type 1): one ordered key + binary value. Payload layout:
  `key_len (u16)`, `value_len (u32)`, `key bytes`, `value bytes`.
- **Leaf page** (type 2): ordered `(key, record_page_id)` pairs. Payload
  starts with `entry_count (u16)`, then for each entry: `key_len (u16)`,
  `key bytes`, `record_id (u64)`.
- **Internal page** (type 3): child page identifiers followed by ordered
  separator keys. Payload starts with `separator_count (u16)`, then
  `separator_count + 1` child page IDs (u64 each), then for each
  separator: `key_len (u16)`, `key bytes`.

Keys are UTF-8 strings from 1 to 1024 bytes. Values are binary and limited
by page capacity.

### Write protocol

Version 4 appends changed records and copy-on-write B+ tree path nodes,
then synchronizes all appended pages before writing and synchronizing the
alternate commit slot. This publishes the new root and page count together
in one atomic slot write.

Open validates candidate trees and referenced records before selecting a
generation. A writable open can clear a rejected commit slot and truncate
uncommitted tail pages. A snapshot reader selects a committed root and
does not modify the file. Writers use an exclusive operating system lock.

### Known limits (Stage 4 will address)

- Appended pages are **not reclaimed**. Every update and delete leaves dead
  pages on disk. The file grows monotonically.
- B+ tree deletion does **not rebalance** underfull nodes.
- Open walks the **full tree** to validate — startup is O(index size).
- Tests cover failure before publication and a partial commit-slot write.
  **Real power-loss safety is unproven.**
- Directory entry durability is not established on create.
- Checksums are FNV-1a (accidental corruption only, not cryptographic).

## Safety and compatibility

The checksums detect accidental corruption, not intentional tampering. The
v1 and v4 formats do not share a read or write API. No migration command
exists. Never replace a database with another version in place — a future
migration tool should write a new file, verify it, and only then allow an
explicit user-controlled replacement.

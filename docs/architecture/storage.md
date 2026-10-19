# Storage architecture

## Current behavior

Pages are 4096 bytes. Version 2 reserves page zero for a 24 byte header and
stores a page count at offset 16. Appends write a full page, synchronize it,
then overwrite the page count and synchronize again. Opening a version 2 file
opens it for writing and truncates bytes beyond the page count.

Version 3 reserves a 4096 byte header page. It stores the format version and
page size, then two commit slots at offsets 64 and 104. Data pages are
appended. The writer synchronizes them before writing and synchronizing the
alternate commit slot. Opening a version 3 database may invalidate a stale
slot and truncate an uncommitted tail. The read only reader does neither.

The version 2 and version 3 record representation is a type byte, a payload
length, a 32 bit FNV 1a payload checksum, and data. It stores one record per
page. The slotted page module can store several variable sized records in
memory, but the storage layer does not use it.

Version 4 adds a new signature, two commit slots, immutable appended record
and B+ tree pages, page checksums, and a committed root page identifier. A
transaction synchronizes every new page before publishing its root. The
previous commit remains available as a fallback. Readers hold a root snapshot
and do not prevent a later writer because committed pages are not changed.

## Risks

Version 2's page count is rewritten in place without checksum or locking.
Concurrent writers can target the same page and page count. A concurrent
opener can truncate an append before the writer publishes the new count.
Version 3 improves ordering and locking, but commit slots do not protect page
contents and their checksum is not a proof of atomic durable publication.

The current allocator only appends. Replaced key values remain on disk.
There is no free list, compaction, durable slotted page, or page level
checksum. The file header and record header are also not protected by a
checksum over the complete page.

Version 4 also appends only. Obsolete nodes and records are not reclaimed.
Recovery validates the full reachable tree and its record references, so open
time currently grows with the index size.

## Next design work

Keep existing versions fixed. Harden version 4 publication with failure
injection at each write and synchronization boundary. Then integrate slotted
pages with stable record references, free space tracking, deletion, and
compaction. Keep a read only path that never repairs or truncates.

## Filesystem assumptions

Current durability is unproven beyond the local tests. The code calls
`sync_data` and `sync_all`, but does not claim behavior for every filesystem,
controller cache, or operating system. Creation does not synchronize the
parent directory entry. Version 3 relies on operating system locks; version 2
does not lock. Version 4 writer locks and reader snapshots rely on immutable
pages and local file behavior. See [the audit](audit.md) before using these
experimental formats for important data.

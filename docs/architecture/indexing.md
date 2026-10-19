# Indexing

Versions 1 through 3 have no persistent index. `Storage::lookup`,
`StorageV3::lookup`, and `DatabaseReader::lookup` scan pages from newest to
oldest. Version 4 adds the first persistent ordered B+ tree through
`StorageV4` and `IndexReaderV4`. The older formats and APIs remain unchanged.

## B+ tree plan

The long term index design needs ordered comparison, internal separator
nodes, leaf values that identify records, splits, root growth, range
traversal, update, deletion, and integrity verification.

## Version 4 implementation

Version 4 stores UTF 8 string keys in byte order and binary values in record
pages. Keys compare by their UTF 8 byte sequence. Leaf entries point to record
pages. These page identifiers are not stable across updates because the
format is append only. Internal separator keys direct
lookups. Inserts copy the path to the leaf, split overfull leaf and internal
nodes, and grow a root when needed. Range reads are inclusive at the start
and exclusive at the end. Updates write a new record page and replace the
leaf reference. Deletes remove the leaf entry. Underfull nodes are not
redistributed or merged yet, and pages are not reclaimed.

All record and changed node pages for one transaction are appended and
synchronized before one commit publishes the new root. Open and integrity
verification walk the complete tree and validate page checksums, key order,
record references, and key agreement. This makes reopen validation linear in
the indexed data size for now. Point lookup after opening follows the tree.
The new root and record references are published together, so the current
append only model does not require stable page identifiers for reuse.

## Test requirements

Test ordering for supported keys, duplicate keys, empty trees, leaf
and internal splits, root growth, full range scans, updates, deletion,
rollback, reopen, corrupt links, missing records, and recovery after every
commit boundary. Compare results against a sorted in memory reference model.

Add benchmarks against the existing reverse scan using reproducible data
sizes and key distributions. Do not claim superiority over SQLite without
measured, comparable workloads.

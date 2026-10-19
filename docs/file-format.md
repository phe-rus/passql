# Passql file format notes

All current page sizes are 4096 bytes. The `.psl` extension is conventional;
inspection uses the binary signature. Do not change a format version's
meaning. These notes describe observed layouts and limitations, not a stable
public specification.

## Version 1

The historical signature is eight bytes: `PASSQL` or `passql`, followed by
two zero bytes. Local version 1 samples contain both capitalization
variants. The header stores version 1 at offset 8, page size at offset 12,
and page count at offset 16, all little endian. The first page is the
metadata page. Current code can inspect this header and scan the declared
file length, but does not decode version 1 records or write this format.

## Version 2

The signature is eight bytes: `passql` followed by two zero bytes. Its
metadata fields use the same offsets as version 1, but the format version is
2. The first page is reserved for the header. Each subsequent page is a
4096 byte payload. A page count update rewrites the header in place.

The storage module's `Record` format uses a one byte record type, four byte
little endian payload length, four byte FNV 1a checksum, then payload bytes.
The checksum covers only the payload. The key and value record has its own
four byte marker and two little endian 16 bit lengths. Raw pages are also
accepted by the low level append API, so not every page necessarily has a
record checksum. Because raw pages have no distinct page type envelope, a raw
page that begins with the record marker is ambiguous to the verifier.

## Version 3

The signature is `passql3` followed by a zero byte. The header stores version
3 and page size at the same offsets. Two 40 byte commit slots begin at offsets
64 and 104. Each commit stores the `pslcmmit` marker, version 3, generation,
page count, reserved bytes, and a 64 bit FNV 1a checksum over the first 32
bytes.

The header is page zero. Each data page is 4096 bytes. The writer appends data,
synchronizes it, writes the alternate commit slot, then synchronizes that
slot. The reader selects the newest valid commit whose declared file length
is present. It does not checksum all pages during selection.

## Version 4

The signature is `passql4` followed by a zero byte. The header stores version
4 and the 4096 byte page size at offsets 8 and 12. Two 48 byte commit slots
begin at offsets 64 and 112. A slot stores the `pslcmv4` marker, version,
generation, page count, root page identifier, and a 64 bit FNV 1a checksum
over its first 40 bytes. Page zero is the header. The initial empty tree uses
page zero as its root.

Each nonheader page starts with the `PSL4` marker, a page type, three reserved
zero bytes, and a 64 bit FNV 1a checksum. The checksum covers the signature,
type, reserved bytes, and full page payload, but not the checksum field.
Record pages contain an ordered key and binary value. Leaf pages contain
ordered keys and record page identifiers. Internal pages contain child page
identifiers followed by ordered separator keys. Keys are UTF 8 strings from
1 to 1024 bytes. Values are binary and limited by the page capacity.

Version 4 appends changed records and copy on write tree nodes. It synchronizes
the appended pages before writing and synchronizing the alternate commit
slot. Open validates candidate trees and referenced records before selecting
a generation. A writable open can clear a rejected commit slot and truncate
uncommitted tail pages. A snapshot reader selects a committed root and does
not modify the file. Writers use an exclusive operating system lock.

This ordering is tested with injected failures before publication and during
a partial commit slot write. It is not a general power loss guarantee.
Directory entry durability is not established, and pages are not reclaimed.

## Slotted page experiment

`slotted.rs` defines an in memory page with a `pspg` marker, a slot count, a
data boundary, four byte slot entries, and record bytes packed from the end
of the page. It validates bounds and overlap when decoding. It has no
checksum, durable page identifier, generation, or space compaction.

## Safety and compatibility

The checksums are for accidental corruption detection, not authentication.
The version 2 and version 3 formats have different recovery and locking
behavior. Version 4 adds page checksums and an indexed root but still has
limited crash guarantees. Never replace a database with another version in
place. A future migration should write a new file, verify it, and only then
allow an explicit user controlled replacement.

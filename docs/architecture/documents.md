# Document database status and architecture

There is no document collection API, JSON value type, nested path query,
projection, schema validation, or document index in the current implementation.
The key and value byte API does not provide these features.

## Intended shared core

Documents should be encoded values owned by the same page allocator,
transaction manager, and index implementation used by relational rows. A
collection can provide flexible fields and document identifiers, while SQL
continues to enforce its own schema and type rules. A SQL view over documents
should be explicit and tested rather than inferred from arbitrary JSON.

## Design sequence

Specify supported JSON value types and serialization first. Then define
collection metadata, identifier uniqueness, nested field paths, comparison
rules, filters, projections, insert, find, update, upsert, deletion,
validation, and transaction behavior. Add field indexes only when index
updates commit atomically with their documents.

Binary JSON should be selected only after benchmarks demonstrate a storage or
query advantage over a safe standard representation.

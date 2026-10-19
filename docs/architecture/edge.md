# Edge and platform architecture

The current implementation uses `std::fs::File`, positional seek operations,
file synchronization, and operating system locks. It is native file storage,
not an edge storage abstraction. There is no in memory backend, WASM build
target, Workers adapter, Durable Object integration, R2 adapter, or mobile
binding.

## Platform boundary

Keep query, record, transaction, and index logic independent of a local file
handle where practical. A storage adapter should make capabilities explicit:
read, write, synchronization, atomic replacement, locking, snapshot, and
durability. Do not report a commit as durable if the environment cannot make
that guarantee.

Native file storage should be made reliable first. An in memory backend can
exercise the same transaction logic without filesystem assumptions. WASM and
edge adapters should then specify whether storage belongs to browser APIs,
Workers bindings, a Durable Object, or a remote object store. R2 object
semantics are not a substitute for local random writes or file locks.

Networking, authentication, managed hosting, and dashboards are outside the
core engine.

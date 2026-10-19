# Development

## Requirements

Use a current stable Rust toolchain. The crate targets Rust edition 2024 and
currently has no external dependencies.

## Commands

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo run --bin passql -- info path.psl
```

## Safe storage work

Tests should create unique files in the operating system temporary directory.
Remove only files created by that test. Do not use project sample databases as
temporary test data.

The root `.gitignore` ignores every `.psl` file. Local files may contain
important data and do not appear in normal Git status. Inspect the file and
worktree before changing ignore rules or cleaning samples.

For format changes, add a golden header test, malformed input tests, a reopen
test, and a compatibility test for every older format. Exercise short writes,
sync failures, and restart recovery before changing durability claims. Never
silently migrate an existing file.

## Current layout

The crate keeps format specific code in `storage.rs`, `storage_v3.rs`,
`storage_v4.rs`, `btree.rs`, `transaction.rs`, and `reader.rs`. Version 4
uses copy on write pages so a transaction publishes records and its new index
root together. `slotted.rs` is still an in memory page implementation.
`format.rs` performs read only inspection and verification. The binary entry
point is `src/bin/passql.rs`.

## Contribution expectations

Keep the package, binary, modules, and database identity as `passql`. Use
`.psl` for the native extension. Preserve format meanings and experimental
files. Document new limits and compatibility effects. Do not add benchmarks
or ORM support claims without reproducible tests.

# Passql

## Stack

* Language and runtime: Rust, edition 2024
* Framework: none
* Dependencies: none
* Package manager: Cargo

## Build approach

<TBD, set by /scope>

## Commands

```sh
cargo build
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

## Specs

Stored in `docs/specs/` using `NNNN-title.md` names.

## Rules

* Use `passql` for packages, modules, identifiers, and binaries.
* Use `.psl` as the native database extension.
* Keep current format versions distinct. Never silently change their meaning.
* Keep inspection read only and avoid destructive recovery on reader paths.
* Add tests for malformed metadata, reopen behavior, and format compatibility.
* Preserve local sample databases. They are ignored by Git.
* Report unsupported behavior rather than implying SQL, document, index, or ORM support.
* Run tests, formatting, and Clippy after Rust changes.

## Agent skills

- [.agents/skills/mcp-builder/](.agents/skills/mcp-builder/): `anthropics/skills`, conventions for building a future MCP server.

MCP servers: none verified as compatible with Passql.

## Context files

<!-- Nested AGENTS.md files are listed here as they are created -->

_Drafted by /audit from the repo, worth a quick human pass. Edit freely: once a line stops matching this draft, later runs treat it as curated and will flag rather than overwrite it._

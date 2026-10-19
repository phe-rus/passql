# SQL status and architecture

There is no SQL tokenizer, parser, AST, planner, executor, schema catalog, or
SQL command in the current crate. The `query` command described in the project
vision does not yet exist. The key and value API is not a SQL interface.

## Intended implementation order

After storage, transactions, and indexes are reliable, use a maintained
parser that produces a structured AST. Start with a deliberately bounded
subset: table creation, inserts, selects, updates, deletes, predicates,
ordering, limits, basic types, primary keys, constraints, prepared parameters,
and explicit transactions.

Preserve the distinction between parse errors, binding errors, constraint
errors, execution errors, and storage errors. Never implement SQL by splitting
strings or regular expressions. Store JSON values in rows only after the value
representation and SQL type semantics are specified.

Join, grouping, aggregates, schema migration, and query optimization follow
the tested base. A SQLite like spelling does not make a Passql database
compatible with SQLite clients.

## Driver requirements

A future driver needs a stable statement lifecycle, binding rules, result
metadata, row iteration, affected row counts, error codes, transaction
boundaries, schema introspection, and migration behavior. Test these contracts
before advertising an ORM adapter.

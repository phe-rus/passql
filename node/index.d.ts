// TypeScript definitions for the passql Node.js binding.
// Mirrors better-sqlite3's synchronous API.

/** A bound SQL parameter value. */
export type SqlValue = string | number | bigint | Buffer | Uint8Array | null;

/** A row returned from a SELECT query, keyed by column name. */
export type Row = Record<string, SqlValue>;

/** A prepared SQL statement. Cheap to re-run with different parameters. */
export class Statement {
  /** Run the statement and return all result rows. */
  all(params: SqlValue[]): Row[];
  /** Run the statement and return the first row, or null. */
  get(params: SqlValue[]): Row | null;
  /** Run the statement and return the number of affected rows. */
  run(params: SqlValue[]): number;
}

/** A Passql database connection. Synchronous API like better-sqlite3. */
export class Database {
  /** Open (or create if missing) a Passql database at `path`. */
  constructor(path: string);

  /** Execute one or more SQL statements (no parameters, no result rows). */
  exec(sql: string): void;

  /** Prepare a SQL statement for repeated execution. */
  prepare(sql: string): Statement;

  /** Close the database handle. */
  close(): void;
}

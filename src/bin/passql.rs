use passql::document::{Collection, Filter};
use passql::format;
use passql::sql::{ExecutionResult, Value};
use passql::{IndexReader, Storage, VERSION};
use std::error::Error;

fn main() {
    if let Err(error) = run() {
        eprintln!("passql: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("create") => {
            let path = args.get(2).ok_or("missing database path")?;
            let db = Storage::create(path)?;
            println!("created: {path}");
            print_info(path, db.info());
        }

        Some("set") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let value = args.get(4).ok_or("missing value")?;

            let mut db = Storage::open(path)?;
            db.set(key, value.as_bytes())?;

            println!("committed: {key}");
            println!("generation: {}", db.generation());
        }

        Some("batch") => {
            let path = args.get(2).ok_or("missing database path")?;
            let entries = &args[3..];
            if entries.is_empty() || !entries.len().is_multiple_of(2) {
                return Err("expected key/value pairs: batch <file.psl> <key> <value> ...".into());
            }

            let mut db = Storage::open(path)?;
            let mut transaction = db.begin()?;
            for pair in entries.as_chunks::<2>().0 {
                transaction.set(&pair[0], pair[1].as_bytes())?;
            }
            let count = transaction.pending();
            let generation = transaction.commit()?;
            println!("committed {count} distinct keys");
            println!("generation: {generation}");
        }

        Some("lookup") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let mut db = IndexReader::open(path)?;
            match db.get(key)? {
                Some(value) => println!("{}", String::from_utf8(value)?),
                None => println!("key not found: {key}"),
            }
        }

        Some("delete") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let mut db = Storage::open(path)?;
            let previous_generation = db.generation();
            let generation = db.delete(key)?;
            if generation == previous_generation {
                println!("key not found: {key}");
            } else {
                println!("deleted: {key}");
                println!("generation: {generation}");
            }
        }

        Some("range") => {
            let path = args.get(2).ok_or("missing database path")?;
            let start = args.get(3).map(String::as_str);
            let end = args.get(4).map(String::as_str);
            let mut db = IndexReader::open(path)?;
            for (key, value) in db.range(start, end)? {
                println!("{key}\t{}", String::from_utf8(value)?);
            }
        }

        Some("info") => {
            let path = args.get(2).ok_or("missing database path")?;
            print_info(path, format::inspect(path)?);
        }

        Some("verify") => {
            let path = args.get(2).ok_or("missing database path")?;
            let report = format::verify(path)?;

            println!("passql integrity verification");
            println!("database: {path}");
            print_info(path, report.info);
            println!("scanned pages: {}", report.pages_scanned);
            println!("verified records: {}", report.records_verified);
            if report.pages_without_record_checksum > 0 {
                println!(
                    "pages without record checksums: {}",
                    report.pages_without_record_checksum
                );
            }
            println!("result: structural checks passed");
        }

        Some("compact") => {
            // passql compact <source.psl> <destination.psl>
            // Rewrite the source database to destination, removing dead pages
            // left behind by updates and deletes. The source is not modified.
            let source = args.get(2).ok_or("missing source database path")?;
            let dest = args.get(3).ok_or("missing destination database path")?;
            let database = Storage::open(source)?;
            let pages_before = database.page_count();
            let pages_after = database.compact_to(dest)?;
            println!("compacted: {source} -> {dest}");
            println!("pages: {pages_before} -> {pages_after}");
            let saved = pages_before.saturating_sub(pages_after);
            if saved > 0 {
                let saved_kb = (saved * passql::PAGE_SIZE as u64) / 1024;
                println!("reclaimed: {saved} pages ({saved_kb} KB)");
            } else {
                println!("(no dead pages to reclaim)");
            }
        }

        Some("query") => {
            let path = args.get(2).ok_or("missing database path")?;
            let sql = args.get(3).ok_or("missing SQL statement")?;
            let mut db = Storage::open(path)?;
            let result = passql::sql::execute(sql, &mut db)?;
            print_query_result(&result);
        }

        Some("exec") => {
            // Execute multiple statements from a SQL file or stdin.
            let path = args.get(2).ok_or("missing database path")?;
            let sql = if let Some(file) = args.get(3) {
                std::fs::read_to_string(file)?
            } else {
                use std::io::Read;
                let mut buffer = String::new();
                std::io::stdin().read_to_string(&mut buffer)?;
                buffer
            };
            let mut db = Storage::open(path)?;
            // Split on semicolons (simple split — no string-literal awareness yet).
            for statement in sql.split(';') {
                let trimmed = statement.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let result = passql::sql::execute(trimmed, &mut db)?;
                print_query_result(&result);
            }
        }

        Some("doc") => {
            // Document collection commands (subcommand-first, like git):
            //   passql doc create    <file.psl> <coll>
            //   passql doc insert    <file.psl> <coll> <json>
            //   passql doc get       <file.psl> <coll> <id>
            //   passql doc list      <file.psl> <coll>
            //   passql doc find      <file.psl> <coll> <filter-json>
            //   passql doc patch     <file.psl> <coll> <id> <path> <json>
            //   passql doc delete    <file.psl> <coll> <id>
            //   passql doc drop      <file.psl> <coll>
            let subcommand = args
                .get(2)
                .map(String::as_str)
                .ok_or("missing doc subcommand")?;
            let path = args.get(3).ok_or("missing database path")?;
            let coll_name = args.get(4).ok_or("missing collection name")?;
            let mut db = open_or_create_storage(path)?;
            match subcommand {
                "create" => {
                    Collection::create(&mut db, coll_name)?;
                    println!("collection {coll_name} created");
                }
                "insert" => {
                    let json_str = args.get(5).ok_or("missing JSON document")?;
                    let value: serde_json::Value = serde_json::from_str(json_str)?;
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    let id = coll.insert(&value)?;
                    println!("inserted id {id}");
                }
                "get" => {
                    let id: u64 = args.get(5).ok_or("missing id")?.parse()?;
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    match coll.get(id)? {
                        Some(doc) => println!("{}", serde_json::to_string_pretty(&doc)?),
                        None => println!("not found"),
                    }
                }
                "list" => {
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    for (id, doc) in coll.list()? {
                        println!("{id}\t{}", serde_json::to_string(&doc)?);
                    }
                }
                "find" => {
                    let filter_str = args.get(5).ok_or("missing filter JSON")?;
                    let filter_value: serde_json::Value = serde_json::from_str(filter_str)?;
                    let filter = Filter::parse(&filter_value)?;
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    let matches = coll.find(&filter)?;
                    println!("({} matches)", matches.len());
                    for (id, doc) in matches {
                        println!("{id}\t{}", serde_json::to_string(&doc)?);
                    }
                }
                "patch" => {
                    let id: u64 = args.get(5).ok_or("missing id")?.parse()?;
                    let path_str = args.get(6).ok_or("missing path")?;
                    let json_str = args.get(7).ok_or("missing value JSON")?;
                    let value: serde_json::Value = serde_json::from_str(json_str)?;
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    match coll.patch(id, &[(passql::DocumentPath::parse(path_str), value)])? {
                        Some(updated) => println!("{}", serde_json::to_string_pretty(&updated)?),
                        None => println!("not found"),
                    }
                }
                "delete" => {
                    let id: u64 = args.get(5).ok_or("missing id")?.parse()?;
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    if coll.delete(id)? {
                        println!("deleted id {id}");
                    } else {
                        println!("not found");
                    }
                }
                "drop" => {
                    let mut coll = Collection::open(&mut db, coll_name)?;
                    if coll.drop_collection()? {
                        println!("collection {coll_name} dropped");
                    } else {
                        println!("collection {coll_name} not found");
                    }
                }
                other => {
                    return Err(format!("unknown doc subcommand: {other}").into());
                }
            }
        }

        _ => {
            println!("Passql v{VERSION}");
            println!("embedded, edge-native database engine\n");
            println!("usage:");
            println!("  passql create  <file.psl>");
            println!("  passql set     <file.psl> <key> <value>");
            println!("  passql batch   <file.psl> <key> <value> ...");
            println!("  passql lookup  <file.psl> <key>");
            println!("  passql delete  <file.psl> <key>");
            println!("  passql range   <file.psl> [start] [end]");
            println!("  passql info    <file.psl>");
            println!("  passql verify  <file.psl>");
            println!("  passql compact <src.psl> <dst.psl>  # remove dead pages");
            println!();
            println!("  passql query   <file.psl> <sql>     # execute one SQL statement");
            println!("  passql exec    <file.psl> <file.sql> # execute a SQL script");
            println!();
            println!("  passql doc create <file.psl> <coll>                  # create collection");
            println!("  passql doc insert <file.psl> <coll> <json>           # insert document");
            println!("  passql doc get    <file.psl> <coll> <id>             # fetch by id");
            println!("  passql doc list   <file.psl> <coll>                 # list all");
            println!("  passql doc find   <file.psl> <coll> <filter-json>    # find by filter");
            println!("  passql doc patch  <file.psl> <coll> <id> <path> <v>  # patch field");
            println!("  passql doc delete <file.psl> <coll> <id>             # delete by id");
            println!("  passql doc drop   <file.psl> <coll>                  # drop collection");
            println!();
            println!("upcoming:");
            println!("  passql serve   # stage 5 — wire protocol server");
        }
    }

    Ok(())
}

fn print_info(path: &str, info: passql::DatabaseInfo) {
    println!("passql database: {path}");
    println!("format: {}", info.format.version());
    println!("pages: {}", info.page_count);
    println!("page size: {} bytes", info.page_size);
    if let Some(generation) = info.generation {
        println!("generation: {generation}");
    }
}

fn print_query_result(result: &ExecutionResult) {
    match result {
        ExecutionResult::SchemaChanged { message } => println!("{message}"),
        ExecutionResult::Inserted { rows_affected } => {
            println!("inserted {rows_affected} row(s)");
        }
        ExecutionResult::Updated { rows_affected } => {
            println!("updated {rows_affected} row(s)");
        }
        ExecutionResult::Deleted { rows_affected } => {
            println!("deleted {rows_affected} row(s)");
        }
        ExecutionResult::Selected { columns, rows } => {
            if rows.is_empty() {
                println!("(no rows)");
                return;
            }
            // Header row
            println!("{}", columns.join("\t"));
            for row in rows {
                let cells: Vec<String> = row.iter().map(format_value).collect();
                println!("{}", cells.join("\t"));
            }
            println!("({} row(s))", rows.len());
        }
    }
}

fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => s.clone(),
        Value::Blob(bytes) => format!("blob[{}]", bytes.len()),
    }
}

/// Open an existing Passql database, or create one if it does not exist.
/// Used by the `doc` subcommand so users don't have to `passql create`
/// before their first document operation.
fn open_or_create_storage(path: &str) -> Result<Storage, Box<dyn Error>> {
    if std::path::Path::new(path).exists() {
        Ok(Storage::open(path)?)
    } else {
        Ok(Storage::create(path)?)
    }
}

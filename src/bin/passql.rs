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
            println!();
            println!("  passql query   <file.psl> <sql>     # execute one SQL statement");
            println!("  passql exec    <file.psl> <file.sql> # execute a SQL script");
            println!();
            println!("upcoming:");
            println!("  passql doc     <file.psl> <collection> ...  # stage 3 — documents");
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

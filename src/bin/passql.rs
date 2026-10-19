use passql::storage_v3::StorageV3;
use passql::{DatabaseInfo, DatabaseReader, Record, Storage, VERSION, format};
use passql::{IndexReaderV4, StorageV4};
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
            println!("pages: {}", db.page_count());
        }

        Some("put") => {
            let path = args.get(2).ok_or("missing database path")?;
            let text = args.get(3).ok_or("missing record data")?;

            let mut db = Storage::open(path)?;
            let id = db.append_record(&Record::new(text.as_bytes()))?;

            println!("record stored at page {id}");
        }

        Some("get") => {
            let path = args.get(2).ok_or("missing database path")?;
            let id: u64 = args.get(3).ok_or("missing record id")?.parse()?;

            let mut db = Storage::open(path)?;
            let record = db.read_record(id)?;

            println!("{}", String::from_utf8(record.data)?);
        }

        Some("set") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let value = args.get(4).ok_or("missing value")?;

            let mut db = Storage::open(path)?;
            db.set(key, value.as_bytes())?;

            println!("stored: {key}");
        }

        Some("lookup") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;

            let mut db = Storage::open(path)?;

            match db.lookup(key)? {
                Some(value) => println!("{}", String::from_utf8(value)?),
                None => println!("key not found: {key}"),
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

        Some("create3") => {
            let path = args.get(2).ok_or("missing database path")?;
            let db = StorageV3::create(path)?;

            println!("created: {path}");
            println!("format: 3");
            println!("generation: {}", db.generation());
        }

        Some("set3") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let value = args.get(4).ok_or("missing value")?;

            let mut db = StorageV3::open(path)?;
            let mut tx = db.begin();

            tx.set(key, value.as_bytes())?;
            tx.commit()?;

            println!("committed: {key}");
            println!("generation: {}", db.generation());
        }

        Some("batch3") => {
            let path = args.get(2).ok_or("missing database path")?;
            let entries = &args[3..];

            if entries.is_empty() || !entries.len().is_multiple_of(2) {
                return Err("expected key/value pairs: batch3 <file.psl> <key> <value> ...".into());
            }

            let mut db = StorageV3::open(path)?;
            let mut tx = db.begin();

            for pair in entries.chunks_exact(2) {
                tx.set(&pair[0], pair[1].as_bytes())?;
            }

            let count = tx.pending();
            tx.commit()?;

            println!("committed {count} records");
            println!("generation: {}", db.generation());
        }

        Some("lookup3") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;

            let mut db = DatabaseReader::open(path)?;

            match db.lookup(key)? {
                Some(value) => println!("{}", String::from_utf8(value)?),
                None => println!("key not found: {key}"),
            }
        }

        Some("info3") => {
            let path = args.get(2).ok_or("missing database path")?;
            let db = DatabaseReader::open(path)?;

            println!("passql database: {path}");
            println!("format: 3");
            println!("generation: {}", db.generation());
            println!("pages: {}", db.page_count());
        }

        Some("create4") => {
            let path = args.get(2).ok_or("missing database path")?;
            let db = StorageV4::create(path)?;
            println!("created: {path}");
            print_info(path, db.info());
        }

        Some("set4") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let value = args.get(4).ok_or("missing value")?;
            let mut db = StorageV4::open(path)?;
            db.set(key, value.as_bytes())?;
            println!("committed: {key}");
            println!("generation: {}", db.generation());
        }

        Some("batch4") => {
            let path = args.get(2).ok_or("missing database path")?;
            let entries = &args[3..];
            if entries.is_empty() || !entries.len().is_multiple_of(2) {
                return Err("expected key/value pairs: batch4 <file.psl> <key> <value> ...".into());
            }

            let mut db = StorageV4::open(path)?;
            let mut transaction = db.begin()?;
            for pair in entries.chunks_exact(2) {
                transaction.set(&pair[0], pair[1].as_bytes())?;
            }
            let count = transaction.pending();
            let generation = transaction.commit()?;
            println!("committed {count} distinct keys");
            println!("generation: {generation}");
        }

        Some("lookup4") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let mut db = IndexReaderV4::open(path)?;
            match db.get(key)? {
                Some(value) => println!("{}", String::from_utf8(value)?),
                None => println!("key not found: {key}"),
            }
        }

        Some("delete4") => {
            let path = args.get(2).ok_or("missing database path")?;
            let key = args.get(3).ok_or("missing key")?;
            let mut db = StorageV4::open(path)?;
            let previous_generation = db.generation();
            let generation = db.delete(key)?;
            if generation == previous_generation {
                println!("key not found: {key}");
            } else {
                println!("deleted: {key}");
                println!("generation: {generation}");
            }
        }

        Some("range4") => {
            let path = args.get(2).ok_or("missing database path")?;
            let start = args.get(3).map(String::as_str);
            let end = args.get(4).map(String::as_str);
            let mut db = IndexReaderV4::open(path)?;
            for (key, value) in db.range(start, end)? {
                println!("{key}\t{}", String::from_utf8(value)?);
            }
        }

        _ => {
            println!("Passql v{VERSION}");
            println!("embedded database engine\n");
            println!("usage:");
            println!("  passql create <file.psl>");
            println!("  passql put    <file.psl> <text>");
            println!("  passql get    <file.psl> <page>");
            println!("  passql set    <file.psl> <key> <value>");
            println!("  passql lookup <file.psl> <key>");
            println!("  passql info   <file.psl>");
            println!("  passql verify <file.psl>");
            println!("\nexperimental format v3:");
            println!("  passql create3 <file.psl>");
            println!("  passql set3 <file.psl> <key> <value>");
            println!("  passql batch3 <file.psl> <key> <value> ...");
            println!("  passql lookup3 <file.psl> <key>");
            println!("  passql info3 <file.psl>");
            println!("\nversion 4 persistent index:");
            println!("  passql create4 <file.psl>");
            println!("  passql set4 <file.psl> <key> <value>");
            println!("  passql batch4 <file.psl> <key> <value> ...");
            println!("  passql lookup4 <file.psl> <key>");
            println!("  passql delete4 <file.psl> <key>");
            println!("  passql range4 <file.psl> [start] [end]");
        }
    }

    Ok(())
}

fn print_info(path: &str, info: DatabaseInfo) {
    println!("passql database: {path}");
    println!("format: {}", info.format.version());
    println!("pages: {}", info.page_count);
    println!("page size: {} bytes", info.page_size);
    if let Some(generation) = info.generation {
        println!("generation: {generation}");
    }
}

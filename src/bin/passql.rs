use passql::{Record, Storage, VERSION};
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
            let db = Storage::open(path)?;

            println!("passql database: {path}");
            println!("pages: {}", db.page_count());
            println!("page size: {} bytes", passql::PAGE_SIZE);
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
        }
    }

    Ok(())
}

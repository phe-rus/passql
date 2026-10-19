use passql::{Storage, VERSION};
use std::error::Error;

fn main() {
    if let Err(error) = run() {
        eprintln!("passql error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("create") => {
            let path = args.get(2).ok_or("Missing database path")?;
            let db = Storage::create(path)?;

            println!("Created passql database: {path}");
            println!("Pages: {}", db.page_count());
        }

        Some("append") => {
            let path = args.get(2).ok_or("Missing database path")?;
            let data = args.get(3).ok_or("Missing data")?;
            let mut db = Storage::open(path)?;

            let page = db.append_page(data.as_bytes())?;
            println!("Stored data in page {page}");
        }

        Some("read") => {
            let path = args.get(2).ok_or("Missing database path")?;
            let page: u64 = args.get(3).ok_or("Missing page number")?.parse()?;
            let mut db = Storage::open(path)?;

            let data = db.read_page(page)?;
            let end = data
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(data.len());

            println!("{}", String::from_utf8_lossy(&data[..end]));
        }

        _ => {
            println!("passql v{VERSION}");
            println!("A modern, embedded database engine.\n");
            println!("Usage:");
            println!("  passql create <database>");
            println!("  passql append <database> <text>");
            println!("  passql read   <database> <page>");
        }
    }

    Ok(())
}

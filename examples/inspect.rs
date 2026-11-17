use std::{env, error::Error};

use hdoc_decoder::HdocDocument;

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args_os().nth(1).ok_or("usage: inspect <input.hdoc>")?;
    let document = HdocDocument::open(path)?;
    println!(
        "pages={} assets={}",
        document.page_count(),
        document.asset_count()
    );
    if let Some(title) = document.metadata().get("title") {
        println!("title={title}");
    }
    if document.page_count() > 0 {
        let page = document.page(0)?;
        println!(
            "first_page={}x{} commands={}",
            page.width,
            page.height,
            page.commands.len()
        );
        if let Some(text) = document.page_text(0)? {
            println!("first_page_text_bytes={}", text.len());
        }
    }
    Ok(())
}

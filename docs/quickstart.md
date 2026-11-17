# Quick start

Add the crate from GitHub:

```toml
[dependencies]
hdoc-decoder = { git = "https://github.com/younsabood/hdoc-decoder" }
```

Open a file, request one page, and search its text:

```rust,no_run
use hdoc_decoder::HdocDocument;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let document = HdocDocument::open("example.hdoc")?;
    let page = document.page(0)?;
    println!("{} pages; first is {} x {}", document.page_count(), page.width, page.height);

    if let Some(text) = document.page_text(0)? {
        println!("page text: {text}");
    }
    for result in document.search("hyperdoc")? {
        println!("found on page {}", result.page_id);
    }
    Ok(())
}
```

For in-memory bytes, use `HdocDocument::from_bytes`. A custom source can
implement `ByteSource` and be supplied to `HdocDocument::from_source`.

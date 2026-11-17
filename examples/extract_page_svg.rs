use std::{env, error::Error, fs, path::PathBuf};

use hdoc_decoder::{HdocDocument, PageCommand};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let input = args
        .next()
        .ok_or("usage: extract_page_svg <input.hdoc> <page-index> <output.svg>")?;
    let page_index = args
        .next()
        .ok_or("usage: extract_page_svg <input.hdoc> <page-index> <output.svg>")?
        .to_string_lossy()
        .parse::<u32>()?;
    let output = args
        .next()
        .ok_or("usage: extract_page_svg <input.hdoc> <page-index> <output.svg>")?;

    let document = HdocDocument::open(PathBuf::from(input))?;
    let page = document.page(page_index)?;
    let svg = page
        .commands
        .iter()
        .find_map(|command| match command {
            PageCommand::DrawSvgFallback { svg } => Some(svg),
            _ => None,
        })
        .ok_or("page has no SVG vector display list")?;
    fs::write(output, svg)?;
    Ok(())
}

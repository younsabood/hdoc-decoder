use std::{collections::HashMap, env, error::Error, fs};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use hdoc_decoder::{
    format::{CentralIndex, FileHeader, END_MARKER, HEADER_SIZE},
    opcodes::{decode_page, PageCommand},
};

#[derive(Default)]
struct AssetTotals {
    count: usize,
    bytes: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let paths = env::args_os().skip(1).collect::<Vec<_>>();
    if paths.is_empty() {
        return Err("usage: size_breakdown <file.hdoc> [file.hdoc ...]".into());
    }

    println!("| file | file bytes | AVIF assets (count / bytes) | inline AVIF refs / raw bytes | vector paths (count / bytes) | path dictionary (count / bytes) | other assets (count / bytes) | path refs / AVIF asset refs | page chunks (compressed / raw) | search (compressed / raw) | index (compressed / raw) | header + trailer |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");

    for path in paths {
        let bytes = fs::read(&path)?;
        let header = FileHeader::decode(&bytes[..HEADER_SIZE])?;
        let footer_start = usize::try_from(header.footer_offset)?;
        let footer_end = footer_start
            .checked_add(header.footer_size as usize)
            .ok_or("footer range overflow")?;
        let raw_index =
            zstd::bulk::decompress(&bytes[footer_start..footer_end], 128 * 1024 * 1024)?;
        let index = CentralIndex::decode(&raw_index)?;

        let mut avif = AssetTotals::default();
        let mut vector_paths = AssetTotals::default();
        let mut vector_dictionaries = AssetTotals::default();
        let mut other = AssetTotals::default();
        for asset in &index.assets {
            let totals = if asset.media_type.eq_ignore_ascii_case("image/avif") {
                &mut avif
            } else if asset.media_type == "application/vnd.hyperdoc.vector-dictionary;version=1" {
                &mut vector_dictionaries
            } else if asset
                .media_type
                .starts_with("application/vnd.hyperdoc.vector-path")
            {
                &mut vector_paths
            } else {
                &mut other
            };
            totals.count += 1;
            totals.bytes = totals.bytes.saturating_add(asset.length);
        }

        let mut decoded_chunks = HashMap::<u32, Vec<u8>>::new();
        let mut inline_avif_references = 0usize;
        let mut inline_avif_raw_bytes = 0u64;
        let mut asset_references = 0usize;
        let mut vector_path_references = 0usize;
        for page in &index.pages {
            let chunk_record = &index.chunks[page.chunk_id as usize];
            let raw_chunk = decoded_chunks.entry(page.chunk_id).or_insert_with(|| {
                let start = chunk_record.offset as usize;
                let end = start + chunk_record.compressed_size as usize;
                zstd::bulk::decompress(&bytes[start..end], chunk_record.uncompressed_size as usize)
                    .expect("page chunk decompresses")
            });
            let (display_list, _) = decode_page(
                &raw_chunk[page.offset_in_chunk as usize..],
                page.width,
                page.height,
                page.flags,
            )?;
            for command in &display_list.commands {
                if let PageCommand::DrawSvgTemplate { glyphs, .. } = command {
                    vector_path_references += glyphs.len();
                }
                let svg = match command {
                    PageCommand::DrawSvgFallback { svg }
                    | PageCommand::DrawSvgTemplate { svg, .. } => svg.as_str(),
                    _ => continue,
                };
                asset_references += svg.matches("hdoc-asset://").count();
                inline_avif_references += svg.matches("data:image/avif;base64,").count();
                inline_avif_raw_bytes =
                    inline_avif_raw_bytes.saturating_add(inline_avif_bytes(svg)?);
            }
        }

        let compressed_chunks = index
            .chunks
            .iter()
            .map(|chunk| u64::from(chunk.compressed_size))
            .sum::<u64>();
        let raw_chunks = index
            .chunks
            .iter()
            .map(|chunk| u64::from(chunk.uncompressed_size))
            .sum::<u64>();
        let search = index.search_block;
        let path = path.to_string_lossy();
        println!(
            "| {} | {} | {} / {} | {} / {} | {} / {} | {} / {} | {} / {} | {} / {} | {} / {} | {} / {} | {} / {} | {} |",
            path,
            bytes.len(),
            avif.count,
            avif.bytes,
            inline_avif_references,
            inline_avif_raw_bytes,
            vector_paths.count,
            vector_paths.bytes,
            vector_dictionaries.count,
            vector_dictionaries.bytes,
            other.count,
            other.bytes,
            vector_path_references,
            asset_references,
            compressed_chunks,
            raw_chunks,
            search.compressed_size,
            search.uncompressed_size,
            header.footer_size,
            raw_index.len(),
            HEADER_SIZE + END_MARKER.len(),
        );
    }
    Ok(())
}

fn inline_avif_bytes(svg: &str) -> Result<u64, Box<dyn Error>> {
    let marker = "data:image/avif;base64,";
    let mut remaining = svg;
    let mut total = 0u64;
    while let Some(marker_start) = remaining.find(marker) {
        let encoded = &remaining[marker_start + marker.len()..];
        let encoded_end = encoded.find(['\'', '"']).unwrap_or(encoded.len());
        let decoded = STANDARD.decode(&encoded[..encoded_end])?;
        total = total.saturating_add(decoded.len() as u64);
        remaining = &encoded[encoded_end..];
    }
    Ok(total)
}

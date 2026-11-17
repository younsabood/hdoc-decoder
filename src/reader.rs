use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::{
    cache::ChunkCache,
    error::DecoderError,
    format::{
        CentralIndex, FileHeader, END_MARKER, FLAG_ARCHIVAL, FLAG_ENCRYPTED, FLAG_HAS_SEARCH_INDEX,
        HEADER_SIZE,
    },
    opcodes::{decode_page, PageCommand, PageDisplayList, SvgGlyphStyle, SvgGlyphUse},
    search::{tokenize, SearchBlock},
    vector_path::decode_path_asset,
    ByteSource, MemorySource, MmapSource,
};

const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
const MAX_SEARCH_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPRESSED_CHUNK_BYTES: usize = 65 * 1024 * 1024;
const MAX_COMPRESSED_SEARCH_BYTES: usize = 65 * 1024 * 1024;
const MAX_COMPRESSED_INDEX_BYTES: usize = 129 * 1024 * 1024;
const DEFAULT_CACHE_CHUNKS: usize = 4;
const VECTOR_PATH_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path;version=1";
const VECTOR_PATH_ZSTD_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path+zstd;version=1";
const VECTOR_PATH_ZSTD_DICTIONARY_MEDIA_TYPE: &str =
    "application/vnd.hyperdoc.vector-path+zstd-dict;version=1";
const VECTOR_DICTIONARY_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-dictionary;version=1";
const SVG_GLYPH_MARKER: &str = "<!--HDOC_GLYPH-->";
const SVG_ASSET_URI_PREFIX: &str = "hdoc-asset://";
const AVIF_DATA_URI_PREFIX: &str = "data:image/avif;base64,";

fn svg_path_element(path: &str, style: &SvgGlyphStyle, x: i64, y: i64) -> String {
    let x = x as f64 / 64.0;
    let y = y as f64 / 64.0;
    format!(
        "<path transform=\"matrix({} {} {} {} {x} {y})\" {} d=\"{path}\"/>",
        style.matrix[0], style.matrix[1], style.matrix[2], style.matrix[3], style.presentation
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub page_id: u32,
    pub matched_terms: usize,
}

/// Open HyperDoc reader. Page lookup is direct through the directory, then a
/// single independent chunk is decompressed and reused from the bounded cache.
pub struct HdocDocument<S: ByteSource> {
    source: S,
    header: FileHeader,
    index: CentralIndex,
    search_block: Option<SearchBlock>,
    chunk_cache: Mutex<ChunkCache>,
}

impl HdocDocument<MmapSource> {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DecoderError> {
        Self::from_source(MmapSource::open(path)?)
    }
}

impl HdocDocument<MemorySource> {
    pub fn from_bytes(bytes: impl Into<Arc<[u8]>>) -> Result<Self, DecoderError> {
        Self::from_source(MemorySource::new(bytes))
    }
}

impl<S: ByteSource> HdocDocument<S> {
    pub fn from_source(source: S) -> Result<Self, DecoderError> {
        Self::from_source_with_cache(source, DEFAULT_CACHE_CHUNKS)
    }

    pub fn from_source_with_cache(source: S, cache_chunks: usize) -> Result<Self, DecoderError> {
        if source.len() < (HEADER_SIZE + END_MARKER.len()) as u64 {
            return Err(DecoderError::Corrupt("file is too short".into()));
        }
        let header_bytes = source.read_slice(0, HEADER_SIZE)?;
        let header = FileHeader::decode(&header_bytes)?;
        if header.flags & FLAG_ENCRYPTED != 0 {
            return Err(DecoderError::UnsupportedFeature("encrypted documents"));
        }
        if header.flags & !(FLAG_ARCHIVAL | FLAG_HAS_SEARCH_INDEX) != 0 {
            return Err(DecoderError::UnsupportedFeature("unknown header flags"));
        }
        if header.footer_size as usize > MAX_COMPRESSED_INDEX_BYTES {
            return Err(DecoderError::DecompressionLimit);
        }
        let footer_start = header.footer_offset;
        let footer_end = footer_start
            .checked_add(u64::from(header.footer_size))
            .ok_or_else(|| DecoderError::Corrupt("footer range overflows".into()))?;
        let expected_end = footer_end
            .checked_add(END_MARKER.len() as u64)
            .ok_or_else(|| DecoderError::Corrupt("end marker range overflows".into()))?;
        if footer_start < HEADER_SIZE as u64 || expected_end != source.len() {
            return Err(DecoderError::Corrupt(
                "footer or end marker is outside the file".into(),
            ));
        }
        let footer = source.read_slice(footer_start, header.footer_size as usize)?;
        let marker = source.read_slice(footer_end, END_MARKER.len())?;
        if marker.as_ref() != END_MARKER {
            return Err(DecoderError::Corrupt("missing HDOC end marker".into()));
        }
        let raw_index = decompress_bounded(&footer, MAX_INDEX_BYTES, MAX_INDEX_BYTES)?;
        let index = CentralIndex::decode(&raw_index)?;
        validate_index(&header, &index)?;
        let has_search_flag = header.flags & FLAG_HAS_SEARCH_INDEX != 0;
        let has_search_record = index.search_block.compressed_size > 0;
        if has_search_flag != has_search_record {
            return Err(DecoderError::Corrupt(
                "search flag and search block record disagree".into(),
            ));
        }
        let search_block = if has_search_record {
            let record = index.search_block;
            if record.uncompressed_size as usize > MAX_SEARCH_BLOCK_BYTES
                || record.compressed_size as usize > MAX_COMPRESSED_SEARCH_BYTES
            {
                return Err(DecoderError::DecompressionLimit);
            }
            let compressed = source.read_slice(record.offset, record.compressed_size as usize)?;
            let raw = decompress_bounded(
                &compressed,
                record.uncompressed_size as usize,
                MAX_SEARCH_BLOCK_BYTES,
            )?;
            if raw.len() != record.uncompressed_size as usize {
                return Err(DecoderError::Corrupt(
                    "search block has an unexpected uncompressed size".into(),
                ));
            }
            let block = SearchBlock::decode(&raw)?;
            if block.page_texts.len() != header.page_count as usize {
                return Err(DecoderError::Corrupt(
                    "search block page count does not match header".into(),
                ));
            }
            if block
                .postings
                .values()
                .flatten()
                .any(|page| *page >= header.page_count)
            {
                return Err(DecoderError::Corrupt(
                    "search index references a missing page".into(),
                ));
            }
            Some(block)
        } else {
            None
        };
        Ok(Self {
            source,
            header,
            index,
            search_block,
            chunk_cache: Mutex::new(ChunkCache::new(cache_chunks)),
        })
    }

    pub fn page_count(&self) -> u32 {
        self.header.page_count
    }
    pub fn header(&self) -> FileHeader {
        self.header
    }
    pub fn metadata(&self) -> &std::collections::BTreeMap<String, String> {
        &self.index.metadata
    }
    pub fn asset_count(&self) -> u32 {
        self.header.asset_count
    }

    /// Original UTF-8 text for a page, when the file includes a search block.
    pub fn page_text(&self, page_id: u32) -> Result<Option<&str>, DecoderError> {
        if page_id >= self.header.page_count {
            return Err(DecoderError::PageOutOfRange(page_id));
        }
        Ok(self
            .search_block
            .as_ref()
            .and_then(|block| block.page_text(page_id)))
    }

    pub fn page(&self, page_id: u32) -> Result<PageDisplayList, DecoderError> {
        let record = self
            .index
            .pages
            .get(page_id as usize)
            .ok_or(DecoderError::PageOutOfRange(page_id))?;
        let chunk_record = self
            .index
            .chunks
            .get(record.chunk_id as usize)
            .ok_or_else(|| DecoderError::Corrupt("page references a missing chunk".into()))?;
        let next_offset = self.index.pages.get(page_id as usize + 1).and_then(|next| {
            (next.chunk_id == record.chunk_id).then_some(next.offset_in_chunk as usize)
        });
        let cached = self
            .chunk_cache
            .lock()
            .map_err(|_| DecoderError::Corrupt("chunk cache lock is poisoned".into()))?
            .get(record.chunk_id);
        let chunk = match cached {
            Some(data) => data,
            None => {
                let compressed = self
                    .source
                    .read_slice(chunk_record.offset, chunk_record.compressed_size as usize)?;
                let raw = decompress_bounded(
                    &compressed,
                    chunk_record.uncompressed_size as usize,
                    MAX_CHUNK_BYTES,
                )?;
                if raw.len() != chunk_record.uncompressed_size as usize {
                    return Err(DecoderError::Corrupt(
                        "chunk has an unexpected uncompressed size".into(),
                    ));
                }
                let raw: Arc<[u8]> = raw.into();
                self.chunk_cache
                    .lock()
                    .map_err(|_| DecoderError::Corrupt("chunk cache lock is poisoned".into()))?
                    .insert(record.chunk_id, Arc::clone(&raw));
                raw
            }
        };
        let start = record.offset_in_chunk as usize;
        let end = next_offset.unwrap_or(chunk.len());
        let data = chunk.get(start..end).ok_or_else(|| {
            DecoderError::Corrupt("page offset exceeds decompressed chunk".into())
        })?;
        let (page, consumed) = decode_page(data, record.width, record.height, record.flags)?;
        if consumed != data.len() {
            return Err(DecoderError::Corrupt(
                "page directory range contains trailing bytes".into(),
            ));
        }
        let mut page = page;
        self.materialize_svg_templates(&mut page)?;
        Ok(page)
    }

    fn materialize_svg_templates(&self, page: &mut PageDisplayList) -> Result<(), DecoderError> {
        for command in &mut page.commands {
            let expanded = match command {
                PageCommand::DrawSvgTemplate {
                    svg,
                    glyph_styles,
                    glyphs,
                } => Some(self.expand_svg_template(svg, glyph_styles, glyphs)?),
                PageCommand::DrawSvgFallback { svg } if svg.contains(SVG_ASSET_URI_PREFIX) => {
                    Some(svg.clone())
                }
                _ => None,
            };
            if let Some(svg) = expanded {
                *command = PageCommand::DrawSvgFallback {
                    svg: self.expand_svg_asset_references(&svg)?,
                };
            }
        }
        Ok(())
    }

    fn expand_svg_asset_references(&self, svg: &str) -> Result<String, DecoderError> {
        let mut output = String::with_capacity(svg.len());
        let mut remaining = svg;
        let mut encoded_assets = HashMap::<u32, String>::new();

        while let Some(reference_start) = remaining.find(SVG_ASSET_URI_PREFIX) {
            let id_start = reference_start + SVG_ASSET_URI_PREFIX.len();
            let reference_tail = &remaining[id_start..];
            let reference_end = reference_tail
                .find(['"', '\'', ')', ' ', '\t', '\r', '\n'])
                .unwrap_or(reference_tail.len());
            let asset_id = reference_tail[..reference_end]
                .parse::<u32>()
                .map_err(|_| DecoderError::Corrupt("invalid SVG asset reference".into()))?;
            if let std::collections::hash_map::Entry::Vacant(entry) = encoded_assets.entry(asset_id)
            {
                let record = self
                    .index
                    .assets
                    .get(asset_id as usize)
                    .filter(|asset| asset.id == asset_id)
                    .ok_or(DecoderError::AssetNotFound(asset_id))?;
                if record.media_type != "image/avif" {
                    return Err(DecoderError::Corrupt(format!(
                        "asset {asset_id} is not an AVIF image"
                    )));
                }
                let bytes = self.asset_bytes(asset_id)?;
                entry.insert(STANDARD.encode(&bytes));
            }
            let encoded = encoded_assets
                .get(&asset_id)
                .expect("AVIF asset has just been inserted into the page cache");

            output.push_str(&remaining[..reference_start]);
            output.push_str(AVIF_DATA_URI_PREFIX);
            output.push_str(encoded);
            remaining = &reference_tail[reference_end..];
        }
        output.push_str(remaining);
        Ok(output)
    }

    fn expand_svg_template(
        &self,
        template: &str,
        styles: &[SvgGlyphStyle],
        glyphs: &[SvgGlyphUse],
    ) -> Result<String, DecoderError> {
        if template.matches(SVG_GLYPH_MARKER).count() != glyphs.len() {
            return Err(DecoderError::Corrupt(
                "SVG glyph template marker count does not match glyph table".into(),
            ));
        }
        let estimated_paths = template
            .len()
            .saturating_add(glyphs.len().saturating_mul(96));
        let mut output = String::with_capacity(estimated_paths);
        let mut path_cache = HashMap::<u32, String>::new();
        let mut x = 0i64;
        let mut y = 0i64;

        for (marker_index, segment) in template.split(SVG_GLYPH_MARKER).enumerate() {
            output.push_str(segment);
            if marker_index == glyphs.len() {
                break;
            }
            let glyph = glyphs[marker_index];
            x = x
                .checked_add(i64::from(glyph.dx))
                .ok_or(DecoderError::DecompressionLimit)?;
            y = y
                .checked_add(i64::from(glyph.dy))
                .ok_or(DecoderError::DecompressionLimit)?;
            let path = match path_cache.get(&glyph.asset_id) {
                Some(path) => path,
                None => {
                    let asset = self
                        .index
                        .assets
                        .get(glyph.asset_id as usize)
                        .filter(|asset| asset.id == glyph.asset_id)
                        .ok_or(DecoderError::AssetNotFound(glyph.asset_id))?;
                    let (compressed, use_dictionary) = match asset.media_type.as_str() {
                        VECTOR_PATH_MEDIA_TYPE => (false, false),
                        VECTOR_PATH_ZSTD_MEDIA_TYPE => (true, false),
                        VECTOR_PATH_ZSTD_DICTIONARY_MEDIA_TYPE => (true, true),
                        _ => {
                            return Err(DecoderError::Corrupt(format!(
                                "asset {} is not a vector path",
                                glyph.asset_id
                            )))
                        }
                    };
                    let bytes = self.asset_bytes(glyph.asset_id)?;
                    let dictionary = if use_dictionary {
                        let dictionary_asset = self
                            .index
                            .assets
                            .iter()
                            .find(|asset| asset.media_type == VECTOR_DICTIONARY_MEDIA_TYPE)
                            .ok_or_else(|| {
                                DecoderError::Corrupt(
                                    "vector path dictionary asset is missing".into(),
                                )
                            })?;
                        Some(self.asset_bytes(dictionary_asset.id)?)
                    } else {
                        None
                    };
                    let path = decode_path_asset(&bytes, compressed, dictionary.as_deref())?;
                    path_cache.entry(glyph.asset_id).or_insert(path)
                }
            };
            let style = styles.get(glyph.style_id as usize).ok_or_else(|| {
                DecoderError::Corrupt("SVG glyph references a missing style".into())
            })?;
            output.push_str(&svg_path_element(path, style, x, y));
        }
        Ok(output)
    }

    /// Return a borrowed asset slice for mmap/in-memory sources, or owned bytes
    /// for range-backed sources.
    pub fn asset_bytes(&self, asset_id: u32) -> Result<Cow<'_, [u8]>, DecoderError> {
        let record = self
            .index
            .assets
            .get(asset_id as usize)
            .filter(|asset| asset.id == asset_id)
            .ok_or(DecoderError::AssetNotFound(asset_id))?;
        let length =
            usize::try_from(record.length).map_err(|_| DecoderError::DecompressionLimit)?;
        let bytes = self.source.read_slice(record.offset, length)?;
        if blake3::hash(&bytes).as_bytes() != &record.hash {
            return Err(DecoderError::Corrupt(format!(
                "asset {asset_id} hash mismatch"
            )));
        }
        Ok(bytes)
    }

    pub fn search(&self, query: &str) -> Result<Vec<SearchResult>, DecoderError> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let Some(search_block) = &self.search_block else {
            return Err(DecoderError::SearchUnavailable);
        };
        let mut counts = std::collections::BTreeMap::<u32, usize>::new();
        let mut seen_terms = std::collections::BTreeSet::new();
        for term in terms {
            if !seen_terms.insert(term.clone()) {
                continue;
            }
            if let Some(postings) = search_block.postings.get(&term) {
                for page_id in postings {
                    *counts.entry(*page_id).or_default() += 1;
                }
            }
        }
        let term_count = seen_terms.len();
        let mut results = counts
            .into_iter()
            .filter(|(_, matched_terms)| *matched_terms == term_count)
            .map(|(page_id, matched_terms)| SearchResult {
                page_id,
                matched_terms,
            })
            .collect::<Vec<_>>();
        results.sort_by_key(|result| result.page_id);
        Ok(results)
    }
}

fn validate_index(header: &FileHeader, index: &CentralIndex) -> Result<(), DecoderError> {
    if index.pages.len() != header.page_count as usize
        || index.assets.len() != header.asset_count as usize
    {
        return Err(DecoderError::Corrupt(
            "header counts do not match the central index".into(),
        ));
    }
    let footer_start = header.footer_offset;
    let mut regions = Vec::<(u64, u64)>::new();
    if header.page_count > 0 && index.chunks.is_empty() {
        return Err(DecoderError::Corrupt("page directory has no chunks".into()));
    }
    let mut next_page = 0u32;
    for (expected_id, asset) in index.assets.iter().enumerate() {
        if asset.id as usize != expected_id || asset.length == 0 {
            return Err(DecoderError::Corrupt("invalid asset record".into()));
        }
        let end = asset
            .offset
            .checked_add(asset.length)
            .ok_or_else(|| DecoderError::Corrupt("asset range overflows".into()))?;
        if asset.offset < HEADER_SIZE as u64 || end > footer_start {
            return Err(DecoderError::Corrupt(
                "asset range exceeds file data".into(),
            ));
        }
        regions.push((asset.offset, end));
    }
    for (chunk_id, chunk) in index.chunks.iter().enumerate() {
        if chunk.compressed_size == 0
            || chunk.compressed_size as usize > MAX_COMPRESSED_CHUNK_BYTES
            || chunk.uncompressed_size == 0
            || chunk.uncompressed_size as usize > MAX_CHUNK_BYTES
            || chunk.page_count == 0
        {
            return Err(DecoderError::Corrupt("empty chunk record".into()));
        }
        let end = chunk
            .offset
            .checked_add(u64::from(chunk.compressed_size))
            .ok_or_else(|| DecoderError::Corrupt("chunk range overflows".into()))?;
        if chunk.offset < HEADER_SIZE as u64 || end > footer_start {
            return Err(DecoderError::Corrupt(
                "chunk range exceeds file data".into(),
            ));
        }
        let page_end = chunk
            .first_page
            .checked_add(chunk.page_count)
            .ok_or_else(|| DecoderError::Corrupt("chunk page range overflows".into()))?;
        if chunk.first_page != next_page || page_end as usize > index.pages.len() {
            return Err(DecoderError::Corrupt(
                "chunk page ranges are not contiguous".into(),
            ));
        }
        next_page = page_end;
        let chunk_pages = &index.pages[chunk.first_page as usize..page_end as usize];
        if chunk_pages
            .first()
            .is_some_and(|page| page.offset_in_chunk != 0)
            || chunk_pages
                .windows(2)
                .any(|pair| pair[0].offset_in_chunk >= pair[1].offset_in_chunk)
        {
            return Err(DecoderError::Corrupt(
                "page offsets are not strictly increasing from the chunk start".into(),
            ));
        }
        for page in chunk_pages {
            if page.chunk_id as usize != chunk_id
                || page.offset_in_chunk >= chunk.uncompressed_size
                || page.width == 0
                || page.height == 0
            {
                return Err(DecoderError::Corrupt(
                    "page directory does not match chunk table".into(),
                ));
            }
        }
        regions.push((chunk.offset, end));
    }
    if next_page != header.page_count {
        return Err(DecoderError::Corrupt(
            "chunk table does not cover every page".into(),
        ));
    }
    if index.search_block.compressed_size > 0 {
        if index.search_block.uncompressed_size == 0 {
            return Err(DecoderError::Corrupt(
                "search block has no uncompressed data".into(),
            ));
        }
        if index.search_block.uncompressed_size as usize > MAX_SEARCH_BLOCK_BYTES
            || index.search_block.compressed_size as usize > MAX_COMPRESSED_SEARCH_BYTES
        {
            return Err(DecoderError::DecompressionLimit);
        }
        let end = index
            .search_block
            .offset
            .checked_add(u64::from(index.search_block.compressed_size))
            .ok_or_else(|| DecoderError::Corrupt("search block range overflows".into()))?;
        if index.search_block.offset < HEADER_SIZE as u64 || end > footer_start {
            return Err(DecoderError::Corrupt(
                "search block range exceeds file data".into(),
            ));
        }
        regions.push((index.search_block.offset, end));
    } else if index.search_block.offset != 0 || index.search_block.uncompressed_size != 0 {
        return Err(DecoderError::Corrupt(
            "empty search block has non-zero offsets or sizes".into(),
        ));
    }
    regions.sort_unstable_by_key(|region| region.0);
    if regions.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(DecoderError::Corrupt(
            "asset and chunk ranges overlap".into(),
        ));
    }
    Ok(())
}

fn decompress_bounded(
    compressed: &[u8],
    capacity: usize,
    maximum_output: usize,
) -> Result<Vec<u8>, DecoderError> {
    if capacity > maximum_output {
        return Err(DecoderError::DecompressionLimit);
    }
    let mut decompressor = zstd::bulk::Decompressor::new()?;
    decompressor.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(27))?;
    Ok(decompressor.decompress(compressed, capacity)?)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::format::{AssetRecord, CentralIndex, FORMAT_VERSION};

    #[test]
    fn svg_asset_references_expand_to_avif_data_uris() {
        let bytes = b"avif-payload".to_vec();
        let document = HdocDocument {
            source: MemorySource::new(Arc::<[u8]>::from(bytes.clone())),
            header: FileHeader {
                version: FORMAT_VERSION,
                flags: 0,
                page_count: 0,
                asset_count: 1,
                footer_offset: 0,
                footer_size: 0,
            },
            index: CentralIndex {
                assets: vec![AssetRecord {
                    id: 0,
                    offset: 0,
                    length: bytes.len() as u64,
                    media_type: "image/avif".into(),
                    width: 0,
                    height: 0,
                    hash: *blake3::hash(&bytes).as_bytes(),
                }],
                ..CentralIndex::default()
            },
            search_block: None,
            chunk_cache: Mutex::new(ChunkCache::new(1)),
        };

        let svg = document
            .expand_svg_asset_references(
                r#"<svg><image href="hdoc-asset://0"/><image href='hdoc-asset://0'/></svg>"#,
            )
            .unwrap();
        let expected = STANDARD.encode(bytes);
        assert_eq!(
            svg.matches(&format!("{AVIF_DATA_URI_PREFIX}{expected}"))
                .count(),
            2
        );
        assert!(!svg.contains(SVG_ASSET_URI_PREFIX));
    }
}

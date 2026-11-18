use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    ops::Bound,
    sync::{Arc, Mutex, OnceLock},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::{
    cache::{ChunkCache, PathCache},
    error::DecoderError,
    format::{
        CentralIndex, FileHeader, END_MARKER, FLAG_ARCHIVAL, FLAG_ENCRYPTED, FLAG_HAS_SEARCH_INDEX,
        HEADER_SIZE,
    },
    opcodes::{
        decode_page, validate_svg_markup, validate_svg_presentation, PageCommand, PageDisplayList,
        SvgGlyphStyle, SvgGlyphUse,
    },
    search::{tokenize, SearchBlock},
    vector_path::decode_path_asset,
    ByteSource, FileSource, MemorySource, MmapSource,
};

const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
const MAX_SEARCH_BLOCK_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPRESSED_CHUNK_BYTES: usize = 65 * 1024 * 1024;
const MAX_COMPRESSED_SEARCH_BYTES: usize = 65 * 1024 * 1024;
const MAX_COMPRESSED_INDEX_BYTES: usize = 129 * 1024 * 1024;
const MAX_EXPANDED_SVG_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_CACHE_CHUNKS: usize = 4;
const DEFAULT_PATH_CACHE_ENTRIES: usize = 2048;
const VECTOR_PATH_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path;version=1";
const VECTOR_PATH_ZSTD_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-path+zstd;version=1";
const VECTOR_PATH_ZSTD_DICTIONARY_MEDIA_TYPE: &str =
    "application/vnd.hyperdoc.vector-path+zstd-dict;version=1";
const VECTOR_DICTIONARY_MEDIA_TYPE: &str = "application/vnd.hyperdoc.vector-dictionary;version=1";
const SVG_GLYPH_MARKER: &str = "<!--HDOC_GLYPH-->";
const SVG_ASSET_URI_PREFIX: &str = "hdoc-asset://";
const ALLOWED_SVG_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/avif",
    "image/jpeg",
    "image/png",
    "image/svg+xml",
    "image/webp",
];

fn append_svg_path_element(
    out: &mut String,
    path: &str,
    style: &SvgGlyphStyle,
    x: i64,
    y: i64,
) -> Result<(), DecoderError> {
    let x = x as f64 / 64.0;
    let y = y as f64 / 64.0;
    let estimated = 64usize
        .saturating_add(style.presentation.len())
        .saturating_add(path.len());
    ensure_expanded_capacity(out.len(), estimated)?;
    let _ = write!(
        out,
        "<path transform=\"matrix({} {} {} {} {x} {y})\"",
        style.matrix[0], style.matrix[1], style.matrix[2], style.matrix[3]
    );
    if !style.presentation.is_empty() {
        out.push(' ');
        out.push_str(&style.presentation);
    }
    out.push_str(" d=\"");
    out.push_str(path);
    out.push_str("\"/>");
    ensure_expanded_capacity(out.len(), 0)
}

fn append_svg_use_element(
    out: &mut String,
    asset_id: u32,
    style: &SvgGlyphStyle,
    x: i64,
    y: i64,
) -> Result<(), DecoderError> {
    let x = x as f64 / 64.0;
    let y = y as f64 / 64.0;
    let estimated = 72usize.saturating_add(style.presentation.len());
    ensure_expanded_capacity(out.len(), estimated)?;
    let _ = write!(
        out,
        "<use href=\"#hdoc-p{asset_id}\" transform=\"matrix({} {} {} {} {x} {y})\"",
        style.matrix[0], style.matrix[1], style.matrix[2], style.matrix[3]
    );
    if !style.presentation.is_empty() {
        out.push(' ');
        out.push_str(&style.presentation);
    }
    out.push_str("/>");
    ensure_expanded_capacity(out.len(), 0)
}

fn ensure_expanded_capacity(current: usize, additional: usize) -> Result<(), DecoderError> {
    let total = current
        .checked_add(additional)
        .ok_or(DecoderError::DecompressionLimit)?;
    if total > MAX_EXPANDED_SVG_BYTES {
        return Err(DecoderError::DecompressionLimit);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub page_id: u32,
    pub matched_terms: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedSearchResult {
    pub page_id: u32,
    pub matched_terms: usize,
    pub score: f64,
    pub snippet: String,
}

/// Open HyperDoc reader. Page lookup is direct through the directory, then a
/// single independent chunk is decompressed and reused from bounded caches.
pub struct HdocDocument<S: ByteSource> {
    source: S,
    header: FileHeader,
    index: CentralIndex,
    dictionary_asset_id: Option<u32>,
    dictionary_bytes: OnceLock<Arc<[u8]>>,
    search_block: OnceLock<SearchBlock>,
    chunk_cache: Mutex<ChunkCache>,
    path_cache: Mutex<PathCache>,
    verified_assets: Mutex<Vec<bool>>,
}

impl HdocDocument<MmapSource> {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, DecoderError> {
        Self::from_source(MmapSource::open(path)?)
    }
}

impl HdocDocument<FileSource> {
    /// Open a file using safe positional OS reads (`pread` / `seek_read`) without `mmap`.
    pub fn open_file(path: impl AsRef<std::path::Path>) -> Result<Self, DecoderError> {
        Self::from_source(FileSource::open(path)?)
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
        let dictionary_asset_id = index
            .assets
            .iter()
            .find(|asset| asset.media_type == VECTOR_DICTIONARY_MEDIA_TYPE)
            .map(|asset| asset.id);
        let asset_total = index.assets.len();
        Ok(Self {
            source,
            header,
            index,
            dictionary_asset_id,
            dictionary_bytes: OnceLock::new(),
            search_block: OnceLock::new(),
            chunk_cache: Mutex::new(ChunkCache::new(cache_chunks)),
            path_cache: Mutex::new(PathCache::new(DEFAULT_PATH_CACHE_ENTRIES)),
            verified_assets: Mutex::new(vec![false; asset_total]),
        })
    }

    pub fn page_count(&self) -> u32 {
        self.header.page_count
    }

    pub fn header(&self) -> FileHeader {
        self.header
    }

    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.index.metadata
    }

    pub fn asset_count(&self) -> u32 {
        self.header.asset_count
    }

    /// Original UTF-8 text for a page, lazily decoding the search block on first access.
    pub fn page_text(&self, page_id: u32) -> Result<Option<&str>, DecoderError> {
        if page_id >= self.header.page_count {
            return Err(DecoderError::PageOutOfRange(page_id));
        }
        if self.index.search_block.compressed_size == 0 {
            return Ok(None);
        }
        let block = self.ensure_search_block()?;
        Ok(block.page_text(page_id))
    }

    /// Decode a page's structured display list without expanding `DrawSvgTemplate`
    /// or converting `hdoc-asset://` URIs into Base64 strings.
    pub fn page_raw(&self, page_id: u32) -> Result<PageDisplayList, DecoderError> {
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
        Ok(page)
    }

    pub fn page(&self, page_id: u32) -> Result<PageDisplayList, DecoderError> {
        let mut page = self.page_raw(page_id)?;
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
                    validate_svg_markup(svg)?;
                    Some(svg.clone())
                }
                PageCommand::DrawSvgFallback { svg } => {
                    validate_svg_markup(svg)?;
                    None
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
        ensure_expanded_capacity(svg.len(), 0)?;
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
                let media_lower = record.media_type.to_ascii_lowercase();
                if ALLOWED_SVG_IMAGE_MEDIA_TYPES
                    .binary_search(&media_lower.as_str())
                    .is_err()
                {
                    return Err(DecoderError::Corrupt(format!(
                        "asset {asset_id} has unsupported SVG image media type {}",
                        record.media_type
                    )));
                }
                let bytes = self.asset_bytes(asset_id)?;
                let b64_len = bytes.len().saturating_mul(4) / 3 + 32 + media_lower.len();
                ensure_expanded_capacity(output.len(), b64_len)?;
                let mut data_uri = String::with_capacity(b64_len);
                data_uri.push_str("data:");
                data_uri.push_str(&media_lower);
                data_uri.push_str(";base64,");
                data_uri.push_str(&STANDARD.encode(&bytes));
                entry.insert(data_uri);
            }
            let data_uri = encoded_assets
                .get(&asset_id)
                .expect("image asset has just been inserted into the page cache");

            let prefix = &remaining[..reference_start];
            ensure_expanded_capacity(output.len(), prefix.len().saturating_add(data_uri.len()))?;
            output.push_str(prefix);
            output.push_str(data_uri);
            remaining = &reference_tail[reference_end..];
        }
        ensure_expanded_capacity(output.len(), remaining.len())?;
        output.push_str(remaining);
        Ok(output)
    }

    fn expand_svg_template(
        &self,
        template: &str,
        styles: &[SvgGlyphStyle],
        glyphs: &[SvgGlyphUse],
    ) -> Result<String, DecoderError> {
        validate_svg_markup(template)?;
        for style in styles {
            validate_svg_presentation(&style.presentation)?;
        }

        let mut asset_uses = BTreeMap::<u32, usize>::new();
        for glyph in glyphs {
            *asset_uses.entry(glyph.asset_id).or_insert(0) += 1;
        }
        let mut local_paths = HashMap::<u32, Arc<str>>::with_capacity(asset_uses.len());
        for &asset_id in asset_uses.keys() {
            let decoded = self.resolve_vector_path(asset_id)?;
            local_paths.insert(asset_id, decoded);
        }

        let svg_root_insert_pos = find_svg_open_tag_body_offset(template);
        let mut defs_block = String::new();
        if svg_root_insert_pos.is_some() {
            for (&asset_id, &count) in &asset_uses {
                if count >= 2 {
                    let path = &local_paths[&asset_id];
                    ensure_expanded_capacity(defs_block.len(), path.len().saturating_add(40))?;
                    if defs_block.is_empty() {
                        defs_block.push_str("<defs>");
                    }
                    let _ = write!(defs_block, "<path id=\"hdoc-p{asset_id}\" d=\"{path}\"/>");
                }
            }
            if !defs_block.is_empty() {
                defs_block.push_str("</defs>");
            }
        }

        let initial_cap = template
            .len()
            .saturating_add(defs_block.len())
            .saturating_add(glyphs.len().saturating_mul(72))
            .min(4 * 1024 * 1024);
        ensure_expanded_capacity(template.len(), defs_block.len())?;
        let mut output = String::with_capacity(initial_cap);
        let mut x = 0i64;
        let mut y = 0i64;
        let mut injected_defs = defs_block.is_empty();
        let insert_at = svg_root_insert_pos.unwrap_or(0);
        let mut consumed_template_bytes = 0usize;
        let mut marker_count = 0usize;

        for (segment_index, segment) in template.split(SVG_GLYPH_MARKER).enumerate() {
            if segment_index > 0 {
                marker_count += 1;
                if marker_count > glyphs.len() {
                    return Err(DecoderError::Corrupt(
                        "SVG glyph template marker count does not match glyph table".into(),
                    ));
                }
                let glyph = glyphs[marker_count - 1];
                x = x
                    .checked_add(i64::from(glyph.dx))
                    .ok_or(DecoderError::DecompressionLimit)?;
                y = y
                    .checked_add(i64::from(glyph.dy))
                    .ok_or(DecoderError::DecompressionLimit)?;
                let style = styles.get(glyph.style_id as usize).ok_or_else(|| {
                    DecoderError::Corrupt("SVG glyph references a missing style".into())
                })?;
                if !defs_block.is_empty() && asset_uses[&glyph.asset_id] >= 2 {
                    append_svg_use_element(&mut output, glyph.asset_id, style, x, y)?;
                } else {
                    let path = &local_paths[&glyph.asset_id];
                    append_svg_path_element(&mut output, path, style, x, y)?;
                }
            }

            ensure_expanded_capacity(output.len(), segment.len())?;
            if !injected_defs && consumed_template_bytes <= insert_at {
                let seg_end = consumed_template_bytes + segment.len();
                if insert_at <= seg_end {
                    let split_rel = insert_at - consumed_template_bytes;
                    output.push_str(&segment[..split_rel]);
                    output.push_str(&defs_block);
                    output.push_str(&segment[split_rel..]);
                    injected_defs = true;
                } else {
                    output.push_str(segment);
                }
            } else {
                output.push_str(segment);
            }
            consumed_template_bytes += segment.len() + SVG_GLYPH_MARKER.len();
        }

        if marker_count != glyphs.len() {
            return Err(DecoderError::Corrupt(
                "SVG glyph template marker count does not match glyph table".into(),
            ));
        }
        Ok(output)
    }

    fn resolve_vector_path(&self, asset_id: u32) -> Result<Arc<str>, DecoderError> {
        if let Some(cached) = self
            .path_cache
            .lock()
            .map_err(|_| DecoderError::Corrupt("path cache lock is poisoned".into()))?
            .get(asset_id)
        {
            return Ok(cached);
        }
        let asset = self
            .index
            .assets
            .get(asset_id as usize)
            .filter(|asset| asset.id == asset_id)
            .ok_or(DecoderError::AssetNotFound(asset_id))?;
        let (compressed, use_dictionary) = match asset.media_type.as_str() {
            VECTOR_PATH_MEDIA_TYPE => (false, false),
            VECTOR_PATH_ZSTD_MEDIA_TYPE => (true, false),
            VECTOR_PATH_ZSTD_DICTIONARY_MEDIA_TYPE => (true, true),
            _ => {
                return Err(DecoderError::Corrupt(format!(
                    "asset {asset_id} is not a vector path"
                )));
            }
        };
        let bytes = self.asset_bytes(asset_id)?;
        let dictionary = if use_dictionary {
            Some(self.ensure_dictionary_bytes()?)
        } else {
            None
        };
        let decoded: Arc<str> = decode_path_asset(&bytes, compressed, dictionary)?.into();
        self.path_cache
            .lock()
            .map_err(|_| DecoderError::Corrupt("path cache lock is poisoned".into()))?
            .insert(asset_id, Arc::clone(&decoded));
        Ok(decoded)
    }

    fn ensure_dictionary_bytes(&self) -> Result<&[u8], DecoderError> {
        if let Some(bytes) = self.dictionary_bytes.get() {
            return Ok(bytes.as_ref());
        }
        let dict_id = self.dictionary_asset_id.ok_or_else(|| {
            DecoderError::Corrupt("vector path dictionary asset is missing".into())
        })?;
        let loaded: Arc<[u8]> = self.asset_bytes(dict_id)?.into_owned().into();
        let _ = self.dictionary_bytes.set(loaded);
        Ok(self
            .dictionary_bytes
            .get()
            .expect("dictionary bytes initialized")
            .as_ref())
    }

    fn ensure_search_block(&self) -> Result<&SearchBlock, DecoderError> {
        if let Some(block) = self.search_block.get() {
            return Ok(block);
        }
        let record = self.index.search_block;
        if record.compressed_size == 0 {
            return Err(DecoderError::SearchUnavailable);
        }
        if record.uncompressed_size as usize > MAX_SEARCH_BLOCK_BYTES
            || record.compressed_size as usize > MAX_COMPRESSED_SEARCH_BYTES
        {
            return Err(DecoderError::DecompressionLimit);
        }
        let compressed = self
            .source
            .read_slice(record.offset, record.compressed_size as usize)?;
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
        if block.page_texts.len() != self.header.page_count as usize {
            return Err(DecoderError::Corrupt(
                "search block page count does not match header".into(),
            ));
        }
        if block
            .postings
            .values()
            .flatten()
            .any(|page| *page >= self.header.page_count)
        {
            return Err(DecoderError::Corrupt(
                "search index references a missing page".into(),
            ));
        }
        let _ = self.search_block.set(block);
        Ok(self.search_block.get().expect("search block initialized"))
    }

    /// Return a borrowed asset slice for mmap/in-memory sources, or owned bytes
    /// for file/range-backed sources. BLAKE3 integrity is verified once per asset.
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
        let already_verified = self
            .verified_assets
            .lock()
            .map_err(|_| DecoderError::Corrupt("asset verification lock is poisoned".into()))?
            .get(asset_id as usize)
            .copied()
            .unwrap_or(false);
        if !already_verified {
            if blake3::hash(&bytes).as_bytes() != &record.hash {
                return Err(DecoderError::Corrupt(format!(
                    "asset {asset_id} hash mismatch"
                )));
            }
            if let Some(slot) = self
                .verified_assets
                .lock()
                .map_err(|_| DecoderError::Corrupt("asset verification lock is poisoned".into()))?
                .get_mut(asset_id as usize)
            {
                *slot = true;
            }
        }
        Ok(bytes)
    }

    pub fn search(&self, query: &str) -> Result<Vec<SearchResult>, DecoderError> {
        let terms = tokenize(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        if self.index.search_block.compressed_size == 0 {
            return Err(DecoderError::SearchUnavailable);
        }
        let search_block = self.ensure_search_block()?;
        let mut counts = BTreeMap::<u32, usize>::new();
        let mut seen_terms = BTreeSet::new();
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

    /// Rank matching pages using BM25 term-frequency relevance, prefix expansion,
    /// and automatic UTF-8 context snippets.
    pub fn search_ranked(&self, query: &str) -> Result<Vec<RankedSearchResult>, DecoderError> {
        let raw_terms = tokenize(query);
        if raw_terms.is_empty() {
            return Ok(Vec::new());
        }
        if self.index.search_block.compressed_size == 0 {
            return Err(DecoderError::SearchUnavailable);
        }
        let search_block = self.ensure_search_block()?;
        let mut unique_terms = Vec::new();
        let mut seen = BTreeSet::new();
        for term in raw_terms {
            if seen.insert(term.clone()) {
                unique_terms.push(term);
            }
        }

        let total_docs = search_block.page_texts.len().max(1) as f64;
        let doc_lengths: Vec<f64> = search_block
            .page_texts
            .iter()
            .map(|text| (text.len() as f64 / 5.0).max(1.0))
            .collect();
        let avg_doc_len = doc_lengths.iter().sum::<f64>() / total_docs;
        let k1 = 1.2f64;
        let b = 0.75f64;

        let mut scores = BTreeMap::<u32, (usize, f64)>::new();
        for query_term in &unique_terms {
            let mut page_tf = BTreeMap::<u32, (u32, f64)>::new();
            for (indexed_term, tf_entries) in search_block
                .term_frequencies
                .range((Bound::Included(query_term.clone()), Bound::Unbounded))
            {
                if !indexed_term.starts_with(query_term.as_str()) {
                    break;
                }
                let weight = if indexed_term == query_term {
                    1.0
                } else {
                    0.65
                };
                for &(page_id, tf) in tf_entries {
                    let entry = page_tf.entry(page_id).or_insert((0, 0.0));
                    entry.0 = entry.0.saturating_add(tf);
                    if weight > entry.1 {
                        entry.1 = weight;
                    }
                }
            }
            if page_tf.is_empty() {
                continue;
            }
            let df = page_tf.len() as f64;
            let idf = ((total_docs - df + 0.5) / (df + 0.5) + 1.0).ln();
            for (page_id, (tf, prefix_weight)) in page_tf {
                let doc_len = doc_lengths
                    .get(page_id as usize)
                    .copied()
                    .unwrap_or(avg_doc_len);
                let tf_f = f64::from(tf);
                let norm = tf_f * (k1 + 1.0)
                    / (tf_f + k1 * (1.0 - b + b * (doc_len / avg_doc_len.max(1.0))));
                let entry = scores.entry(page_id).or_insert((0, 0.0));
                entry.0 += 1;
                entry.1 += idf * norm * prefix_weight;
            }
        }

        let mut results = scores
            .into_iter()
            .map(|(page_id, (matched_terms, score))| RankedSearchResult {
                page_id,
                matched_terms,
                score,
                snippet: search_block.snippet_for_page(page_id, &unique_terms, 120),
            })
            .collect::<Vec<_>>();
        results.sort_by(|left, right| {
            right
                .matched_terms
                .cmp(&left.matched_terms)
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| left.page_id.cmp(&right.page_id))
        });
        Ok(results)
    }
}

fn find_svg_open_tag_body_offset(svg: &str) -> Option<usize> {
    let svg_start = svg.find("<svg")?;
    let bytes = svg.as_bytes();
    let mut cursor = svg_start + 4;
    let mut quote = None;
    while let Some(byte) = bytes.get(cursor).copied() {
        match (quote, byte) {
            (Some(open), ch) if open == ch => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => {
                if cursor > svg_start && bytes[cursor - 1] == b'/' {
                    return None;
                }
                return Some(cursor + 1);
            }
            _ => {}
        }
        cursor += 1;
    }
    None
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
    use crate::{
        format::{AssetRecord, CentralIndex, FORMAT_VERSION},
        varint,
    };

    fn dummy_document_with_assets(assets: Vec<(Vec<u8>, &str)>) -> HdocDocument<MemorySource> {
        let mut blob = Vec::new();
        let mut records = Vec::new();
        for (idx, (bytes, media_type)) in assets.into_iter().enumerate() {
            let offset = blob.len() as u64;
            let length = bytes.len() as u64;
            let hash = *blake3::hash(&bytes).as_bytes();
            blob.extend_from_slice(&bytes);
            records.push(AssetRecord {
                id: idx as u32,
                offset,
                length,
                media_type: media_type.into(),
                width: 0,
                height: 0,
                hash,
            });
        }
        let asset_count = records.len();
        HdocDocument {
            source: MemorySource::new(Arc::<[u8]>::from(blob)),
            header: FileHeader {
                version: FORMAT_VERSION,
                flags: 0,
                page_count: 0,
                asset_count: asset_count as u32,
                footer_offset: 0,
                footer_size: 0,
            },
            index: CentralIndex {
                assets: records,
                ..CentralIndex::default()
            },
            dictionary_asset_id: None,
            dictionary_bytes: OnceLock::new(),
            search_block: OnceLock::new(),
            chunk_cache: Mutex::new(ChunkCache::new(1)),
            path_cache: Mutex::new(PathCache::new(16)),
            verified_assets: Mutex::new(vec![false; asset_count]),
        }
    }

    #[test]
    fn svg_asset_references_expand_avif_png_and_jpeg_data_uris() {
        let avif_bytes = b"avif-payload".to_vec();
        let png_bytes = b"png-payload".to_vec();
        let document = dummy_document_with_assets(vec![
            (avif_bytes.clone(), "image/avif"),
            (png_bytes.clone(), "image/png"),
        ]);

        let svg = document
            .expand_svg_asset_references(
                r#"<svg><image href="hdoc-asset://0"/><image href='hdoc-asset://1'/></svg>"#,
            )
            .unwrap();
        assert!(svg.contains(&format!(
            "data:image/avif;base64,{}",
            STANDARD.encode(&avif_bytes)
        )));
        assert!(svg.contains(&format!(
            "data:image/png;base64,{}",
            STANDARD.encode(&png_bytes)
        )));
        assert!(!svg.contains(SVG_ASSET_URI_PREFIX));
    }

    #[test]
    fn repeated_template_glyphs_emit_compact_defs_and_use_elements() {
        let mut hpth = b"HPTH\x01".to_vec();
        varint::write_u32(3, &mut hpth);
        hpth.push(b'M');
        varint::write_i32(0, &mut hpth);
        varint::write_i32(0, &mut hpth);
        hpth.push(b'l');
        varint::write_i32(64, &mut hpth);
        varint::write_i32(64, &mut hpth);
        hpth.push(b'z');

        let document = dummy_document_with_assets(vec![(hpth, VECTOR_PATH_MEDIA_TYPE)]);
        let styles = vec![SvgGlyphStyle {
            matrix: [1.0, 0.0, 0.0, 1.0],
            presentation: "fill=\"#000\"".into(),
        }];
        let glyphs = vec![
            SvgGlyphUse {
                asset_id: 0,
                style_id: 0,
                dx: 64,
                dy: 128,
            },
            SvgGlyphUse {
                asset_id: 0,
                style_id: 0,
                dx: 128,
                dy: 0,
            },
        ];
        let expanded = document
            .expand_svg_template(
                "<svg xmlns=\"http://www.w3.org/2000/svg\"><!--HDOC_GLYPH--><!--HDOC_GLYPH--></svg>",
                &styles,
                &glyphs,
            )
            .unwrap();

        assert_eq!(expanded.matches("<defs><path id=\"hdoc-p0\"").count(), 1);
        assert_eq!(expanded.matches("<use href=\"#hdoc-p0\"").count(), 2);
    }

    #[test]
    fn rejects_malicious_presentation_injection_in_svg_expansion() {
        let mut hpth = b"HPTH\x01".to_vec();
        varint::write_u32(1, &mut hpth);
        hpth.push(b'z');
        let document = dummy_document_with_assets(vec![(hpth, VECTOR_PATH_MEDIA_TYPE)]);
        let styles = vec![SvgGlyphStyle {
            matrix: [1.0, 0.0, 0.0, 1.0],
            presentation: "onload=\"alert(1)\"".into(),
        }];
        let glyphs = vec![SvgGlyphUse {
            asset_id: 0,
            style_id: 0,
            dx: 0,
            dy: 0,
        }];
        let result = document.expand_svg_template("<svg><!--HDOC_GLYPH--></svg>", &styles, &glyphs);
        assert!(result.is_err());
    }
}

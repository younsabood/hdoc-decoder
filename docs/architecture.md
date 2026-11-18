# Architecture

The decoder is a standalone crate and owns an independent implementation of
the HDOC schema. It does not depend on the encoder repository or a shared core.

## Open and read

1. Read and validate the 32-byte header.
2. Read the end marker and bounded compressed central index.
3. Validate page, chunk, asset, and search-block byte ranges without eagerly
   decompressing page chunks or the search block.
4. On `page_raw(id)` or `page(id)`, locate the page directory entry, decompress
   only the requested solid chunk (reusing an LRU chunk cache), and reuse
   document-wide dictionary and HPTH vector-path caches across pages.
5. On `page_text(id)`, `search(query)`, or `search_ranked(query)`, lazily
   decompress and memoize the search block inside `OnceLock`.

## Source modules

| Module | Responsibility |
| --- | --- |
| `source.rs` | `ByteSource`, safe positional `FileSource`, `MemorySource`, and `MmapSource`. |
| `format.rs` | Header and central-index binary schema and validation. |
| `reader.rs` | Lazy open, bounded SVG materialization, global caches, and BM25 search. |
| `cache.rs` | Bounded LRU caches for decompressed chunks and decoded HPTH paths. |
| `opcodes.rs` | Page bytecode decoding, SVG markup/attribute sanitization, and display lists. |
| `vector_path.rs` | Allocation-frugal HPTH bytecode decoding and dictionary decompression. |
| `search.rs` | Arabic/NFKC query normalization, delta-TF postings decoding, and snippets. |

The decoder is read-only: it does not contain the PDF parser, AVIF encoder, or
page renderer. See the [data-source diagram](diagrams/data-sources.mmd) and
[lazy-read diagram](diagrams/reader-flow.mmd).

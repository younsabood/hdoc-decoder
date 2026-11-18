# API reference

## `HdocDocument`

| Method | Purpose |
| --- | --- |
| `open(path)` | Open a file through a read-only memory map (`MmapSource`). |
| `open_file(path)` | Open a file through safe positional OS reads (`FileSource`). |
| `from_bytes(bytes)` | Decode from an in-memory buffer (`MemorySource`). |
| `from_source(source)` | Open a caller-implemented `ByteSource`. |
| `from_source_with_cache(source, chunks)` | Set the number of decoded chunks retained by the cache. |
| `page_count()` | Return the number of pages. |
| `header()` | Return the parsed fixed header. |
| `metadata()` | Borrow the document metadata. |
| `asset_count()` | Return the indexed asset count. |
| `page_text(page_id)` | Lazily decode the search block and borrow a page's UTF-8 text. |
| `page_raw(page_id)` | Decode a page's structured display list without SVG template expansion. |
| `page(page_id)` | Decode one page and materialize compact `<defs>`/`<use>` SVG and image URIs. |
| `asset_bytes(asset_id)` | Borrow or own verified bytes for an indexed asset. |
| `search(query)` | Exact normalized multi-term search over the lazy text index. |
| `search_ranked(query)` | BM25-ranked search with prefix expansion and context snippets. |

## Byte sources

`FileSource` performs safe positional file reads (`pread` / `seek_read`).
`MmapSource` maps a file read-only for zero-copy borrowing. `MemorySource` owns
an `Arc<[u8]>`. Custom implementations of `ByteSource` can fetch ranges from
remote storage.

## Page and search results

`page_raw()` preserves `PageCommand::DrawSvgTemplate` for GPU/vector renderers,
whereas `page()` expands templates into standalone SVG with bounded memory caps
and attribute sanitization. `SearchResult` reports exact term matches, while
`RankedSearchResult` adds BM25 `score` and UTF-8 `snippet` previews.

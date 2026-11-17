# API reference

## `HdocDocument`

| Method | Purpose |
| --- | --- |
| `open(path)` | Open a file through a read-only memory map. |
| `from_bytes(bytes)` | Decode from an in-memory buffer. |
| `from_source(source)` | Open a caller-implemented `ByteSource`. |
| `from_source_with_cache(source, chunks)` | Set the number of decoded chunks retained by the cache. |
| `page_count()` | Return the number of pages. |
| `header()` | Return the parsed fixed header. |
| `metadata()` | Borrow the document metadata. |
| `asset_count()` | Return the indexed asset count. |
| `page_text(page_id)` | Borrow a page's original UTF-8 text, if present. |
| `page(page_id)` | Decode one page and materialize SVG template references. |
| `asset_bytes(asset_id)` | Borrow or own the bytes for an indexed asset. |
| `search(query)` | Search the optional text index. |

## Byte sources

`MmapSource` maps a file read-only. `MemorySource` owns an `Arc<[u8]>`. The
`ByteSource` trait exposes length and bounded range reads; custom implementations
can fetch bytes from application-managed storage or a range-request layer.

## Page results

`page()` returns `PageDisplayList` with ordered `PageCommand` values. It does
not rasterize content. SVG and path content can be passed to a renderer chosen
by the application. `SearchResult` includes the zero-based page ID and matched
term count.

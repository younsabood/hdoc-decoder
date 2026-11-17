# Architecture

The decoder is a standalone crate and owns an independent implementation of
the HDOC schema. It does not depend on the encoder repository or a shared core.

## Open and read

1. Read and validate the 32-byte header.
2. Read the end marker and bounded compressed central index.
3. Validate page, chunk, and asset ranges and decode the optional search index.
4. On `page(id)`, locate the page directory entry and requested compressed
   chunk. Decompress that chunk only and decode the page's opcodes.
5. Reuse recently decoded chunks in the bounded LRU cache.

The index is loaded when the document opens, but page chunks are lazy. Asset
bytes are read on demand. A memory-mapped source can return borrowed slices;
other source implementations may return owned bytes.

## Source modules

| Module | Responsibility |
| --- | --- |
| `source.rs` | `ByteSource`, memory-buffer, and memory-map implementations. |
| `format.rs` | Decoder-side header and central-index parsing. |
| `reader.rs` | Document open, validation, page lookup, search, and asset access. |
| `cache.rs` | Bounded LRU cache for decompressed page chunks. |
| `opcodes.rs` | Page bytecode decoding and display-list types. |
| `vector_path.rs` | HPTH bytecode decoding and optional dictionary decompression. |
| `search.rs` | Search block parsing and query tokenization. |

The decoder is read-only: it does not contain the PDF parser, AVIF encoder, or
page renderer. See the [data-source diagram](diagrams/data-sources.mmd) and
[lazy-read diagram](diagrams/reader-flow.mmd).

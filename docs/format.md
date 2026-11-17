# HyperDoc 1.0 reader format

The decoder accepts the current HDOC v1 encoding. Integer fields are
little-endian. The fixed header begins with `HDOC`, carries version `0x0100`,
page and asset counts, central-index location, flags, and CRC32.

After the header, a file stores asset payloads, independent Zstandard page
chunks, an optional compressed Unicode search block, the compressed `HDIX`
central index, and the `HDOC-END-MARKER!` trailer. The index maps page numbers
to chunk offsets and describes assets without loading page payloads at open.

The current command stream includes state commands, positions, shaped glyph
runs, images, vectors, SVG fallbacks, SVG templates, and the mandatory
`END_PAGE` opcode. HPTH path assets may use raw bytes, an independent Zstandard
frame, or the writer's shared Zstandard dictionary asset.

See [encoder format documentation](https://github.com/younsabood/hdoc-encoder/blob/main/docs/format.md)
for the field tables and opcode IDs. The schema code here is maintained
independently, so format changes must be checked against both repositories.

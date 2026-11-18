use std::collections::BTreeMap;

use hdoc_decoder::{
    format::{
        AssetRecord, CentralIndex, ChunkRecord, FileHeader, PageRecord, SearchBlockRecord,
        END_MARKER, FLAG_HAS_SEARCH_INDEX, HEADER_SIZE,
    },
    opcodes::encode_page,
    varint, DecoderError, HdocDocument, PageCommand, PageDisplayList, SearchBlock, SvgGlyphStyle,
    SvgGlyphUse,
};

fn build_test_hdoc(
    raw_assets: Vec<(Vec<u8>, &str)>,
    pages: Vec<(PageDisplayList, &str)>,
) -> Vec<u8> {
    let mut output = vec![0u8; HEADER_SIZE];
    let mut asset_records = Vec::with_capacity(raw_assets.len());
    for (id, (bytes, media_type)) in raw_assets.into_iter().enumerate() {
        let offset = output.len() as u64;
        let length = bytes.len() as u64;
        let hash = *blake3::hash(&bytes).as_bytes();
        output.extend_from_slice(&bytes);
        asset_records.push(AssetRecord {
            id: id as u32,
            offset,
            length,
            media_type: media_type.into(),
            width: 120,
            height: 80,
            hash,
        });
    }

    let mut raw_chunk = Vec::new();
    let mut page_records = Vec::with_capacity(pages.len());
    let mut page_texts = Vec::with_capacity(pages.len());
    for (display_list, text) in pages {
        let offset_in_chunk = raw_chunk.len() as u32;
        let encoded = encode_page(&display_list).unwrap();
        raw_chunk.extend_from_slice(&encoded);
        page_records.push(PageRecord {
            chunk_id: 0,
            offset_in_chunk,
            width: display_list.width,
            height: display_list.height,
            flags: display_list.flags,
        });
        page_texts.push(text.to_owned());
    }

    let compressed_chunk = zstd::bulk::compress(&raw_chunk, 3).unwrap();
    let chunk_offset = output.len() as u64;
    output.extend_from_slice(&compressed_chunk);
    let chunk_record = ChunkRecord {
        offset: chunk_offset,
        compressed_size: compressed_chunk.len() as u32,
        uncompressed_size: raw_chunk.len() as u32,
        first_page: 0,
        page_count: page_records.len() as u32,
    };

    let raw_search = SearchBlock::from_page_texts(page_texts).encode().unwrap();
    let compressed_search = zstd::bulk::compress(&raw_search, 3).unwrap();
    let search_offset = output.len() as u64;
    output.extend_from_slice(&compressed_search);
    let search_block = SearchBlockRecord {
        offset: search_offset,
        compressed_size: compressed_search.len() as u32,
        uncompressed_size: raw_search.len() as u32,
    };

    let index = CentralIndex {
        assets: asset_records,
        chunks: vec![chunk_record],
        pages: page_records,
        metadata: BTreeMap::from([("title".into(), "Integration Doc".into())]),
        search_block,
    };
    let raw_index = index.encode().unwrap();
    let compressed_index = zstd::bulk::compress(&raw_index, 3).unwrap();
    let footer_offset = output.len() as u64;
    let footer_size = compressed_index.len() as u32;
    output.extend_from_slice(&compressed_index);
    output.extend_from_slice(&END_MARKER);

    let mut header = FileHeader::new(
        index.pages.len() as u32,
        index.assets.len() as u32,
        footer_offset,
        footer_size,
    );
    header.flags = FLAG_HAS_SEARCH_INDEX;
    output[..HEADER_SIZE].copy_from_slice(&header.encode());
    output
}

fn make_sample_hpth(length_units: i32) -> Vec<u8> {
    let mut hpth = b"HPTH\x01".to_vec();
    varint::write_u32(4, &mut hpth);
    hpth.push(b'M');
    varint::write_i32(0, &mut hpth);
    varint::write_i32(0, &mut hpth);
    hpth.push(b'l');
    varint::write_i32(length_units * 64, &mut hpth);
    varint::write_i32(0, &mut hpth);
    hpth.push(b'l');
    varint::write_i32(0, &mut hpth);
    varint::write_i32(length_units * 64, &mut hpth);
    hpth.push(b'z');
    hpth
}

#[test]
fn lazy_search_ranked_bm25_and_safe_file_source_work_end_to_end() {
    let dictionary = b"HPTH\x01 common vector glyph dictionary sample".to_vec();
    let raw_hpth = make_sample_hpth(10);
    let mut dict_compressor = zstd::bulk::Compressor::with_dictionary(9, &dictionary).unwrap();
    let compressed_hpth = dict_compressor.compress(&raw_hpth).unwrap();

    let mut page0 = PageDisplayList::new(612, 792);
    page0.push(PageCommand::DrawSvgTemplate {
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"hdoc-asset://2\"/><!--HDOC_GLYPH--><!--HDOC_GLYPH--></svg>".into(),
        glyph_styles: vec![SvgGlyphStyle {
            matrix: [1.0, 0.0, 0.0, 1.0],
            presentation: "fill=\"#222\"".into(),
        }],
        glyphs: vec![
            SvgGlyphUse {
                asset_id: 0,
                style_id: 0,
                dx: 64,
                dy: 128,
            },
            SvgGlyphUse {
                asset_id: 0,
                style_id: 0,
                dx: 256,
                dy: 0,
            },
        ],
    });

    let mut page1 = PageDisplayList::new(612, 792);
    page1.push(PageCommand::DrawSvgTemplate {
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\"><!--HDOC_GLYPH--></svg>".into(),
        glyph_styles: vec![SvgGlyphStyle {
            matrix: [0.5, 0.0, 0.0, 0.5],
            presentation: String::new(),
        }],
        glyphs: vec![SvgGlyphUse {
            asset_id: 0,
            style_id: 0,
            dx: 32,
            dy: 32,
        }],
    });

    let hdoc_bytes = build_test_hdoc(
        vec![
            (
                compressed_hpth,
                "application/vnd.hyperdoc.vector-path+zstd-dict;version=1",
            ),
            (
                dictionary,
                "application/vnd.hyperdoc.vector-dictionary;version=1",
            ),
            (b"png-binary-image".to_vec(), "image/png"),
        ],
        vec![
            (page0, "مقدمة قصيرة عن ضغط المستندات وﻻ ﺇﻟﻪ ﺇﻻ \u{FDF2}"),
            (
                page1,
                "ضغط المستندات المتقدم: ضغط عالي الكفاءة مع بحث سريع في مدرسة الهندسة",
            ),
        ],
    );

    let temp_path =
        std::env::temp_dir().join(format!("hdoc_decoder_test_{}.hdoc", std::process::id()));
    std::fs::write(&temp_path, &hdoc_bytes).unwrap();

    let doc = HdocDocument::open_file(&temp_path).unwrap();
    assert_eq!(doc.page_count(), 2);
    assert_eq!(
        doc.metadata().get("title").map(String::as_str),
        Some("Integration Doc")
    );

    let raw_p0 = doc.page_raw(0).unwrap();
    assert!(matches!(
        raw_p0.commands[0],
        PageCommand::DrawSvgTemplate { .. }
    ));

    let mat_p0 = doc.page(0).unwrap();
    let PageCommand::DrawSvgFallback { ref svg } = mat_p0.commands[0] else {
        panic!("expected materialized SVG");
    };
    assert!(svg.contains("<defs><path id=\"hdoc-p0\""));
    assert_eq!(svg.matches("<use href=\"#hdoc-p0\"").count(), 2);
    assert!(svg.contains("data:image/png;base64,"));

    let mat_p1 = doc.page(1).unwrap();
    let PageCommand::DrawSvgFallback { svg: ref svg1 } = mat_p1.commands[0] else {
        panic!("expected materialized SVG on page 1");
    };
    assert!(svg1.contains("<path transform="));

    let exact_arabic = doc.search("ولا اله الا الله").unwrap();
    assert_eq!(exact_arabic.len(), 1);
    assert_eq!(exact_arabic[0].page_id, 0);

    let ranked = doc.search_ranked("ضغط").unwrap();
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0].page_id, 1);
    assert!(ranked[0].score > ranked[1].score);
    assert!(!ranked[0].snippet.is_empty());

    let _ = std::fs::remove_file(&temp_path);
}

#[test]
fn stops_svg_expansion_amplification_bombs() {
    let mut big_hpth = b"HPTH\x01".to_vec();
    let segs = 25_000u32;
    varint::write_u32(segs, &mut big_hpth);
    big_hpth.push(b'M');
    varint::write_i32(0, &mut big_hpth);
    varint::write_i32(0, &mut big_hpth);
    for _ in 1..segs {
        big_hpth.push(b'l');
        varint::write_i32(64, &mut big_hpth);
        varint::write_i32(64, &mut big_hpth);
    }

    let repeat_count = 600_000usize;
    let template = format!("<svg>{}</svg>", "<!--HDOC_GLYPH-->".repeat(repeat_count));
    let mut page = PageDisplayList::new(500, 500);
    page.push(PageCommand::DrawSvgTemplate {
        svg: template,
        glyph_styles: vec![SvgGlyphStyle {
            matrix: [1.0, 0.0, 0.0, 1.0],
            presentation: String::new(),
        }],
        glyphs: (0..repeat_count)
            .map(|i| SvgGlyphUse {
                asset_id: (i % 2) as u32,
                style_id: 0,
                dx: 1,
                dy: 0,
            })
            .collect(),
    });

    let hdoc_bytes = build_test_hdoc(
        vec![
            (
                big_hpth.clone(),
                "application/vnd.hyperdoc.vector-path;version=1",
            ),
            (big_hpth, "application/vnd.hyperdoc.vector-path;version=1"),
        ],
        vec![(page, "bomb test")],
    );

    let doc = HdocDocument::from_bytes(hdoc_bytes).unwrap();
    assert!(matches!(doc.page(0), Err(DecoderError::DecompressionLimit)));
}

use crate::{varint, HdocError};

const PATH_MAGIC: &[u8; 4] = b"HPTH";
const PATH_VERSION: u8 = 1;
const MAX_PATH_SEGMENTS: usize = 1_000_000;

pub(crate) fn decode_path_asset(
    input: &[u8],
    compressed: bool,
    dictionary: Option<&[u8]>,
) -> Result<String, HdocError> {
    let decompressed;
    let input = if compressed {
        decompressed = if let Some(dictionary) = dictionary {
            zstd::bulk::Decompressor::with_dictionary(dictionary)
                .and_then(|mut decoder| decoder.decompress(input, 16 * 1024 * 1024))
        } else {
            zstd::bulk::decompress(input, 16 * 1024 * 1024)
        }
        .map_err(|error| {
            HdocError::InvalidFormat(format!("invalid compressed vector path: {error}"))
        })?;
        decompressed.as_slice()
    } else {
        input
    };
    if input.get(..4) != Some(PATH_MAGIC) {
        return Err(HdocError::InvalidFormat(
            "missing vector path asset magic".into(),
        ));
    }
    if input.get(4).copied() != Some(PATH_VERSION) {
        return Err(HdocError::InvalidFormat(
            "unsupported vector path asset version".into(),
        ));
    }
    let mut cursor = 5usize;
    let segment_count = varint::read_u32(input, &mut cursor)? as usize;
    if segment_count > MAX_PATH_SEGMENTS || segment_count > input.len().saturating_sub(cursor) {
        return Err(HdocError::LimitExceeded);
    }
    let mut output = String::new();
    for _ in 0..segment_count {
        let command = take(input, &mut cursor, 1)?[0];
        if !output.is_empty() {
            output.push(' ');
        }
        output.push(command as char);
        match command.to_ascii_uppercase() {
            b'M' | b'L' => {
                append_pair(input, &mut cursor, &mut output)?;
            }
            b'H' | b'V' => append_coord(input, &mut cursor, &mut output)?,
            b'C' => {
                for point in 0..3 {
                    append_pair(input, &mut cursor, &mut output)?;
                    if point < 2 {
                        output.push(' ');
                    }
                }
            }
            b'S' | b'Q' => {
                for point in 0..2 {
                    append_pair(input, &mut cursor, &mut output)?;
                    if point == 0 {
                        output.push(' ');
                    }
                }
            }
            b'T' => append_pair(input, &mut cursor, &mut output)?,
            b'A' => {
                append_coord(input, &mut cursor, &mut output)?;
                output.push(' ');
                append_coord(input, &mut cursor, &mut output)?;
                output.push(' ');
                append_coord(input, &mut cursor, &mut output)?;
                let flags = take(input, &mut cursor, 1)?[0];
                if flags & !0b11 != 0 {
                    return Err(HdocError::InvalidFormat("invalid vector arc flags".into()));
                }
                output.push(' ');
                output.push(if flags & 1 == 0 { '0' } else { '1' });
                output.push(' ');
                output.push(if flags & 2 == 0 { '0' } else { '1' });
                append_pair(input, &mut cursor, &mut output)?;
            }
            b'Z' => {}
            _ => {
                return Err(HdocError::InvalidFormat(
                    "unknown vector path command".into(),
                ));
            }
        }
    }
    if cursor != input.len() {
        return Err(HdocError::InvalidFormat(
            "unexpected data after vector path asset".into(),
        ));
    }
    Ok(output)
}

fn append_pair(input: &[u8], cursor: &mut usize, out: &mut String) -> Result<(), HdocError> {
    append_coord(input, cursor, out)?;
    out.push(' ');
    append_coord(input, cursor, out)
}

fn append_coord(input: &[u8], cursor: &mut usize, out: &mut String) -> Result<(), HdocError> {
    let value = varint::read_i32(input, cursor)? as f64 / 64.0;
    out.push_str(&value.to_string());
    Ok(())
}

fn take<'a>(input: &'a [u8], cursor: &mut usize, count: usize) -> Result<&'a [u8], HdocError> {
    let end = cursor.checked_add(count).ok_or(HdocError::LimitExceeded)?;
    let bytes = input.get(*cursor..end).ok_or(HdocError::Truncated)?;
    *cursor = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::decode_path_asset;
    use crate::varint;

    #[test]
    fn binary_path_asset_decodes_to_svg_path_data() {
        let mut bytes = b"HPTH\x01".to_vec();
        varint::write_u32(4, &mut bytes);
        bytes.push(b'M');
        varint::write_i32(0, &mut bytes);
        varint::write_i32(0, &mut bytes);
        bytes.push(b'L');
        varint::write_i32(64, &mut bytes);
        varint::write_i32(0, &mut bytes);
        bytes.push(b'L');
        varint::write_i32(64, &mut bytes);
        varint::write_i32(64, &mut bytes);
        bytes.push(b'z');

        assert_eq!(
            decode_path_asset(&bytes, false, None).unwrap(),
            "M0 0 L1 0 L1 1 z"
        );
    }

    #[test]
    fn curve_control_points_are_separated_in_svg_output() {
        let mut bytes = b"HPTH\x01".to_vec();
        varint::write_u32(2, &mut bytes);
        bytes.push(b'M');
        varint::write_i32(0, &mut bytes);
        varint::write_i32(0, &mut bytes);
        bytes.push(b'Q');
        for value in [64, 128, 192, 256] {
            varint::write_i32(value, &mut bytes);
        }

        assert_eq!(
            decode_path_asset(&bytes, false, None).unwrap(),
            "M0 0 Q1 2 3 4"
        );
    }

    #[test]
    fn dictionary_compressed_path_assets_decode() {
        let mut raw_path = b"HPTH\x01".to_vec();
        varint::write_u32(2, &mut raw_path);
        raw_path.push(b'M');
        varint::write_i32(0, &mut raw_path);
        varint::write_i32(0, &mut raw_path);
        raw_path.push(b'L');
        varint::write_i32(64000, &mut raw_path);
        varint::write_i32(64000, &mut raw_path);
        let dictionary = b"HPTH common path command coordinates";
        let mut compressor = zstd::bulk::Compressor::with_dictionary(19, dictionary).unwrap();
        let compressed = compressor.compress(&raw_path).unwrap();

        assert_eq!(
            decode_path_asset(&compressed, true, Some(dictionary)).unwrap(),
            "M0 0 L1000 1000"
        );
    }

    #[test]
    fn rejects_trailing_bytes_and_unknown_commands() {
        let mut bytes = b"HPTH\x01".to_vec();
        varint::write_u32(1, &mut bytes);
        bytes.push(b'X');
        assert!(decode_path_asset(&bytes, false, None).is_err());
    }
}

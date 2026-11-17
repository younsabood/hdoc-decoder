use thiserror::Error;

#[derive(Debug, Error)]
pub enum HdocError {
    #[error("invalid HyperDoc file: {0}")]
    InvalidFormat(String),
    #[error("unsupported HyperDoc version 0x{0:04x}")]
    UnsupportedVersion(u16),
    #[error("HyperDoc header checksum does not match")]
    ChecksumMismatch,
    #[error("truncated HyperDoc data")]
    Truncated,
    #[error("unknown page opcode 0x{0:02x}")]
    UnknownOpcode(u8),
    #[error("invalid UTF-8 in page text")]
    InvalidUtf8,
    #[error("requested value exceeds the HyperDoc format limit")]
    LimitExceeded,
}

#[derive(Debug, Error)]
pub enum DecoderError {
    #[error(transparent)]
    Format(#[from] HdocError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("requested page {0} is outside the document")]
    PageOutOfRange(u32),
    #[error("asset {0} does not exist")]
    AssetNotFound(u32),
    #[error("corrupt HyperDoc file: {0}")]
    Corrupt(String),
    #[error("byte range is outside the data source")]
    RangeOutOfBounds,
    #[error("decompressed data exceeds the configured safety limit")]
    DecompressionLimit,
    #[error("the document has no text search block")]
    SearchUnavailable,
    #[error("unsupported HyperDoc feature: {0}")]
    UnsupportedFeature(&'static str),
}

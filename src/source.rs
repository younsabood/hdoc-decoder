use std::{borrow::Cow, fs::File, path::Path, sync::Arc};

use memmap2::{Mmap, MmapOptions};

use crate::DecoderError;

/// Random-access input used by the decoder. Implementations may borrow bytes
/// from a memory map or return owned data for remote/range-backed sources.
pub trait ByteSource: Send + Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read_slice(&self, offset: u64, length: usize) -> Result<Cow<'_, [u8]>, DecoderError>;
}

#[derive(Debug, Clone)]
pub struct MemorySource {
    data: Arc<[u8]>,
}

impl MemorySource {
    pub fn new(data: impl Into<Arc<[u8]>>) -> Self {
        Self { data: data.into() }
    }
}

impl ByteSource for MemorySource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_slice(&self, offset: u64, length: usize) -> Result<Cow<'_, [u8]>, DecoderError> {
        let start = usize::try_from(offset).map_err(|_| DecoderError::RangeOutOfBounds)?;
        let end = start
            .checked_add(length)
            .ok_or(DecoderError::RangeOutOfBounds)?;
        self.data
            .get(start..end)
            .map(Cow::Borrowed)
            .ok_or(DecoderError::RangeOutOfBounds)
    }
}

#[derive(Debug)]
pub struct MmapSource {
    map: Mmap,
}

impl MmapSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DecoderError> {
        let file = File::open(path)?;
        // SAFETY: the map is read-only. Callers must not mutate or truncate the
        // underlying file while this source is alive, as with any file mmap.
        let map = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self { map })
    }
}

impl ByteSource for MmapSource {
    fn len(&self) -> u64 {
        self.map.len() as u64
    }

    fn read_slice(&self, offset: u64, length: usize) -> Result<Cow<'_, [u8]>, DecoderError> {
        let start = usize::try_from(offset).map_err(|_| DecoderError::RangeOutOfBounds)?;
        let end = start
            .checked_add(length)
            .ok_or(DecoderError::RangeOutOfBounds)?;
        self.map
            .get(start..end)
            .map(Cow::Borrowed)
            .ok_or(DecoderError::RangeOutOfBounds)
    }
}

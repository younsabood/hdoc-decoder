use std::{borrow::Cow, fs::File, path::Path, sync::Arc};

use memmap2::{Mmap, MmapOptions};

use crate::DecoderError;

/// Random-access input used by the decoder. Implementations may borrow bytes
/// from a memory map or return owned data for file/remote/range-backed sources.
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

/// Safe positional file reader (`pread` / `seek_read`) that avoids `mmap`
/// undefined behavior when external processes truncate or modify files on disk.
#[derive(Debug)]
pub struct FileSource {
    #[cfg(any(unix, windows))]
    file: File,
    #[cfg(not(any(unix, windows)))]
    file: std::sync::Mutex<File>,
    len: u64,
}

impl FileSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DecoderError> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            #[cfg(any(unix, windows))]
            file,
            #[cfg(not(any(unix, windows)))]
            file: std::sync::Mutex::new(file),
            len,
        })
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_slice(&self, offset: u64, length: usize) -> Result<Cow<'_, [u8]>, DecoderError> {
        let end = offset
            .checked_add(length as u64)
            .ok_or(DecoderError::RangeOutOfBounds)?;
        if end > self.len {
            return Err(DecoderError::RangeOutOfBounds);
        }
        let mut buffer = vec![0u8; length];
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt as _;
            let mut read_total = 0usize;
            while read_total < length {
                let pos = offset
                    .checked_add(read_total as u64)
                    .ok_or(DecoderError::RangeOutOfBounds)?;
                let count = self.file.read_at(&mut buffer[read_total..], pos)?;
                if count == 0 {
                    return Err(DecoderError::RangeOutOfBounds);
                }
                read_total += count;
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt as _;
            let mut read_total = 0usize;
            while read_total < length {
                let pos = offset
                    .checked_add(read_total as u64)
                    .ok_or(DecoderError::RangeOutOfBounds)?;
                let count = self.file.seek_read(&mut buffer[read_total..], pos)?;
                if count == 0 {
                    return Err(DecoderError::RangeOutOfBounds);
                }
                read_total += count;
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut guard = self
                .file
                .lock()
                .map_err(|_| DecoderError::Corrupt("file source lock is poisoned".into()))?;
            guard.seek(SeekFrom::Start(offset))?;
            guard.read_exact(&mut buffer)?;
        }
        Ok(Cow::Owned(buffer))
    }
}

#[derive(Debug)]
pub struct MmapSource {
    map: Mmap,
}

impl MmapSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DecoderError> {
        let file = File::open(path)?;
        // SAFETY: the map is read-only. Callers who cannot guarantee that external
        // processes will not truncate the file should prefer `FileSource::open`.
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

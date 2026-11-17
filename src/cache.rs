use std::{num::NonZeroUsize, sync::Arc};

use lru::LruCache;

#[derive(Debug)]
pub(crate) struct ChunkCache {
    entries: LruCache<u32, Arc<[u8]>>,
}

impl ChunkCache {
    pub fn new(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity is clamped to one");
        Self {
            entries: LruCache::new(capacity),
        }
    }

    pub fn get(&mut self, chunk_id: u32) -> Option<Arc<[u8]>> {
        self.entries.get(&chunk_id).cloned()
    }
    pub fn insert(&mut self, chunk_id: u32, data: Arc<[u8]>) {
        self.entries.put(chunk_id, data);
    }
}

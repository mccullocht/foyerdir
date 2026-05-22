use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    file: usize,
    page: usize,
}

/// The underlying native implementation of the Java FoyerDirectory class.
///
/// This is structured as a native class to avoid caching lots of data on the Java heap, which also
/// forces us to handle IO details to provide read-through and write-through cache.
pub struct FoyerDirectory {
    path: PathBuf,
    dir_file: File,
    cache: foyer::Cache<CacheKey, Box<[u8]>>,
    page_size: usize,
}

impl FoyerDirectory {
    fn new(path: PathBuf, cache_size: u64, page_size: u32) -> io::Result<Self> {
        let dir_file = File::open(&path)?;
        let cache = foyer::CacheBuilder::new(cache_size as usize)
            .with_weighter(|_key: &CacheKey, value: &Box<[u8]>| {
                std::mem::size_of::<CacheKey>() + value.len()
            })
            .build();
        Ok(Self {
            path,
            dir_file,
            cache,
            page_size: page_size as usize,
        })
    }

    fn sync_metadata(&self) -> io::Result<()> {
        self.dir_file.sync_all()
    }
}

/// Returns the library version as a u32.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foyerdir_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn foyer_open_directory(
    path: *const u8,
    path_len: u64,
    cache_bytes: u64,
    log_page_size: i32,
) -> *const FoyerDirectory {
    let path_bytes = unsafe { std::slice::from_raw_parts(path, path_len as usize) };
    let path = PathBuf::from(std::str::from_utf8(path_bytes).unwrap());
    let page_size = 1u32 << log_page_size;
    Arc::into_raw(Arc::new(
        FoyerDirectory::new(path, cache_bytes, page_size).unwrap(),
    ))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn foyer_close_directory(dir: *const FoyerDirectory) {
    let dir = unsafe { Arc::from_raw(dir) };
    if Arc::strong_count(&dir) > 1 {
        eprintln!("Closing directory with open files!");
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn foyer_directory_sync(dir: *const FoyerDirectory) {
    let dir = unsafe { &*dir };
    dir.sync_metadata()
        .expect("by policy, crash if sync fails.");
}

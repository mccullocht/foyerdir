use std::path::PathBuf;

#[derive(Debug, Copy, Clone, Hash, PartialEq, Eq)]
struct CacheKey {
    file_ord: usize,
    offset: usize,
}

pub struct FoyerDirectory {
    path: PathBuf,
    cache: foyer::Cache<CacheKey, Box<[u8]>>,
    page_size: usize,
}

impl FoyerDirectory {
    fn new(path: PathBuf, cache_size: u64, page_size: u32) -> Self {
        let cache = foyer::CacheBuilder::new(cache_size as usize)
            .with_weighter(|_key: &CacheKey, value: &Box<[u8]>| {
                std::mem::size_of::<CacheKey>() + value.len()
            })
            .build();
        Self {
            path,
            cache,
            page_size: page_size as usize,
        }
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
) -> *mut FoyerDirectory {
    let path_bytes = unsafe { std::slice::from_raw_parts(path, path_len as usize) };
    let path = PathBuf::from(std::str::from_utf8(path_bytes).unwrap());
    let page_size = 1u32 << log_page_size;
    Box::into_raw(Box::new(FoyerDirectory::new(path, cache_bytes, page_size)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn foyer_close_directory(dir: *mut FoyerDirectory) {
    let _ = unsafe { Box::from_raw(dir) };
}

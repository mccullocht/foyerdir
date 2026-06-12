#![allow(unused)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};

use aligned_vec::{ABox, AVec, ConstAlign};
use crc::{CRC_32_ISO_HDLC, Crc, Digest, Table};
use foyer::{CacheEntry, GetOrFetch};
use futures::future::join_all;

const PAGE_ALIGNMENT: usize = 4096;
type PageVec = AVec<u8, ConstAlign<PAGE_ALIGNMENT>>;
type PageBox = ABox<[u8], ConstAlign<PAGE_ALIGNMENT>>;

fn alloc_page(len: usize) -> PageVec {
    let mut v = PageVec::with_capacity(4096, len.next_multiple_of(4096));
    v.resize(len.next_multiple_of(4096), 0u8);
    v
}

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
    cache: foyer::Cache<CacheKey, PageBox>,
    page_size: usize,
    runtime: tokio::runtime::Runtime,
    file_ids: FileIdMap,
}

impl FoyerDirectory {
    fn new(path: PathBuf, cache_size: u64, page_size: usize) -> io::Result<Self> {
        let dir_file = File::open(&path)?;
        let num_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .max_blocking_threads(num_cores)
            .build()?;
        // Build the cache inside the runtime so foyer's background tasks have a reactor.
        let _guard = runtime.enter();
        let cache = foyer::CacheBuilder::new(cache_size as usize)
            .with_weighter(|_key: &CacheKey, value: &PageBox| {
                std::mem::size_of::<CacheKey>() + value.len().next_multiple_of(PAGE_ALIGNMENT)
            })
            .build();
        Ok(Self {
            path,
            dir_file,
            cache,
            page_size,
            runtime,
            file_ids: FileIdMap::default(),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn sync_metadata(&self) -> io::Result<()> {
        self.dir_file.sync_all()
    }

    fn delete_file_id(&self, relative_path: &Path) {
        self.file_ids.remove(relative_path);
    }

    fn new_input(self: &Arc<Self>, relative_path: &Path) -> io::Result<FoyerIndexInput> {
        let path = self.path.join(relative_path);
        FoyerIndexInput::new(
            Arc::clone(self),
            &path,
            self.file_ids.get_input(relative_path),
            self.page_size,
        )
    }

    fn new_output(self: &Arc<Self>, relative_path: &Path) -> io::Result<FoyerIndexOutput> {
        let path = self.path.join(relative_path);
        FoyerIndexOutput::new(
            Arc::clone(self),
            &self.path.join(relative_path),
            self.file_ids.get_output(relative_path),
            self.page_size,
        )
    }
}

#[derive(Default)]
struct FileIdMap {
    map: RwLock<HashMap<PathBuf, usize>>,
    next: AtomicUsize,
}

impl FileIdMap {
    fn get_input(&self, relative_path: &Path) -> usize {
        {
            let m = self.map.read().expect("poison");
            if let Some(id) = m.get(relative_path) {
                return *id;
            }
        }
        self.get_output(relative_path)
    }

    fn get_output(&self, relative_path: &Path) -> usize {
        let mut m = self.map.write().expect("poison");
        *m.entry(relative_path.to_owned())
            .or_insert_with(|| self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }

    fn remove(&self, relative_path: &Path) {
        let mut m = self.map.write().expect("poison");
        m.remove(relative_path);
    }
}

pub struct FoyerIndexInput {
    dir: Arc<FoyerDirectory>,
    file: Arc<File>,
    file_len: usize,
    file_id: usize,
    pages: usize,
    last_page_size: usize,
}

/// Opens a file read-only, bypassing the kernel buffer cache.
///
/// On Linux this sets O_DIRECT at open time. On macOS it uses F_NOCACHE via fcntl, which
/// is the only supported mechanism for disabling the buffer cache on that platform.
fn open_direct(path: &Path) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(path)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let file = File::open(path)?;
        let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
        if ret == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }
}

impl FoyerIndexInput {
    fn new(
        dir: Arc<FoyerDirectory>,
        path: &Path,
        file_id: usize,
        page_size: usize,
    ) -> io::Result<Self> {
        let file = open_direct(path)?;
        let file_len = file.metadata()?.len() as usize;
        let pages = file_len.div_ceil(page_size);
        let last_page_size = if pages == 0 {
            0
        } else {
            let r = file_len % page_size;
            if r == 0 { page_size } else { r }
        };
        Ok(Self {
            dir,
            file: Arc::new(file),
            file_len,
            file_id,
            pages,
            last_page_size,
        })
    }

    /// pread starting at `off` into `out`, writing up to `out.len()` bytes.
    ///
    /// Returns the number of bytes actually read into the buffer.
    ///
    /// This method is intended to handle small buffered reads for IndexInput API calls that
    /// read very small amounts of data (a byte to a few KB).
    /// requesting a few KB of data or less.
    fn read_at(&self, off: u64, out: &mut [u8]) -> foyer::Result<u64> {
        let pinned_read = self.read_chunked(off, out.len() as u64)?;
        let mut out_next = 0usize;
        for s in pinned_read.page_iter() {
            let out_end = out_next + s.len();
            out[out_next..out_end].copy_from_slice(s);
            out_next = out_end;
        }
        Ok(out_next as u64)
    }

    /// pread starting at `off` and up to `len` bytes.
    ///
    /// Returns a struct containing references to page cache entries and an iterator that can be
    /// used to access all of the bytes in the range.
    ///
    /// This method is intended for large reads where it would be impractical to allocate a direct
    /// ByteBuffer to pass down to the native layer. This method returns a handle that _pins entries
    /// in the cache_ which prevents those entries from being freed. Callers are encouraged to
    /// free the handle quickly to avoid locking up cache space.
    fn read_chunked(&self, off: u64, len: u64) -> foyer::Result<ReadChunks> {
        let _guard = self.dir.runtime.enter();
        let pages = if let Some(pages) = self.page_range(off, len) {
            let page_reads = self.read_pages(pages);
            if page_reads.iter().any(GetOrFetch::need_await) {
                let read = self
                    .dir
                    .runtime
                    .block_on(async move { join_all(page_reads).await });
                read.into_iter().collect::<foyer::Result<Vec<_>>>()
            } else {
                Ok(page_reads
                    .into_iter()
                    .map(|p| p.try_unwrap().expect("in cache"))
                    .collect())
            }
        } else {
            Ok(vec![])
        }?;
        Ok(ReadChunks::new(pages, self.dir.page_size, off, len))
    }

    /// Fetch the pages necessary to read `off..(off + len)` into the cache.
    /// This method issues any necessary reads asynchronously without blocking so the pages are not
    /// guaranteed to exist when this call returns.
    fn prefetch(&self, off: u64, len: u64) {
        if let Some(pages) = self.page_range(off, len) {
            let _guard = self.dir.runtime.enter();
            let page_reads = self.read_pages(pages);
            if page_reads.iter().any(GetOrFetch::need_await) {
                self.dir
                    .runtime
                    .spawn(async move { join_all(page_reads).await });
            }
        }
    }

    fn len(&self) -> usize {
        self.file_len
    }

    /// Computes a range of pages to read to get all of the bytes in `off..(off + len)`.
    /// Returns `None` if this range would not produce any pages.
    fn page_range(&self, off: u64, len: u64) -> Option<RangeInclusive<usize>> {
        if self.pages > 0 && len > 0 && off < self.file_len as u64 {
            let page_size = self.dir.page_size;
            let first_page = off as usize / page_size;
            let last_byte = off + len - 1;
            let last_page = last_byte as usize / page_size;
            Some(first_page..=last_page)
        } else {
            None
        }
    }

    fn read_pages(
        &self,
        pages: impl IntoIterator<Item = usize>,
    ) -> Vec<GetOrFetch<CacheKey, PageBox>> {
        pages
            .into_iter()
            .map(|p| {
                let dir = Arc::clone(&self.dir);
                let file = Arc::clone(&self.file);
                let page_size = dir.page_size;
                let page_offset = dir.page_size as u64 * p as u64;
                dir.cache.get_or_fetch(
                    &CacheKey {
                        file: self.file_id,
                        page: p as usize,
                    },
                    move || async move {
                        let (bytes_read, mut buf) = tokio::task::spawn_blocking(move || {
                            use std::os::unix::fs::FileExt;
                            let mut buf = alloc_page(page_size);
                            let bytes_read = file.read_at(&mut buf, page_offset);
                            (bytes_read, buf)
                        })
                        .await
                        .expect("blocking task panicked");
                        let bytes_read = bytes_read?;
                        if bytes_read != page_size {
                            buf.truncate(page_size);
                        }
                        Ok::<_, io::Error>(buf.into_boxed_slice())
                    },
                )
            })
            .collect()
    }
}

/// Results of a read spanning some file range structured as a series of cached pages.
///
/// This holds pins on relevant cache entries and provides methods to examine the read bytes as
/// a series of byte slices that can be read in place or copied out e.g. to JVM heap buffers.
pub struct ReadChunks {
    pages: Vec<CacheEntry<CacheKey, PageBox>>,
    first_page_off: usize,
    last_page_len: usize,
}

impl ReadChunks {
    fn new(
        pages: Vec<CacheEntry<CacheKey, PageBox>>,
        page_size: usize,
        off: u64,
        len: u64,
    ) -> Self {
        let first_page_off = (off % page_size as u64) as usize;
        let last_page_idx = ((off + len) % page_size as u64) as usize;
        let last_page_len = if last_page_idx == 0 {
            page_size
        } else {
            last_page_idx
        };
        Self {
            pages,
            first_page_off,
            last_page_len,
        }
    }

    fn page_iter(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        self.pages.iter().enumerate().map(|(i, p)| {
            let start = if i == 0 { self.first_page_off } else { 0 };
            let end = if i == self.pages.len() - 1 {
                self.last_page_len
            } else {
                p.len()
            };
            &p[start..end]
        })
    }
}

/// Creates a write-only file, bypassing the kernel buffer cache.
///
/// On Linux this sets O_DIRECT at open time. On macOS it uses F_NOCACHE via fcntl, which
/// is the only supported mechanism for disabling the buffer cache on that platform.
fn create_direct(path: &Path) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .create(path)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let file = File::create(path)?;
        let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
        if ret == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }
}

// This should match Java's CRC32 class settings.
const CRC: Crc<u32, Table<1>> = Crc::<u32, _>::new(&CRC_32_ISO_HDLC);

pub struct FoyerIndexOutput {
    dir: Arc<FoyerDirectory>,
    file: File,
    file_id: usize,
    page: usize,
    checksum: Digest<'static, u32, Table<1>>,
    page_size: usize,
}

impl FoyerIndexOutput {
    fn new(
        dir: Arc<FoyerDirectory>,
        path: &Path,
        file_id: usize,
        page_size: usize,
    ) -> io::Result<Self> {
        let file = create_direct(path)?;
        Ok(Self {
            dir,
            file,
            file_id,
            page: 0,
            checksum: CRC.digest(),
            page_size,
        })
    }

    /// Write a single page to the end of the file.
    ///
    /// *Panics* if page.len() does not match page size.
    fn write_page(&mut self, page: &[u8]) -> io::Result<()> {
        assert_eq!(page.len(), self.page_size);
        let page = PageVec::from_slice(PAGE_ALIGNMENT, page);
        self.file.write_all(&page)?;
        self.checksum.update(&page);
        let key = CacheKey {
            file: self.file_id,
            page: self.page,
        };
        self.dir.cache.insert(
            CacheKey {
                file: self.file_id,
                page: self.page,
            },
            page.into_boxed_slice(),
        );
        self.page += 1;
        Ok(())
    }

    /// Return an ISO HDLC 32-bit CRC checksum of all of the file bytes written up to this point
    /// plus any buffered bytes that have not been flushed.
    fn checksum(&self, buffered: &[u8]) -> u32 {
        let mut digest = self.checksum.clone();
        digest.update(buffered);
        digest.finalize()
    }

    /// Write a single page to the end of the file, keeping only len bytes, then flush the file and
    /// close it.
    ///
    /// *Panics* if bytes.len() does not match page size.
    fn close(mut self, page: &[u8], len: usize) -> io::Result<()> {
        assert_eq!(page.len(), self.page_size);
        assert!(len <= self.page_size, "{len} {}", self.page_size);
        let mut page = PageVec::from_slice(PAGE_ALIGNMENT, page);
        self.file.write_all(&page)?;
        self.file
            .set_len((self.page * self.page_size + len) as u64)?;
        page.truncate(len);
        if let Err(e) = self.file.sync_all() {
            panic!("Failed to flush file: {e}");
        }
        self.dir.cache.insert(
            CacheKey {
                file: self.file_id,
                page: self.page,
            },
            {
                page.truncate(len);
                page.into_boxed_slice()
            },
        );
        Ok(())
    }
}

mod ffi {
    use super::*;

    fn path_from_ptr<'a>(p: *const u8, len: u32) -> &'a Path {
        Path::new(
            std::str::from_utf8(unsafe { std::slice::from_raw_parts::<'a, _>(p, len as usize) })
                .expect("valid utf8"),
        )
    }

    /// Returns the library version as a u32.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyerdir_version() -> u32 {
        1
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_directory_open(
        path: *const u8,
        path_len: u32,
        cache_bytes: u64,
        log_page_size: i32,
    ) -> *const FoyerDirectory {
        let path = path_from_ptr(path, path_len);
        let page_size = 1usize << log_page_size;
        Arc::into_raw(Arc::new(
            FoyerDirectory::new(path.to_owned(), cache_bytes, page_size).unwrap(),
        ))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_directory_close(dir: *const FoyerDirectory) {
        let dir = unsafe { Arc::from_raw(dir) };
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_directory_sync(dir: *const FoyerDirectory) {
        let dir = unsafe { &*dir };
        dir.sync_metadata()
            .expect("by policy, crash if sync fails.");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_directory_delete_file_id(
        dir: *const FoyerDirectory,
        relative_path: *const u8,
        len: u32,
    ) {
        // NB: caller holds a reference.
        let dir = unsafe { Arc::from_raw(dir) };
        let relative_path = path_from_ptr(relative_path, len);
        dir.delete_file_id(relative_path);
        Arc::into_raw(dir);
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_directory_create_output(
        dir: *const FoyerDirectory,
        relative_path: *const u8,
        path_len: u32,
    ) -> *mut FoyerIndexOutput {
        // NB: caller holds a reference.
        let dir = unsafe { Arc::from_raw(dir) };
        let relative_path = path_from_ptr(relative_path, path_len);
        let out = dir.new_output(relative_path).unwrap();
        Arc::into_raw(dir);
        Box::into_raw(Box::new(out))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_output_write_page(
        out: *mut FoyerIndexOutput,
        page: *const u8,
        page_len: u32,
    ) {
        let out = unsafe { &mut *out };
        let page = unsafe { std::slice::from_raw_parts(page, page_len as usize) };
        out.write_page(page)
            .expect("output write page does not propagate errors");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_output_checksum(
        out: *const FoyerIndexOutput,
        buffered: *const u8,
        buffered_len: u32,
    ) -> u32 {
        let out = unsafe { &*out };
        let buffered = unsafe { std::slice::from_raw_parts(buffered, buffered_len as usize) };
        out.checksum(buffered)
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_output_close(
        out: *mut FoyerIndexOutput,
        page: *const u8,
        page_len: u32,
        len: u32,
    ) {
        let out = unsafe { *Box::from_raw(out) };
        let page = unsafe { std::slice::from_raw_parts(page, page_len as usize) };
        out.close(page, len as usize)
            .expect("output close does not propagate errors");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_directory_create_input(
        dir: *const FoyerDirectory,
        relative_path: *const u8,
        path_len: u32,
    ) -> *mut FoyerIndexInput {
        // NB: caller holds a reference.
        let dir = unsafe { Arc::from_raw(dir) };
        let relative_path = path_from_ptr(relative_path, path_len);
        let input = dir.new_input(relative_path).unwrap();
        Arc::into_raw(dir);
        Box::into_raw(Box::new(input))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_input_read_at(
        input: *mut FoyerIndexInput,
        offset: u64,
        length: u64,
        out: *mut u8,
        out_len: u32,
    ) -> u32 {
        let input = unsafe { &*input };
        let out = unsafe { std::slice::from_raw_parts_mut(out, out_len as usize) };
        input
            .read_at(offset, out)
            .expect("input read_at does not propagate errors") as u32
    }

    /// Pointer address + length pair for byte arrays.
    #[repr(C)]
    pub struct ByteSliceAddr {
        pub addr: u64,
        pub len: u64,
    }

    impl From<&[u8]> for ByteSliceAddr {
        fn from(value: &[u8]) -> Self {
            Self {
                addr: value.as_ptr().addr() as u64,
                len: value.len() as u64,
            }
        }
    }

    /// Holds an FFI friendly representation of a series of pages and extents within each page that
    /// are used to serve a read request. This struct keeps referenced data alive until dropped.
    #[repr(C)]
    pub struct FoyerReadChunks {
        pub page_len: u64,
        pub page_extents: *const ByteSliceAddr,
        // The following fields are not visible to FFI callers.
        pages: ReadChunks,
        page_extents_vec: Vec<ByteSliceAddr>, // aliased by slices.
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_input_read_chunks(
        input: *mut FoyerIndexInput,
        offset: u64,
        length: u64,
    ) -> *const FoyerReadChunks {
        let input = unsafe { &*input };
        let pages = input
            .read_chunked(offset, length)
            .expect("input read_at does not propagate errors");
        let page_extents_vec = pages
            .page_iter()
            .map(ByteSliceAddr::from)
            .collect::<Vec<_>>();
        Box::into_raw(Box::new(FoyerReadChunks {
            page_len: page_extents_vec.len() as u64,
            page_extents: page_extents_vec.as_ptr(),
            pages,
            page_extents_vec,
        }))
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_read_chunks_drop(read_chunks: *mut FoyerReadChunks) {
        drop(unsafe { Box::from_raw(read_chunks) });
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_input_prefetch(
        input: *const FoyerIndexInput,
        offset: u64,
        length: u64,
    ) {
        let input = unsafe { &*input };
        input.prefetch(offset, length);
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_input_len(input: *mut FoyerIndexInput) -> u64 {
        let input = unsafe { &*input };
        input.len() as u64
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn foyer_index_input_close(input: *mut FoyerIndexInput) {
        drop(unsafe { Box::from_raw(input) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const PAGE_SIZE: usize = 16384;

    struct Fixture {
        // dir must be declared before _tmp so it is dropped first, releasing
        // the open directory fd before tempfile attempts to delete the path.
        dir: Arc<FoyerDirectory>,
        _tmp: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let dir = Arc::new(
                FoyerDirectory::new(tmp.path().to_path_buf(), 1 << 20, PAGE_SIZE).unwrap(),
            );
            Self { dir, _tmp: tmp }
        }
    }

    #[test]
    fn write_page_persists_bytes() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        let page: Vec<u8> = (0..PAGE_SIZE).map(|i| (i % 256) as u8).collect();
        out.write_page(&page).unwrap();

        assert_eq!(std::fs::read(f.dir.path().join("test.bin")).unwrap(), page);
    }

    #[test]
    fn write_multiple_pages_appends() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        let page0 = vec![0xAAu8; PAGE_SIZE];
        let page1 = vec![0xBBu8; PAGE_SIZE];
        out.write_page(&page0).unwrap();
        out.write_page(&page1).unwrap();

        let bytes = std::fs::read(f.dir.path().join("test.bin")).unwrap();
        assert_eq!(bytes.len(), PAGE_SIZE * 2);
        assert_eq!(&bytes[..PAGE_SIZE], page0.as_slice());
        assert_eq!(&bytes[PAGE_SIZE..], page1.as_slice());
    }

    #[test]
    fn checksum_over_written_pages() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        let page = vec![0x42u8; PAGE_SIZE];
        out.write_page(&page).unwrap();

        assert_eq!(out.checksum(&[]), CRC.checksum(&page));
    }

    #[test]
    fn checksum_includes_buffered_bytes() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        let page = vec![0x11u8; PAGE_SIZE];
        let buffered = vec![0x22u8; 100];
        out.write_page(&page).unwrap();

        let mut all = page.clone();
        all.extend_from_slice(&buffered);
        assert_eq!(out.checksum(&buffered), CRC.checksum(&all));
    }

    #[test]
    fn close_produces_exact_file_length() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        out.write_page(&vec![0xAAu8; PAGE_SIZE]).unwrap();

        let len = 137;
        out.close(&vec![0xBBu8; PAGE_SIZE], len).unwrap();

        let file_len = std::fs::metadata(f.dir.path().join("test.bin"))
            .unwrap()
            .len();
        assert_eq!(file_len, (PAGE_SIZE + len) as u64);
    }

    #[test]
    fn close_writes_correct_content() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        let page: Vec<u8> = (0..PAGE_SIZE).map(|i| (i % 256) as u8).collect();
        out.write_page(&page).unwrap();

        let last_page = vec![0xFFu8; PAGE_SIZE];
        let len = 512;
        out.close(&last_page, len).unwrap();

        let bytes = std::fs::read(f.dir.path().join("test.bin")).unwrap();
        assert_eq!(bytes.len(), PAGE_SIZE + len);
        assert_eq!(&bytes[..PAGE_SIZE], page.as_slice());
        assert_eq!(&bytes[PAGE_SIZE..], &last_page[..len]);
    }

    #[test]
    fn close_only_partial_page() {
        let f = Fixture::new();
        let out = f.dir.new_output(Path::new("test.bin")).unwrap();

        let len = 200;
        out.close(&vec![0xCCu8; PAGE_SIZE], len).unwrap();

        let file_len = std::fs::metadata(f.dir.path().join("test.bin"))
            .unwrap()
            .len();
        assert_eq!(file_len, len as u64);
    }

    #[test]
    fn close_zero_len_trims_to_full_pages() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();

        out.write_page(&vec![0xAAu8; PAGE_SIZE]).unwrap();
        out.close(&vec![0xBBu8; PAGE_SIZE], 0).unwrap();

        let file_len = std::fs::metadata(f.dir.path().join("test.bin"))
            .unwrap()
            .len();
        assert_eq!(file_len, PAGE_SIZE as u64);
    }

    #[test]
    #[should_panic]
    fn write_page_panics_on_wrong_size() {
        let f = Fixture::new();
        let mut out = f.dir.new_output(Path::new("test.bin")).unwrap();
        out.write_page(&[0u8; 512]).unwrap();
    }

    #[test]
    #[should_panic]
    fn close_panics_on_wrong_page_size() {
        let f = Fixture::new();
        let out = f.dir.new_output(Path::new("test.bin")).unwrap();
        out.close(&[0u8; 512], 100).unwrap();
    }

    #[test]
    fn input_len_exact_page() {
        let f = Fixture::new();
        std::fs::write(f.dir.path().join("test.bin"), vec![0u8; PAGE_SIZE]).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();
        assert_eq!(input.len(), PAGE_SIZE);
    }

    #[test]
    fn input_len_sub_page() {
        let f = Fixture::new();
        std::fs::write(f.dir.path().join("test.bin"), vec![0u8; 100]).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();
        assert_eq!(input.len(), 100);
    }

    #[test]
    fn input_len_multi_page() {
        let f = Fixture::new();
        std::fs::write(
            f.dir.path().join("test.bin"),
            vec![0u8; PAGE_SIZE * 3 + 500],
        )
        .unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();
        assert_eq!(input.len(), PAGE_SIZE * 3 + 500);
    }

    #[test]
    fn prefetch_zero_len_is_noop() {
        let f = Fixture::new();
        std::fs::write(f.dir.path().join("test.bin"), vec![0x42u8; PAGE_SIZE]).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();
        input.prefetch(0, 0); // page_range returns None, nothing should happen
    }

    #[test]
    fn prefetch_offset_past_eof_is_noop() {
        let f = Fixture::new();
        std::fs::write(f.dir.path().join("test.bin"), vec![0x42u8; PAGE_SIZE]).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();
        input.prefetch(PAGE_SIZE as u64 + 1, 100); // past EOF, page_range returns None
    }

    #[test]
    fn prefetch_empty_file_is_noop() {
        let f = Fixture::new();
        std::fs::write(f.dir.path().join("test.bin"), b"").unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();
        input.prefetch(0, 100); // pages == 0, page_range returns None
    }

    #[test]
    fn prefetch_single_page_cold_data_remains_readable() {
        let f = Fixture::new();
        let data: Vec<u8> = (0..PAGE_SIZE).map(|i| (i % 256) as u8).collect();
        std::fs::write(f.dir.path().join("test.bin"), &data).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();

        input.prefetch(0, PAGE_SIZE as u64);

        let mut buf = alloc_page(PAGE_SIZE);
        let n = input.read_at(0, &mut buf).unwrap();
        assert_eq!(n, PAGE_SIZE as u64);
        assert_eq!(&buf[..], data.as_slice());
    }

    #[test]
    fn prefetch_multi_page_cold_data_remains_readable() {
        let f = Fixture::new();
        let data: Vec<u8> = (0..PAGE_SIZE * 3).map(|i| (i % 251) as u8).collect();
        std::fs::write(f.dir.path().join("test.bin"), &data).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();

        input.prefetch(0, (PAGE_SIZE * 3) as u64);

        let mut buf = alloc_page(PAGE_SIZE);
        for page_id in 0..3 {
            let n = input.read_at((page_id * PAGE_SIZE) as u64, &mut buf).unwrap();
            assert_eq!(n, PAGE_SIZE as u64);
            assert_eq!(
                &buf[..],
                &data[page_id * PAGE_SIZE..(page_id + 1) * PAGE_SIZE]
            );
        }
    }

    #[test]
    fn prefetch_warm_pages_data_still_readable() {
        // Read a page into cache first, then prefetch the same range.
        // The need_await == false path should be exercised and reads must still return correct data.
        let f = Fixture::new();
        let data: Vec<u8> = (0..PAGE_SIZE).map(|i| (i % 256) as u8).collect();
        std::fs::write(f.dir.path().join("test.bin"), &data).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();

        let mut buf = alloc_page(PAGE_SIZE);
        input.read_at(0, &mut buf).unwrap();

        // Page is now in cache; prefetch should skip the spawn and be a no-op.
        input.prefetch(0, PAGE_SIZE as u64);

        let n = input.read_at(0, &mut buf).unwrap();
        assert_eq!(n, PAGE_SIZE as u64);
        assert_eq!(&buf[..], data.as_slice());
    }

    #[test]
    fn prefetch_partial_range_within_single_page() {
        // A small window inside page 0 should only touch that one page.
        let f = Fixture::new();
        let data: Vec<u8> = (0..PAGE_SIZE).map(|i| (i % 256) as u8).collect();
        std::fs::write(f.dir.path().join("test.bin"), &data).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();

        input.prefetch(128, 512);

        let mut buf = alloc_page(PAGE_SIZE);
        let n = input.read_at(0, &mut buf).unwrap();
        assert_eq!(n, PAGE_SIZE as u64);
        assert_eq!(&buf[..], data.as_slice());
    }

    #[test]
    fn prefetch_range_spanning_page_boundary() {
        // A range that straddles two pages should queue both pages.
        let f = Fixture::new();
        let page0 = vec![0xAAu8; PAGE_SIZE];
        let page1 = vec![0xBBu8; PAGE_SIZE];
        let mut data = page0.clone();
        data.extend_from_slice(&page1);
        std::fs::write(f.dir.path().join("test.bin"), &data).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();

        // Straddle the boundary: last 64 bytes of page 0 + first 64 bytes of page 1.
        let off = (PAGE_SIZE - 64) as u64;
        input.prefetch(off, 128);

        let mut buf = alloc_page(PAGE_SIZE);
        let n0 = input.read_at(0, &mut buf).unwrap();
        assert_eq!(n0, PAGE_SIZE as u64);
        assert_eq!(&buf[..], page0.as_slice());

        let n1 = input.read_at(PAGE_SIZE as u64, &mut buf).unwrap();
        assert_eq!(n1, PAGE_SIZE as u64);
        assert_eq!(&buf[..], page1.as_slice());
    }

    #[test]
    fn prefetch_sub_page_file_is_noop_at_eof() {
        // File is smaller than one page; prefetch starting at the file's end should be a no-op.
        let f = Fixture::new();
        let data = vec![0x11u8; 512];
        std::fs::write(f.dir.path().join("test.bin"), &data).unwrap();
        let input = f.dir.new_input(Path::new("test.bin")).unwrap();

        input.prefetch(512, 100); // off == file_len, page_range returns None
    }
}

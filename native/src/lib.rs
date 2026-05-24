// XXX remove.
#![allow(unused)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, RwLock};

use crc::{CRC_32_ISO_HDLC, Crc, Digest, Table};

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
    runtime: tokio::runtime::Runtime,
    file_ids: FileIdMap,
}

impl FoyerDirectory {
    fn new(path: PathBuf, cache_size: u64, page_size: usize) -> io::Result<Self> {
        let dir_file = File::open(&path)?;
        let cache = foyer::CacheBuilder::new(cache_size as usize)
            .with_weighter(|_key: &CacheKey, value: &Box<[u8]>| {
                std::mem::size_of::<CacheKey>() + value.len()
            })
            .build();
        let num_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .max_blocking_threads(num_cores)
            .build()?;
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

    // TODO(FFI): wire into Directory.delete().
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

    // TODO(FFI): wire into Directory.open_output().
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

    fn read_page(&self, page_id: usize, out: &mut [u8]) -> io::Result<usize> {
        assert!(
            page_id < self.pages,
            "page_id {page_id} >= pages {}",
            self.pages
        );
        assert!(
            out.as_ptr() as usize % 4096 == 0,
            "output buffer must be 4096-byte aligned"
        );

        let key = CacheKey {
            file: self.file_id,
            page: page_id,
        };
        let offset = (page_id * self.dir.page_size) as u64;
        let len = out.len();
        let file = Arc::clone(&self.file);

        let entry = self
            .dir
            .runtime
            .block_on(self.dir.cache.get_or_fetch(&key, move || async move {
                // TODO: improve handling for the last page. Interior pages must read a whole block,
                // the last page must issue a whole block read but can't read everything.
                let (bytes_read, mut buf) = tokio::task::spawn_blocking(move || {
                    use std::os::unix::fs::FileExt;
                    let mut buf = vec![0u8; len];
                    let bytes_read = file.read_at(&mut buf, offset);
                    (bytes_read, buf)
                })
                .await
                .expect("blocking task panicked");

                let bytes_read = bytes_read?;
                if bytes_read != len {
                    buf.truncate(len);
                }
                Ok::<_, io::Error>(buf.into_boxed_slice())
            }))
            .map_err(io::Error::other)?;

        let bytes: &[u8] = entry.value();
        out[..bytes.len()].copy_from_slice(bytes);
        Ok(bytes.len())
    }

    fn len(&self) -> usize {
        self.file_len
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
        self.file.write_all(page)?;
        self.checksum.update(page);
        let key = CacheKey {
            file: self.file_id,
            page: self.page,
        };
        self.dir.cache.insert(
            CacheKey {
                file: self.file_id,
                page: self.page,
            },
            Box::from(page),
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
        assert!(len < self.page_size, "{len} {}", self.page_size);
        self.file.write_all(page)?;
        self.file
            .set_len((self.page * self.page_size + len) as u64)?;
        if let Err(e) = self.file.sync_all() {
            panic!("Failed to flush file: {e}");
        }
        self.dir.cache.insert(
            CacheKey {
                file: self.file_id,
                page: self.page,
            },
            Box::from(&page[..len]),
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
    pub unsafe extern "C" fn foyer_index_input_read_page(
        input: *mut FoyerIndexInput,
        page_id: u32,
        out: *mut u8,
        out_len: u32,
    ) -> u32 {
        let input = unsafe { &*input };
        let out = unsafe { std::slice::from_raw_parts_mut(out, out_len as usize) };
        input
            .read_page(page_id as usize, out)
            .expect("input read page does not propagate errors") as u32
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
    #[should_panic]
    fn close_panics_when_len_equals_page_size() {
        let f = Fixture::new();
        let out = f.dir.new_output(Path::new("test.bin")).unwrap();
        out.close(&vec![0u8; PAGE_SIZE], PAGE_SIZE).unwrap();
    }
}

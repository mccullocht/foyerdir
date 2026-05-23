// XXX remove.
#![allow(unused)]

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
        })
    }

    fn sync_metadata(&self) -> io::Result<()> {
        self.dir_file.sync_all()
    }
}

pub struct FoyerIndexInput {
    dir: Arc<FoyerDirectory>,
    file: File,
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
    ) -> io::Result<Arc<Self>> {
        let file = open_direct(path)?;
        let file_len = file.metadata()?.len() as usize;
        let pages = file_len.div_ceil(page_size);
        let last_page_size = if pages == 0 {
            0
        } else {
            let r = file_len % page_size;
            if r == 0 { page_size } else { r }
        };
        Ok(Arc::new(Self {
            dir,
            file,
            file_len,
            file_id,
            pages,
            last_page_size,
        }))
    }

    fn read_page(self: &Arc<Self>, page_id: usize, out: &mut [u8]) -> io::Result<usize> {
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
        let this = Arc::clone(self);

        let entry = self
            .dir
            .runtime
            .block_on(self.dir.cache.get_or_fetch(&key, move || async move {
                let (bytes_read, buf) = tokio::task::spawn_blocking(move || {
                    use std::os::unix::fs::FileExt;
                    let mut buf = vec![0u8; len].into_boxed_slice();
                    let bytes_read = this.file.read_at(&mut buf, offset);
                    (bytes_read, buf)
                })
                .await
                .expect("blocking task panicked");

                let bytes_read = bytes_read?;
                Ok::<Box<[u8]>, io::Error>(if bytes_read == len {
                    buf
                } else {
                    buf[..bytes_read].into()
                })
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
    let page_size = 1usize << log_page_size;
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

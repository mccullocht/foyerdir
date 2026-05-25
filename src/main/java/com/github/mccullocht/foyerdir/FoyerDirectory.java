package com.github.mccullocht.foyerdir;

import java.io.FilterOutputStream;
import java.io.IOException;
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.ValueLayout;
import java.nio.charset.StandardCharsets;
import java.nio.file.FileAlreadyExistsException;
import java.nio.file.Files;
import java.nio.file.OpenOption;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.nio.file.StandardOpenOption;
import java.util.Collection;
import java.util.Collections;
import java.util.Set;
import java.util.concurrent.atomic.AtomicLong;

import org.apache.lucene.store.BaseDirectory;
import org.apache.lucene.store.IOContext;
import org.apache.lucene.store.IndexInput;
import org.apache.lucene.store.IndexOutput;
import org.apache.lucene.store.NativeFSLockFactory;
import org.apache.lucene.store.OutputStreamIndexOutput;

public final class FoyerDirectory extends BaseDirectory {
    private static final int CHUNK_SIZE = 8192;

    private final Path path;
    private final int pageSize;
    private final AtomicLong nextTempFileCounter = new AtomicLong();

    private final Arena arena;
    private final MemorySegment nativeHandle;

    /**
     * Open a new directory backed by an explicit in-memory Foyer cache.
     *
     * <p>Data is persisted into a directory at {@code path}, which will be created if it does not
     * exist. The cache will be maintained at no more than {@code cacheBytes}. Each file will be
     * cached in units of up to {@code 1 << logPageSize} bytes.
     *
     * <p>{@code logPageSize} must be >= 12.
     */
    public FoyerDirectory(Path path, long cacheBytes, int logPageSize) throws IOException {
        super(NativeFSLockFactory.INSTANCE);
        this.path = path;
        this.pageSize = 1 << logPageSize;
        if (logPageSize < 12) {
            throw new IllegalArgumentException("logPageSize must be at least 12");
        }
        Files.createDirectories(path);
        this.arena = Arena.ofShared();
        byte[] pathBytes = path.toString().getBytes(StandardCharsets.UTF_8);
        try (var tmp = Arena.ofConfined()) {
            MemorySegment pathSeg = tmp.allocateFrom(ValueLayout.JAVA_BYTE, pathBytes);
            this.nativeHandle = ((MemorySegment) FoyerDirectoryBindings.DIRECTORY_OPEN.invokeExact(
                    pathSeg, pathBytes.length, cacheBytes, logPageSize))
                    .reinterpret(0, this.arena, ptr -> {
                        try {
                            FoyerDirectoryBindings.DIRECTORY_CLOSE.invokeExact(ptr);
                        } catch (Throwable t) {
                            throw new RuntimeException(t);
                        }
                    });
        } catch (Throwable t) {
            this.arena.close();
            throw new RuntimeException(t);

        }
    }

    public static int version() throws Throwable {
        return (int) FoyerDirectoryBindings.VERSION.invokeExact();
    }

    @Override
    public String[] listAll() throws IOException {
        ensureOpen();
        try (var stream = Files.list(path)) {
            return stream
                    .filter(p -> !Files.isDirectory(p))
                    .map(p -> p.getFileName().toString())
                    .sorted()
                    .toArray(String[]::new);
        }
    }

    @Override
    public void deleteFile(String name) throws IOException {
        ensureOpen();
        Files.delete(path.resolve(name));
    }

    @Override
    public long fileLength(String name) throws IOException {
        ensureOpen();
        return Files.size(path.resolve(name));
    }

    @Override
    public IndexOutput createOutput(String name, IOContext context) throws IOException {
        ensureOpen();
        return newIndexOutput(name, StandardOpenOption.WRITE, StandardOpenOption.CREATE_NEW);
    }

    @Override
    public IndexOutput createTempOutput(String prefix, String suffix, IOContext context) throws IOException {
        ensureOpen();
        while (true) {
            try {
                String name = getTempFileName(prefix, suffix, nextTempFileCounter.getAndIncrement());
                return newIndexOutput(name, StandardOpenOption.WRITE, StandardOpenOption.CREATE_NEW);
            } catch (@SuppressWarnings("unused") FileAlreadyExistsException faee) {
                // Retry with next incremented name
            }
        }
    }

    private OutputStreamIndexOutput newIndexOutput(String name, OpenOption... options) throws IOException {
        Path file = path.resolve(name);
        return new OutputStreamIndexOutput(
                "FoyerIndexOutput(path=\"" + file + "\")",
                name,
                new FilterOutputStream(Files.newOutputStream(file, options)) {
                    @Override
                    public void write(byte[] b, int offset, int length) throws IOException {
                        while (length > 0) {
                            int chunk = Math.min(length, CHUNK_SIZE);
                            out.write(b, offset, chunk);
                            length -= chunk;
                            offset += chunk;
                        }
                    }
                },
                CHUNK_SIZE);
    }

    @Override
    public void sync(Collection<String> names) throws IOException {
        // NB: files will be synced before close, and if sync fails we will crash.
        syncMetaData();
    }

    @Override
    public void syncMetaData() throws IOException {
        ensureOpen();
        try {
            FoyerDirectoryBindings.DIRECTORY_SYNC.invokeExact(nativeHandle);
        } catch (Throwable t) {
            if (t instanceof IOException ioe)
                throw ioe;
            throw new IOException(t);

        }
    }

    @Override
    public void rename(String source, String dest) throws IOException {
        ensureOpen();
        Files.move(path.resolve(source), path.resolve(dest), StandardCopyOption.ATOMIC_MOVE);
    }

    @Override
    public IndexInput openInput(String name, IOContext context) throws IOException {
        ensureOpen();
        byte[] nameBytes = name.getBytes(StandardCharsets.UTF_8);
        Arena inputArena = Arena.ofAuto();
        MemorySegment ptr;
        try (var tmp = Arena.ofConfined()) {
            MemorySegment nameSeg = tmp.allocateFrom(ValueLayout.JAVA_BYTE, nameBytes);
            ptr = ((MemorySegment) FoyerDirectoryBindings.DIRECTORY_CREATE_INPUT.invokeExact(
                    nativeHandle, nameSeg, nameBytes.length))
                    .reinterpret(0, inputArena, p -> {
                        try {
                            FoyerDirectoryBindings.INDEX_INPUT_CLOSE.invokeExact(p);
                        } catch (Throwable t) {
                            throw new RuntimeException(t);
                        }
                    });
        } catch (Throwable t) {
            throw new RuntimeException(t);
        }
        return new FoyerIndexInput("FoyerIndexInput(path=\"" + path.resolve(name) + "\")", inputArena, ptr, pageSize);
    }

    @Override
    public Set<String> getPendingDeletions() throws IOException {
        ensureOpen();
        return Collections.emptySet();
    }

    @Override

    public void close() throws IOException {
        // NB: this "closes" the directory on the native side. This should not cause crashes as the
        // native directory is refcounted by all inputs/outputs.
        this.isOpen = false;
        this.arena.close();
    }
}

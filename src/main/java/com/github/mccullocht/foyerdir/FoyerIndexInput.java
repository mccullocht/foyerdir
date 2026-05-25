package com.github.mccullocht.foyerdir;

import java.io.IOException;
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.ByteBuffer;

import org.apache.lucene.store.AlreadyClosedException;
import org.apache.lucene.store.IndexInput;

final class FoyerIndexInput extends IndexInput {
    private static class NativeHandle {
        private final Arena arena;
        private final MemorySegment ptr;
        private volatile boolean isClosed = false;

        NativeHandle(Arena arena, MemorySegment ptr) {
            this.arena = arena;
            this.ptr = ptr;
        }
    }

    private final NativeHandle handle;
    private final boolean isClone;
    private final int pageSize;
    private final long sliceOffset;
    private final long sliceLength;
    private long filePointer = 0;
    private ByteBuffer buf;
    // Absolute file position of buf[0]. -1 means no page is loaded.
    private long bufPageStart = -1;

    FoyerIndexInput(String resourceDescription, Arena arena, MemorySegment ptr, int pageSize) throws IOException {
        super(resourceDescription);
        this.handle = new NativeHandle(arena, ptr);
        this.isClone = false;
        this.pageSize = pageSize;
        this.sliceOffset = 0;
        try {
            this.sliceLength = (long) FoyerDirectoryBindings.INDEX_INPUT_LEN.invokeExact(ptr);
        } catch (Throwable t) {
            throw new IOException(t);
        }
    }

    private FoyerIndexInput(String resourceDescription, NativeHandle handle, int pageSize, long sliceOffset,
            long sliceLength) {
        super(resourceDescription);
        this.handle = handle;
        this.isClone = true;
        this.pageSize = pageSize;
        this.sliceOffset = sliceOffset;
        this.sliceLength = sliceLength;
    }

    // Ensures the page covering absPos is loaded into buf. Reads are page-aligned:
    // the buffer always starts at (absPos / pageSize) * pageSize, so bytes before
    // absPos within the page are read but unused (dead-heading).
    private void loadPage(long absPos) throws IOException {
        if (handle.isClosed) {
            throw new AlreadyClosedException("IndexInput already closed: " + this);
        }
        long pageId = absPos / pageSize;
        long pageStart = pageId * pageSize;
        if (pageStart == bufPageStart) {
            return;
        }
        if (buf == null) {
            buf = ByteBuffer.allocateDirect(pageSize);
        }
        buf.clear();
        try {
            int read = (int) FoyerDirectoryBindings.INDEX_INPUT_READ_PAGE.invokeExact(
                    handle.ptr, pageId, MemorySegment.ofBuffer(buf), pageSize);
            buf.limit(read);
        } catch (Throwable t) {
            throw new IOException(t);
        }
        bufPageStart = pageStart;
    }

    @Override
    public byte readByte() throws IOException {
        if (filePointer >= sliceLength) {
            throw new IOException("read past end of slice");
        }
        long absPos = sliceOffset + filePointer;
        if (buf == null || absPos < bufPageStart || absPos >= bufPageStart + buf.limit()) {
            loadPage(absPos);
        }
        filePointer++;
        return buf.get((int) (absPos - bufPageStart));
    }

    @Override
    public void readBytes(byte[] b, int offset, int length) throws IOException {
        if (filePointer + length > sliceLength) {
            throw new IOException("read past end of slice");
        }
        while (length > 0) {
            long absPos = sliceOffset + filePointer;
            if (buf == null || absPos < bufPageStart || absPos >= bufPageStart + buf.limit()) {
                loadPage(absPos);
            }
            int offsetInBuf = (int) (absPos - bufPageStart);
            int toCopy = Math.min(buf.limit() - offsetInBuf, length);
            buf.get(offsetInBuf, b, offset, toCopy);
            filePointer += toCopy;
            offset += toCopy;
            length -= toCopy;
        }
    }

    @Override
    public long getFilePointer() {
        return filePointer;
    }

    @Override
    public void seek(long pos) throws IOException {
        if (pos < 0 || pos > sliceLength) {
            throw new IllegalArgumentException("seek out of range: " + pos);
        }
        filePointer = pos;
    }

    @Override
    public long length() {
        return sliceLength;
    }

    @Override
    public void close() throws IOException {
        if (!isClone) {
            handle.isClosed = true;
        }
    }

    @Override
    public IndexInput slice(String sliceDescription, long offset, long length) throws IOException {
        if (offset < 0 || length < 0 || offset + length > sliceLength) {
            throw new IllegalArgumentException(
                    "invalid slice: offset=" + offset + " length=" + length + " of " + sliceLength);
        }
        return new FoyerIndexInput(sliceDescription, handle, pageSize, sliceOffset + offset, length);
    }

    @Override
    public FoyerIndexInput clone() {
        FoyerIndexInput clone = new FoyerIndexInput(toString(), handle, pageSize, sliceOffset, sliceLength);
        clone.filePointer = filePointer;
        return clone;

    }
}

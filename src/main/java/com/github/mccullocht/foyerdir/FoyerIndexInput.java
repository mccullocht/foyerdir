package com.github.mccullocht.foyerdir;

import java.io.EOFException;
import java.io.IOException;
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;

import org.apache.lucene.store.AlreadyClosedException;
import org.apache.lucene.store.IndexInput;
import org.apache.lucene.store.RandomAccessInput;

final class FoyerIndexInput extends IndexInput implements RandomAccessInput {
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
            buf = ByteBuffer.allocateDirect(pageSize).order(ByteOrder.LITTLE_ENDIAN);
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
            throw new EOFException("read past end of slice");
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
            throw new EOFException("read past end of slice");
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
    public short readShort() throws IOException {
        long absPos = sliceOffset + filePointer;
        if (filePointer + 2 <= sliceLength && buf != null && absPos >= bufPageStart && absPos + 2 <= bufPageStart + buf.limit()) {
            filePointer += 2;
            return buf.getShort((int) (absPos - bufPageStart));
        }
        return super.readShort();
    }

    @Override
    public int readInt() throws IOException {
        long absPos = sliceOffset + filePointer;
        if (filePointer + 4 <= sliceLength && buf != null && absPos >= bufPageStart && absPos + 4 <= bufPageStart + buf.limit()) {
            filePointer += 4;
            return buf.getInt((int) (absPos - bufPageStart));
        }
        return super.readInt();
    }

    @Override
    public long readLong() throws IOException {
        long absPos = sliceOffset + filePointer;
        if (filePointer + 8 <= sliceLength && buf != null && absPos >= bufPageStart && absPos + 8 <= bufPageStart + buf.limit()) {
            filePointer += 8;
            return buf.getLong((int) (absPos - bufPageStart));
        }
        return super.readLong();
    }

    @Override
    public byte readByte(long pos) throws IOException {
        if (pos < 0 || pos >= sliceLength) {
            throw new EOFException("read past end of slice");
        }
        long absPos = sliceOffset + pos;
        if (buf == null || absPos < bufPageStart || absPos >= bufPageStart + buf.limit()) {
            loadPage(absPos);
        }
        return buf.get((int) (absPos - bufPageStart));
    }

    @Override
    public short readShort(long pos) throws IOException {
        long absPos = sliceOffset + pos;
        if (pos + 2 <= sliceLength && buf != null && absPos >= bufPageStart && absPos + 2 <= bufPageStart + buf.limit()) {
            return buf.getShort((int) (absPos - bufPageStart));
        }
        return (short) ((readByte(pos) & 0xFF) | ((readByte(pos + 1) & 0xFF) << 8));
    }

    @Override
    public int readInt(long pos) throws IOException {
        long absPos = sliceOffset + pos;
        if (pos + 4 <= sliceLength && buf != null && absPos >= bufPageStart && absPos + 4 <= bufPageStart + buf.limit()) {
            return buf.getInt((int) (absPos - bufPageStart));
        }
        return (readByte(pos) & 0xFF) | ((readByte(pos + 1) & 0xFF) << 8)
                | ((readByte(pos + 2) & 0xFF) << 16) | ((readByte(pos + 3) & 0xFF) << 24);
    }

    @Override
    public long readLong(long pos) throws IOException {
        long absPos = sliceOffset + pos;
        if (pos + 8 <= sliceLength && buf != null && absPos >= bufPageStart && absPos + 8 <= bufPageStart + buf.limit()) {
            return buf.getLong((int) (absPos - bufPageStart));
        }
        return (readByte(pos) & 0xFFL) | ((readByte(pos + 1) & 0xFFL) << 8)
                | ((readByte(pos + 2) & 0xFFL) << 16) | ((readByte(pos + 3) & 0xFFL) << 24)
                | ((readByte(pos + 4) & 0xFFL) << 32) | ((readByte(pos + 5) & 0xFFL) << 40)
                | ((readByte(pos + 6) & 0xFFL) << 48) | ((readByte(pos + 7) & 0xFFL) << 56);
    }

    @Override
    public void prefetch(long offset, long length) throws IOException {
        if (handle.isClosed) {
            return;
        }
        try {
            FoyerDirectoryBindings.INDEX_INPUT_PREFETCH.invokeExact(handle.ptr, sliceOffset + offset, length);
        } catch (Throwable t) {
            throw new IOException(t);
        }
    }

    @Override
    public long getFilePointer() {
        return filePointer;
    }

    @Override
    public void seek(long pos) throws IOException {
        if (pos < 0) {
            throw new IllegalArgumentException("seek position must be >= 0: " + pos);
        }
        if (pos > sliceLength) {
            throw new EOFException("seek past EOF: " + pos + " > " + sliceLength);
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
        if (offset < 0 || length < 0 || offset > sliceLength || length > sliceLength - offset) {
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

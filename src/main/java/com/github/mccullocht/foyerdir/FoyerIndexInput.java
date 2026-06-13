package com.github.mccullocht.foyerdir;

import java.io.EOFException;
import java.io.IOException;
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.lang.foreign.ValueLayout;
import java.nio.ByteBuffer;

import org.apache.lucene.store.AlreadyClosedException;
import org.apache.lucene.store.BufferedIndexInput;
import org.apache.lucene.store.IndexInput;

final class FoyerIndexInput extends BufferedIndexInput {
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
    private boolean isClone;
    private final long sliceOffset;
    private final long sliceLength;

    FoyerIndexInput(String resourceDescription, Arena arena, MemorySegment ptr, int pageSize) throws IOException {
        super(resourceDescription, pageSize);
        this.handle = new NativeHandle(arena, ptr);
        this.isClone = false;
        this.sliceOffset = 0;
        try {
            this.sliceLength = (long) FoyerDirectoryBindings.INDEX_INPUT_LEN.invokeExact(ptr);
        } catch (Throwable t) {
            throw new IOException(t);
        }
    }

    private FoyerIndexInput(String resourceDescription, NativeHandle handle, int bufferSize, long sliceOffset,
            long sliceLength) {
        super(resourceDescription, bufferSize);
        this.handle = handle;
        this.isClone = true;
        this.sliceOffset = sliceOffset;
        this.sliceLength = sliceLength;
    }

    @Override
    protected void readInternal(ByteBuffer b) throws IOException {
        if (handle.isClosed) {
            throw new AlreadyClosedException("IndexInput already closed: " + this);
        }
        long pos = getFilePointer() + sliceOffset;
        long length = b.remaining();
        MemorySegment rawChunks;
        try {
            rawChunks = (MemorySegment) FoyerDirectoryBindings.INDEX_INPUT_READ_CHUNKS.invokeExact(
                    handle.ptr, pos, length);
        } catch (Throwable t) {
            throw new IOException(t);
        }
        // FoyerReadChunks is repr(C) with page_len: u64 at offset 0, page_extents: *const ByteSliceAddr at offset 8.
        MemorySegment header = rawChunks.reinterpret(Long.BYTES * 2);
        try {
            long pageLen = header.get(ValueLayout.JAVA_LONG, 0);
            // Each ByteSliceAddr is repr(C) with addr: u64, len: u64 — 16 bytes each.
            MemorySegment extents = header.get(ValueLayout.ADDRESS, Long.BYTES).reinterpret(pageLen * Long.BYTES * 2);
            MemorySegment dst = MemorySegment.ofBuffer(b);
            long dstOffset = 0;
            for (long i = 0; i < pageLen; i++) {
                long addr = extents.get(ValueLayout.JAVA_LONG, i * Long.BYTES * 2);
                long extentLen = extents.get(ValueLayout.JAVA_LONG, i * Long.BYTES * 2 + Long.BYTES);
                MemorySegment src = MemorySegment.ofAddress(addr).reinterpret(extentLen);
                MemorySegment.copy(src, 0L, dst, dstOffset, extentLen);
                dstOffset += extentLen;
            }
            b.position(b.position() + (int) dstOffset);
        } finally {
            try {
                FoyerDirectoryBindings.INDEX_INPUT_READ_CHUNKS_DROP.invokeExact(rawChunks);
            } catch (Throwable t) {
                throw new IOException(t);
            }
        }
    }

    @Override
    protected void seekInternal(long pos) throws IOException {
        if (handle.isClosed) {
            throw new AlreadyClosedException("IndexInput already closed: " + this);
        }
        if (pos > length()) {
            throw new EOFException("read past EOF: pos=" + pos + " vs length=" + length() + ": " + this);
        }
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
        return new FoyerIndexInput(sliceDescription, handle, getBufferSize(), sliceOffset + offset, length);
    }

    @Override
    public FoyerIndexInput clone() {
        FoyerIndexInput clone = (FoyerIndexInput) super.clone();
        clone.isClone = true;
        return clone;
    }
}

package com.github.mccullocht.foyerdir;

import java.io.IOException;
import java.lang.foreign.Arena;
import java.lang.foreign.MemorySegment;
import java.nio.ByteBuffer;

import org.apache.lucene.store.BufferedIndexInput;
import org.apache.lucene.store.IndexInput;

final class FoyerIndexInput extends BufferedIndexInput {
    private static class NativeHandle {
        // XXX when we reinterpret the output of the function we will register cleanup to close or
        // free the underlying native index input.
        private final Arena arena;
        private final MemorySegment ptr;
        private volatile boolean closed = false;

        NativeHandle(Arena arena, MemorySegment ptr) {
            this.arena = arena;
            this.ptr = ptr;
        }
    }

    private final NativeHandle handle;
    private final int pageSize;
    private long pos = 0;

    FoyerIndexInput(String resourceDescription, Arena arena, MemorySegment ptr, int pageSize) {
        super(resourceDescription, pageSize);
        this.handle = new NativeHandle(arena, ptr);
        this.pageSize = pageSize;
    }

    @Override
    protected void readInternal(ByteBuffer b) throws IOException {
        long page = this.pos / this.pageSize;
        try {
            int read = (int) FoyerDirectoryBindings.INDEX_INPUT_READ_PAGE.invokeExact(this.handle.ptr, page,
                    MemorySegment.ofBuffer(b), b.capacity());
            b.position((int) (this.pos % this.pageSize));
            b.limit(read);
        } catch (Throwable t) {
            throw new RuntimeException(t);
        }
    }

    @Override
    protected void seekInternal(long pos) throws IOException {
        if (pos >= length()) {
            throw new IllegalArgumentException("cannot seek to pos beyond end of file");
        }
        this.pos = pos;
    }

    @Override
    public void close() throws IOException {
        throw new UnsupportedOperationException("unimplemented");
    }

    @Override
    public long length() {
        throw new UnsupportedOperationException("unimplemented");
    }

    @Override
    public IndexInput slice(String sliceDescription, long offset, long length) throws IOException {
        throw new UnsupportedOperationException("unimplemented");
    }

    @Override
    public FoyerIndexInput clone() {
        throw new UnsupportedOperationException("unimplemented");
    }
}

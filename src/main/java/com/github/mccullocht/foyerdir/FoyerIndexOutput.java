package com.github.mccullocht.foyerdir;

import java.io.IOException;
import java.lang.foreign.MemorySegment;
import java.nio.ByteBuffer;

import org.apache.lucene.store.IndexOutput;

final class FoyerIndexOutput extends IndexOutput {
    private final MemorySegment ptr;
    private final int pageSize;
    private final ByteBuffer buf;
    private long filePointer = 0;
    private boolean closed = false;

    FoyerIndexOutput(String resourceDescription, String name, MemorySegment ptr, int pageSize) {
        super(resourceDescription, name);
        this.ptr = ptr;
        this.pageSize = pageSize;
        this.buf = ByteBuffer.allocateDirect(pageSize);
    }

    @Override
    public void writeByte(byte b) throws IOException {
        buf.put(b);
        filePointer++;
        if (!buf.hasRemaining()) {
            flushPage();
        }
    }

    @Override
    public void writeBytes(byte[] b, int offset, int length) throws IOException {
        while (length > 0) {
            int toWrite = Math.min(buf.remaining(), length);
            buf.put(b, offset, toWrite);
            filePointer += toWrite;
            offset += toWrite;
            length -= toWrite;
            if (!buf.hasRemaining()) {
                flushPage();
            }
        }
    }

    private void flushPage() throws IOException {
        buf.flip();
        try {
            FoyerDirectoryBindings.INDEX_OUTPUT_WRITE_PAGE.invokeExact(
                    ptr, MemorySegment.ofBuffer(buf), pageSize);
        } catch (Throwable t) {
            throw new IOException(t);
        }
        buf.clear();
    }

    @Override
    public long getFilePointer() {
        return filePointer;
    }

    @Override
    public long getChecksum() throws IOException {
        int bufferedLen = buf.position();
        ByteBuffer snapshot = buf.duplicate();
        snapshot.flip();
        try {
            int crc = (int) FoyerDirectoryBindings.INDEX_OUTPUT_CHECKSUM.invokeExact(
                    ptr, MemorySegment.ofBuffer(snapshot), bufferedLen);
            return Integer.toUnsignedLong(crc);
        } catch (Throwable t) {
            throw new IOException(t);
        }
    }

    @Override
    public void close() throws IOException {
        if (closed) {
            return;
        }
        closed = true;
        int bufferedLen = buf.position();
        buf.position(0).limit(pageSize);
        try {
            FoyerDirectoryBindings.INDEX_OUTPUT_CLOSE.invokeExact(
                    ptr, MemorySegment.ofBuffer(buf), pageSize, bufferedLen);
        } catch (Throwable t) {
            throw new IOException(t);
        }
    }
}

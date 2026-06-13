package com.github.mccullocht.foyerdir;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.SymbolLookup;
import java.lang.foreign.ValueLayout;
import java.lang.invoke.MethodHandle;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;

final class FoyerDirectoryBindings {
    // foyerdir_version() -> u32
    static final MethodHandle VERSION;
    // foyer_directory_open(path, path_len, cache_bytes, log_page_size) -> *FoyerDirectory
    static final MethodHandle DIRECTORY_OPEN;
    // foyer_directory_close(dir)
    static final MethodHandle DIRECTORY_CLOSE;
    // foyer_directory_sync(dir)
    static final MethodHandle DIRECTORY_SYNC;
    // foyer_directory_delete_file_id(dir, relative_path, len)
    static final MethodHandle DIRECTORY_DELETE_FILE_ID;
    // foyer_directory_create_output(dir, relative_path, path_len) -> *FoyerIndexOutput
    static final MethodHandle DIRECTORY_CREATE_OUTPUT;
    // foyer_index_output_write_page(out, page, page_len)
    static final MethodHandle INDEX_OUTPUT_WRITE_PAGE;
    // foyer_index_output_checksum(out, buffered, buffered_len) -> u32
    static final MethodHandle INDEX_OUTPUT_CHECKSUM;
    // foyer_index_output_close(out, page, page_len, len)
    static final MethodHandle INDEX_OUTPUT_CLOSE;
    // foyer_directory_create_input(dir, relative_path, path_len) -> *FoyerIndexInput
    static final MethodHandle DIRECTORY_CREATE_INPUT;
    // foyer_index_input_len(input) -> u64
    static final MethodHandle INDEX_INPUT_LEN;
    // foyer_index_input_read_chunks(input, offset, length) -> *const FoyerReadChunks
    static final MethodHandle INDEX_INPUT_READ_CHUNKS;
    // foyer_read_chunks_drop(read_chunks)
    static final MethodHandle INDEX_INPUT_READ_CHUNKS_DROP;
    // foyer_index_input_prefetch(input, offset, length)
    static final MethodHandle INDEX_INPUT_PREFETCH;
    // foyer_index_input_close(input)
    static final MethodHandle INDEX_INPUT_CLOSE;

    static {
        try {
            SymbolLookup symbols = loadNativeLibrary();
            Linker linker = Linker.nativeLinker();
            VERSION = linker.downcallHandle(
                    symbols.findOrThrow("foyerdir_version"),
                    FunctionDescriptor.of(ValueLayout.JAVA_INT));
            DIRECTORY_OPEN = linker.downcallHandle(
                    symbols.findOrThrow("foyer_directory_open"),
                    FunctionDescriptor.of(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT,
                            ValueLayout.JAVA_LONG,
                            ValueLayout.JAVA_INT));
            DIRECTORY_CLOSE = linker.downcallHandle(
                    symbols.findOrThrow("foyer_directory_close"),
                    FunctionDescriptor.ofVoid(ValueLayout.ADDRESS));
            DIRECTORY_SYNC = linker.downcallHandle(
                    symbols.findOrThrow("foyer_directory_sync"),
                    FunctionDescriptor.ofVoid(ValueLayout.ADDRESS));
            DIRECTORY_DELETE_FILE_ID = linker.downcallHandle(
                    symbols.findOrThrow("foyer_directory_delete_file_id"),
                    FunctionDescriptor.ofVoid(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT));
            DIRECTORY_CREATE_OUTPUT = linker.downcallHandle(
                    symbols.findOrThrow("foyer_directory_create_output"),
                    FunctionDescriptor.of(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT));
            INDEX_OUTPUT_WRITE_PAGE = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_output_write_page"),
                    FunctionDescriptor.ofVoid(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT));
            INDEX_OUTPUT_CHECKSUM = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_output_checksum"),
                    FunctionDescriptor.of(
                            ValueLayout.JAVA_INT,
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT));
            INDEX_OUTPUT_CLOSE = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_output_close"),
                    FunctionDescriptor.ofVoid(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT,
                            ValueLayout.JAVA_INT));
            DIRECTORY_CREATE_INPUT = linker.downcallHandle(
                    symbols.findOrThrow("foyer_directory_create_input"),
                    FunctionDescriptor.of(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_INT));
            INDEX_INPUT_LEN = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_input_len"),
                    FunctionDescriptor.of(
                            ValueLayout.JAVA_LONG,
                            ValueLayout.ADDRESS));
            INDEX_INPUT_READ_CHUNKS = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_input_read_chunks"),
                    FunctionDescriptor.of(
                            ValueLayout.ADDRESS,
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_LONG,
                            ValueLayout.JAVA_LONG));
            INDEX_INPUT_READ_CHUNKS_DROP = linker.downcallHandle(
                    symbols.findOrThrow("foyer_read_chunks_drop"),
                    FunctionDescriptor.ofVoid(ValueLayout.ADDRESS));
            INDEX_INPUT_PREFETCH = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_input_prefetch"),
                    FunctionDescriptor.ofVoid(
                            ValueLayout.ADDRESS,
                            ValueLayout.JAVA_LONG,
                            ValueLayout.JAVA_LONG));
            INDEX_INPUT_CLOSE = linker.downcallHandle(
                    symbols.findOrThrow("foyer_index_input_close"),
                    FunctionDescriptor.ofVoid(ValueLayout.ADDRESS));
        } catch (Exception e) {
            throw new ExceptionInInitializerError(e);
        }
    }

    private FoyerDirectoryBindings() {
    }

    private static SymbolLookup loadNativeLibrary() throws Exception {
        String os = System.getProperty("os.name").toLowerCase();
        String arch = System.getProperty("os.arch").toLowerCase();
        String classifier;
        if (os.startsWith("mac")) {
            classifier = arch.equals("aarch64") ? "darwin-aarch64" : "darwin-x86_64";
        } else if (os.startsWith("linux")) {
            classifier = arch.equals("aarch64") ? "linux-aarch64" : "linux-x86_64";
        } else {
            throw new UnsatisfiedLinkError("Unsupported platform: " + os + " " + arch);
        }

        String libFile = System.mapLibraryName("foyerdir");
        String resourcePath = "/" + classifier + "/" + libFile;
        try (var in = FoyerDirectoryBindings.class.getResourceAsStream(resourcePath)) {
            if (in == null) {
                throw new UnsatisfiedLinkError("Native library not found in classpath: " + resourcePath);
            }
            Path tmp = Files.createTempFile("foyerdir-", "-" + libFile);
            tmp.toFile().deleteOnExit();
            Files.copy(in, tmp, StandardCopyOption.REPLACE_EXISTING);
            return SymbolLookup.libraryLookup(tmp, Arena.global());
        }
    }
}

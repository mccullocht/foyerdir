package com.github.mccullocht.foyerdir;

import java.lang.foreign.Arena;
import java.lang.foreign.FunctionDescriptor;
import java.lang.foreign.Linker;
import java.lang.foreign.SymbolLookup;
import java.lang.invoke.MethodHandle;
import java.lang.foreign.ValueLayout;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;

public final class FoyerDir {
    private static final SymbolLookup SYMBOLS;

    static {
        try {
            SYMBOLS = loadNativeLibrary();
        } catch (Exception e) {
            throw new ExceptionInInitializerError(e);
        }
    }

    private FoyerDir() {}

    public static int version() throws Throwable {
        MethodHandle handle = Linker.nativeLinker()
                .downcallHandle(
                        SYMBOLS.findOrThrow("foyerdir_version"),
                        FunctionDescriptor.of(ValueLayout.JAVA_INT));
        return (int) handle.invokeExact();
    }

    private static SymbolLookup loadNativeLibrary() throws Exception {
        String os = System.getProperty("os.name").toLowerCase();
        String arch = System.getProperty("os.arch").toLowerCase();
        String classifier;
        if (os.startsWith("mac")) {
            classifier = arch.equals("aarch64") ? "darwin-aarch64" : "darwin-x86_64";
        } else if (os.startsWith("linux")) {
            classifier = arch.equals("aarch64") ? "linux-aarch64" : "linux-x86_64";
        } else if (os.startsWith("win")) {
            classifier = "windows-x86_64";
        } else {
            throw new UnsatisfiedLinkError("Unsupported platform: " + os + " " + arch);
        }

        String libFile = System.mapLibraryName("foyerdir");
        String resourcePath = "/" + classifier + "/" + libFile;
        try (var in = FoyerDir.class.getResourceAsStream(resourcePath)) {
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

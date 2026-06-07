package com.github.mccullocht.foyerdir;

import java.io.IOException;
import java.nio.file.Path;
import org.apache.lucene.tests.store.BaseDirectoryTestCase;

public class FoyerDirectoryTest extends BaseDirectoryTestCase {
    @Override
    protected FoyerDirectory getDirectory(Path path) throws IOException {
        return new FoyerDirectory(path, 32 * 1024 * 1024L, 12);
    }
}

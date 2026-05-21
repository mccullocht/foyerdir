# FoyerDir

A Lucene `Directory` implementation that skips the mmap and the kernel's page cache, preferring an
explicit in-memory LRU cache built with Foyer and direct IO to perform reads.
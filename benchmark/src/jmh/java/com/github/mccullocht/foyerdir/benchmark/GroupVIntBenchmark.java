package com.github.mccullocht.foyerdir.benchmark;

import com.github.mccullocht.foyerdir.FoyerDirectory;
import java.io.IOException;
import java.nio.file.Files;
import java.util.Arrays;
import java.util.Random;
import java.util.concurrent.TimeUnit;
import org.apache.lucene.store.ByteArrayDataInput;
import org.apache.lucene.store.ByteArrayDataOutput;
import org.apache.lucene.store.ByteBuffersDataOutput;
import org.apache.lucene.store.ByteBuffersDirectory;
import org.apache.lucene.store.Directory;
import org.apache.lucene.store.IOContext;
import org.apache.lucene.store.IndexInput;
import org.apache.lucene.store.IndexOutput;
import org.apache.lucene.store.MMapDirectory;
import org.apache.lucene.store.NIOFSDirectory;
import org.apache.lucene.util.GroupVIntUtil;
import org.openjdk.jmh.annotations.Benchmark;
import org.openjdk.jmh.annotations.BenchmarkMode;
import org.openjdk.jmh.annotations.Fork;
import org.openjdk.jmh.annotations.Level;
import org.openjdk.jmh.annotations.Measurement;
import org.openjdk.jmh.annotations.Mode;
import org.openjdk.jmh.annotations.OutputTimeUnit;
import org.openjdk.jmh.annotations.Param;
import org.openjdk.jmh.annotations.Scope;
import org.openjdk.jmh.annotations.Setup;
import org.openjdk.jmh.annotations.State;
import org.openjdk.jmh.annotations.TearDown;
import org.openjdk.jmh.annotations.Warmup;
import org.openjdk.jmh.infra.Blackhole;

@BenchmarkMode(Mode.Throughput)
@OutputTimeUnit(TimeUnit.MICROSECONDS)
@State(Scope.Benchmark)
@Warmup(iterations = 4, time = 1)
@Measurement(iterations = 5, time = 1)
@Fork(value = 1, jvmArgsPrepend = { "--add-modules=jdk.unsupported", "--enable-native-access=ALL-UNNAMED" })
public class GroupVIntBenchmark {

  // Cumulative frequency for each number of bits per value used by doc deltas of tail postings on
  // wikibigall. Copied from org.apache.lucene.benchmark.jmh.GroupVIntBenchmark.
  private static final float[] CUMULATIVE_FREQUENCY_BY_BITS_REQUIRED = new float[] {
      0.0f,
      0.01026574f,
      0.021453038f,
      0.03342156f,
      0.046476692f,
      0.060890317f,
      0.07644147f,
      0.093718216f,
      0.11424741f,
      0.13989712f,
      0.17366524f,
      0.22071244f,
      0.2815692f,
      0.3537585f,
      0.43655503f,
      0.52308f,
      0.6104675f,
      0.7047371f,
      0.78155357f,
      0.8671179f,
      0.9740598f,
      1.0f
  };

  final int maxSize = 256;
  final int[] docs = new int[maxSize];
  final int[] values = new int[maxSize];

  IndexInput mmapGVIntIn;
  IndexInput mmapVIntIn;
  IndexInput nioGVIntIn;
  IndexInput nioVIntIn;
  IndexInput foyerGVIntIn;
  IndexInput foyerVIntIn;

  FoyerDirectory foyerDir;

  @Param({ "64" })
  public int size;

  void initNioInput(int[] docs) throws Exception {
    Directory dir = new NIOFSDirectory(Files.createTempDirectory("groupvintdata"));
    IndexOutput vintOut = dir.createOutput("vint", IOContext.DEFAULT);
    IndexOutput gvintOut = dir.createOutput("gvint", IOContext.DEFAULT);
    gvintOut.writeGroupVInts(docs, docs.length);
    for (long v : docs) {
      vintOut.writeVInt((int) v);
    }
    vintOut.close();
    gvintOut.close();
    nioGVIntIn = dir.openInput("gvint", IOContext.DEFAULT);
    nioVIntIn = dir.openInput("vint", IOContext.DEFAULT);
  }

  void initMMapInput(int[] docs) throws Exception {
    Directory dir = new MMapDirectory(Files.createTempDirectory("groupvintdata"));
    IndexOutput vintOut = dir.createOutput("vint", IOContext.DEFAULT);
    IndexOutput gvintOut = dir.createOutput("gvint", IOContext.DEFAULT);
    gvintOut.writeGroupVInts(docs, docs.length);
    for (long v : docs) {
      vintOut.writeVInt((int) v);
    }
    vintOut.close();
    gvintOut.close();
    mmapGVIntIn = dir.openInput("gvint", IOContext.DEFAULT);
    mmapVIntIn = dir.openInput("vint", IOContext.DEFAULT);
  }

  void initFoyerInput(int[] docs) throws Exception {
    foyerDir = new FoyerDirectory(Files.createTempDirectory("groupvintdata"), 64 << 20, 12);
    IndexOutput vintOut = foyerDir.createOutput("vint", IOContext.DEFAULT);
    IndexOutput gvintOut = foyerDir.createOutput("gvint", IOContext.DEFAULT);
    gvintOut.writeGroupVInts(docs, docs.length);
    for (long v : docs) {
      vintOut.writeVInt((int) v);
    }
    vintOut.close();
    gvintOut.close();
    foyerGVIntIn = foyerDir.openInput("gvint", IOContext.DEFAULT);
    foyerVIntIn = foyerDir.openInput("vint", IOContext.DEFAULT);
  }

  @Setup(Level.Trial)
  public void init() throws Exception {
    Random r = new Random(0);
    for (int i = 0; i < maxSize; ++i) {
      float randomFloat = r.nextFloat();
      int numBits = 1 + Arrays.binarySearch(CUMULATIVE_FREQUENCY_BY_BITS_REQUIRED, randomFloat);
      if (numBits < 0) {
        numBits = -numBits;
      }
      docs[i] = r.nextInt(1 << (numBits - 1), 1 << numBits);
    }
    initMMapInput(docs);
    initNioInput(docs);
    initFoyerInput(docs);
  }

  @TearDown(Level.Trial)
  public void tearDown() throws Exception {
    if (foyerGVIntIn != null)
      foyerGVIntIn.close();
    if (foyerVIntIn != null)
      foyerVIntIn.close();
    if (foyerDir != null)
      foyerDir.close();
  }

  @Benchmark
  public void benchMMapDirectoryInputs_readVInt(Blackhole bh) throws IOException {
    mmapVIntIn.seek(0);
    for (int i = 0; i < size; i++) {
      values[i] = mmapVIntIn.readVInt();
    }
    bh.consume(values);
  }

  @Benchmark
  public void benchMMapDirectoryInputs_readGroupVInt(Blackhole bh) throws IOException {
    mmapGVIntIn.seek(0);
    GroupVIntUtil.readGroupVInts(mmapGVIntIn, values, size);
    bh.consume(values);
  }

  @Benchmark
  public void benchNIOFSDirectoryInputs_readVInt(Blackhole bh) throws IOException {
    nioVIntIn.seek(0);
    for (int i = 0; i < size; i++) {
      values[i] = nioVIntIn.readVInt();
    }
    bh.consume(values);
  }

  @Benchmark
  public void benchNIOFSDirectoryInputs_readGroupVInt(Blackhole bh) throws IOException {
    nioGVIntIn.seek(0);
    GroupVIntUtil.readGroupVInts(nioGVIntIn, values, size);
    bh.consume(values);
  }

  @Benchmark
  public void benchFoyerDirectoryInputs_readVInt(Blackhole bh) throws IOException {
    foyerVIntIn.seek(0);
    for (int i = 0; i < size; i++) {
      values[i] = foyerVIntIn.readVInt();
    }
    bh.consume(values);
  }

  @Benchmark
  public void benchFoyerDirectoryInputs_readGroupVInt(Blackhole bh) throws IOException {
    foyerGVIntIn.seek(0);
    GroupVIntUtil.readGroupVInts(foyerGVIntIn, values, size);
    bh.consume(values);
  }
}

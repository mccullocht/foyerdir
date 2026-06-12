package com.github.mccullocht.foyerdir.benchmark;

import com.github.mccullocht.foyerdir.FoyerDirectory;
import java.io.IOException;
import java.nio.file.Files;
import java.util.Random;
import java.util.concurrent.TimeUnit;
import org.apache.lucene.codecs.lucene104.ForUtil;
import org.apache.lucene.codecs.lucene104.PostingIndexInput;
import org.apache.lucene.store.IOContext;
import org.apache.lucene.store.IndexInput;
import org.apache.lucene.store.IndexOutput;
import org.apache.lucene.store.MMapDirectory;
import org.apache.lucene.store.NIOFSDirectory;
import org.apache.lucene.util.IOUtils;
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
@Warmup(iterations = 5, time = 1)
@Measurement(iterations = 5, time = 1)
@Fork(
    value = 3,
    jvmArgsAppend = {"-Xmx1g", "-Xms1g", "-XX:+AlwaysPreTouch", "--enable-native-access=ALL-UNNAMED"})
public class PostingIndexInputBenchmark {

  private final ForUtil forUtil = new ForUtil();
  private final int[] values = new int[ForUtil.BLOCK_SIZE];

  @Param({"2", "3", "4", "5", "6", "7", "8", "9", "10"})
  public int bpv;

  private IndexInput mmapIn;
  private PostingIndexInput mmapPostingIn;

  private IndexInput niofsIn;
  private PostingIndexInput niofsPostingIn;

  private FoyerDirectory foyerDir;
  private IndexInput foyerIn;
  private PostingIndexInput foyerPostingIn;

  @Setup(Level.Trial)
  public void setup() throws Exception {
    var mmapDir = MMapDirectory.open(Files.createTempDirectory("postingBench"));
    try (IndexOutput out = mmapDir.createOutput("docs", IOContext.DEFAULT)) {
      Random r = new Random(0);
      for (int i = 0; i < 100; ++i) {
        out.writeLong(r.nextLong());
      }
    }
    mmapIn = mmapDir.openInput("docs", IOContext.DEFAULT);
    mmapPostingIn = new PostingIndexInput(mmapIn, forUtil);

    var niofsDir = NIOFSDirectory.open(Files.createTempDirectory("postingBench"));
    try (IndexOutput out = niofsDir.createOutput("docs", IOContext.DEFAULT)) {
      Random r = new Random(0);
      for (int i = 0; i < 100; ++i) {
        out.writeLong(r.nextLong());
      }
    }
    niofsIn = niofsDir.openInput("docs", IOContext.DEFAULT);
    niofsPostingIn = new PostingIndexInput(niofsIn, forUtil);

    foyerDir = new FoyerDirectory(Files.createTempDirectory("postingBench"), 64 << 20, 12);
    try (IndexOutput out = foyerDir.createOutput("docs", IOContext.DEFAULT)) {
      Random r = new Random(0);
      for (int i = 0; i < 100; ++i) {
        out.writeLong(r.nextLong());
      }
    }
    foyerIn = foyerDir.openInput("docs", IOContext.DEFAULT);
    foyerPostingIn = new PostingIndexInput(foyerIn, forUtil);
  }

  @TearDown(Level.Trial)
  public void tearDown() throws Exception {
    IOUtils.close(mmapIn);
    IOUtils.close(niofsIn);
    IOUtils.close(foyerIn, foyerDir);
  }

  @Benchmark
  public void mmapDecode(Blackhole bh) throws IOException {
    mmapIn.seek(3);
    mmapPostingIn.decode(bpv, values);
    bh.consume(values);
  }

  @Benchmark
  @Fork(
      value = 3,
      jvmArgsPrepend = {"--add-modules=jdk.incubator.vector"},
      jvmArgsAppend = {"-Xmx1g", "-Xms1g", "-XX:+AlwaysPreTouch"})
  public void mmapDecodeVector(Blackhole bh) throws IOException {
    mmapIn.seek(3);
    mmapPostingIn.decode(bpv, values);
    bh.consume(values);
  }

  @Benchmark
  public void niofsDecode(Blackhole bh) throws IOException {
    niofsIn.seek(3);
    niofsPostingIn.decode(bpv, values);
    bh.consume(values);
  }

  @Benchmark
  @Fork(
      value = 3,
      jvmArgsPrepend = {"--add-modules=jdk.incubator.vector"},
      jvmArgsAppend = {"-Xmx1g", "-Xms1g", "-XX:+AlwaysPreTouch"})
  public void niofsDecodeVector(Blackhole bh) throws IOException {
    niofsIn.seek(3);
    niofsPostingIn.decode(bpv, values);
    bh.consume(values);
  }

  @Benchmark
  public void foyerDecode(Blackhole bh) throws IOException {
    foyerIn.seek(3);
    foyerPostingIn.decode(bpv, values);
    bh.consume(values);
  }

  @Benchmark
  @Fork(
      value = 3,
      jvmArgsPrepend = {"--add-modules=jdk.incubator.vector"},
      jvmArgsAppend = {"-Xmx1g", "-Xms1g", "-XX:+AlwaysPreTouch", "--enable-native-access=ALL-UNNAMED"})
  public void foyerDecodeVector(Blackhole bh) throws IOException {
    foyerIn.seek(3);
    foyerPostingIn.decode(bpv, values);
    bh.consume(values);
  }
}

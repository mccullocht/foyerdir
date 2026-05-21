import java.util.Locale

val rustCdylib = extensions.create<RustCdylibExtension>("rustCdylib")

val os = System.getProperty("os.name").lowercase(Locale.US)
val arch = System.getProperty("os.arch").lowercase(Locale.US)

val nativeClassifier = when {
    os.startsWith("mac") && arch == "aarch64" -> "darwin-aarch64"
    os.startsWith("mac") -> "darwin-x86_64"
    os.startsWith("linux") && arch == "aarch64" -> "linux-aarch64"
    os.startsWith("linux") -> "linux-x86_64"
    os.startsWith("win") -> "windows-x86_64"
    else -> error("Unsupported platform: $os $arch")
}

val libExtension = when {
    os.startsWith("mac") -> "dylib"
    os.startsWith("win") -> "dll"
    else -> "so"
}

val cargoTargetDir = layout.buildDirectory.dir("cargo-target")
val nativeLibDir = layout.buildDirectory.dir("native-libs")

val cargoBuild = tasks.register<Exec>("cargoBuild") {
    group = "build"
    description = "Compiles the Rust cdylib crate with cargo"
    inputs.dir(rustCdylib.crateDir)
    executable("cargo")
    doFirst {
        workingDir(rustCdylib.crateDir.get().asFile)
        args("build", "--release", "--target-dir", cargoTargetDir.get().asFile.absolutePath)
    }
}

val copyNativeLib = tasks.register<Copy>("copyNativeLib") {
    group = "build"
    description = "Copies the compiled native library into the resource output directory"
    dependsOn(cargoBuild)
    from(cargoTargetDir.map { it.dir("release") }) {
        include("*.$libExtension")
    }
    into(nativeLibDir.map { it.dir(nativeClassifier) })
}

plugins.withId("java") {
    val sourceSets = extensions.getByType<SourceSetContainer>()
    sourceSets.named("main") {
        resources.srcDir(nativeLibDir)
    }
    tasks.named("processResources") {
        dependsOn(copyNativeLib)
    }
}

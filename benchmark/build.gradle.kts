plugins {
    java
    id("me.champeau.jmh") version "0.7.2"
}

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(25)
    }
}

repositories {
    mavenCentral()
}

dependencies {
    implementation(project(":"))
    implementation("org.apache.lucene:lucene-core:10.4.0")
}

jmh {
    jvmArgs.add("--enable-native-access=ALL-UNNAMED")
    if (project.hasProperty("benchmarks")) {
        includes.add(project.property("benchmarks") as String)
    }
    if (project.hasProperty("profiler")) {
        profilers.add(project.property("profiler") as String)
    }
}

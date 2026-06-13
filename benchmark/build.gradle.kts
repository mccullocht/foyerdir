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
    if (project.hasProperty("params")) {
        val entries = (project.property("params") as String).split(",")
        for (entry in entries) {
            val (k, v) = entry.split("=", limit = 2)
            benchmarkParameters.put(k.trim(), project.objects.listProperty(String::class.java).value(listOf(v.trim())))
        }
    }
}

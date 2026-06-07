plugins {
    `java-library`
    id("rust-cdylib")
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
    implementation("org.apache.lucene:lucene-core:10.4.0")
    testImplementation("org.apache.lucene:lucene-test-framework:10.4.0")
}

rustCdylib {
    crateDir.set(layout.projectDirectory.dir("native"))
    crateName.set("foyerdir")
}

tasks.withType<Test>().configureEach {
    jvmArgs("--enable-native-access=ALL-UNNAMED")
}

import org.gradle.api.file.DirectoryProperty
import org.gradle.api.provider.Property

interface RustCdylibExtension {
    val crateDir: DirectoryProperty
    val crateName: Property<String>
}

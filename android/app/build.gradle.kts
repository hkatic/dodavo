plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.aboutlibraries)
}

/** Dodavo's version, which only the root Cargo.toml has. */
val cargoVersion: String =
    rootProject.file("../Cargo.toml").readLines()
        .first { it.startsWith("version = ") }
        .substringAfter('"').substringBefore('"')
val versionParts = cargoVersion.split('.').map(String::toInt)

android {
    namespace = "com.hrvojekatic.dodavo"
    compileSdk = 37
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "com.hrvojekatic.dodavo"
        minSdk = 26
        targetSdk = 37
        versionName = cargoVersion
        versionCode = versionParts[0] * 10_000 + versionParts[1] * 100 + versionParts[2]
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    buildFeatures {
        compose = true
    }

    androidResources {
        // locales_config.xml from the res folders of the languages.
        generateLocaleConfig = true
    }

    sourceSets {
        getByName("main") {
            jniLibs.directories.add(layout.buildDirectory.dir("rustJniLibs").get().asFile.path)
        }
    }

    lint {
        // Every language has every text (see crates/dodavo-android).
        error += "MissingTranslation"
    }
}

dependencies {
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material3.navigation.suite)
    implementation(libs.androidx.compose.material.icons)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.appcompat)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.viewmodel.navigation3)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.androidx.navigation3.runtime)
    implementation(libs.androidx.navigation3.ui)
    implementation(libs.androidx.material3.adaptive.navigation3)
    implementation(libs.androidx.webkit)
    implementation(libs.androidx.datastore.preferences)
    debugImplementation(libs.androidx.compose.ui.tooling)
    debugImplementation(libs.androidx.compose.ui.test.manifest)

    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    androidTestImplementation(platform(libs.androidx.compose.bom))
    androidTestImplementation(libs.androidx.compose.ui.test.junit4)
    androidTestImplementation(libs.androidx.test.core)
    androidTestImplementation(libs.androidx.test.runner)
    androidTestImplementation(libs.androidx.test.ext.junit)
}

// The native library: Dodavo's Rust core for the app's ABIs, through
// cargo-ndk, with the NDK that the app is built with.
val buildRust = tasks.register<Exec>("buildRust") {
    description = "Builds libdodavo.so (crates/dodavo-android) for arm64-v8a and x86_64."
    val repository = rootProject.layout.projectDirectory.dir("..")
    val output = layout.buildDirectory.dir("rustJniLibs")
    val cargo = providers.environmentVariable("CARGO")
        .orElse(System.getProperty("user.home") + "/.cargo/bin/cargo")
    workingDir = repository.asFile
    inputs.files(repository.files("Cargo.toml", "Cargo.lock"))
    inputs.dir(repository.dir("src"))
    inputs.dir(repository.dir("crates"))
    inputs.dir(repository.dir("res"))
    outputs.dir(output)
    environment(
        "ANDROID_NDK_HOME",
        androidComponents.sdkComponents.ndkDirectory.get().asFile.path,
    )
    commandLine(
        cargo.get(), "ndk",
        "-t", "arm64-v8a", "-t", "x86_64",
        "--platform", "26",
        "-o", output.get().asFile.path,
        "build", "--release", "--locked", "-p", "dodavo-android",
    )
}

tasks.named("preBuild") {
    dependsOn(buildRust)
}

/** The help pages (res/help/android), which the app shows from assets/help. */
abstract class HelpAssets : DefaultTask() {
    @get:InputDirectory
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val pages: DirectoryProperty

    @get:OutputDirectory
    abstract val output: DirectoryProperty

    @get:Inject
    abstract val files: FileSystemOperations

    @TaskAction
    fun copy() {
        files.sync {
            from(pages)
            into(output.dir("help"))
        }
    }
}

val helpAssets = tasks.register<HelpAssets>("helpAssets") {
    description = "Copies the Android help pages into the app's assets."
    pages.set(rootProject.layout.projectDirectory.dir("../res/help/android"))
}

androidComponents {
    onVariants { variant ->
        variant.sources.assets?.addGeneratedSourceDirectory(helpAssets, HelpAssets::output)
    }
}

// The licenses of the Kotlin and AndroidX libraries in the app, which
// android/licenses.sh adds to res/third-party-licenses-android.txt. Only
// licenses that agree with the GPL 3.0 may come in.
aboutLibraries {
    collect {
        includePlatform = false
        fetchRemoteLicense = false
        fetchRemoteFunding = false
    }
    export {
        outputFile = layout.buildDirectory.file("dodavo/libraries.json").get().asFile
        variant = "release"
        prettyPrint = true
        excludeFields.addAll("funding")
    }
    license {
        strictMode = com.mikepenz.aboutlibraries.plugin.StrictMode.FAIL
        allowedLicenses.addAll("Apache-2.0", "MIT", "BSD-3-Clause", "BSD-2-Clause")
    }
}

/**
 * The licenses of the app's libraries as text, laid out as cargo-about lays
 * out those of the crates (installer/about.hbs): each license, the libraries
 * that use it, and its text. A license whose text names its copyright holder
 * (BSD) comes once for each library, with the holder from [copyrights].
 */
abstract class LibraryLicenses : DefaultTask() {
    @get:InputFile
    @get:PathSensitive(PathSensitivity.NONE)
    abstract val definitions: RegularFileProperty

    /** The year and holder of each library under such a license ("2008 Google Inc"). */
    @get:Input
    abstract val copyrights: MapProperty<String, String>

    @get:OutputFile
    abstract val output: RegularFileProperty

    @TaskAction
    fun write() {
        @Suppress("UNCHECKED_CAST")
        val json = groovy.json.JsonSlurper().parse(definitions.get().asFile) as Map<String, Any?>
        @Suppress("UNCHECKED_CAST")
        val libraries = (json["libraries"] as List<Map<String, Any?>>).sortedBy { it["uniqueId"] as String }
        @Suppress("UNCHECKED_CAST")
        val licenses = json["licenses"] as Map<String, Map<String, Any?>>
        val holders = copyrights.get()

        fun source(library: Map<String, Any?>): String {
            @Suppress("UNCHECKED_CAST")
            val scm = (library["scm"] as Map<String, Any?>?)?.get("url") as String?
            return scm ?: library["website"] as String? ?: "https://mvnrepository.com/artifact/" +
                (library["uniqueId"] as String).replace(':', '/')
        }
        fun line(library: Map<String, Any?>) =
            "- ${library["uniqueId"]} ${library["artifactVersion"]}: ${source(library)}"

        /** An SPDX template's text with its variables filled in. */
        val variable = Regex("""<<var;name=([^;]*);original=([^;]*);match=[^>]*>>""")
        fun text(template: String, library: String?): String = template
            .replace(variable) { match ->
                val (name, original) = match.destructured
                if (name == "copyright") {
                    holders[library ?: ""]
                        ?: throw GradleException("$library: its license names the holder; add it to copyrights")
                } else {
                    original
                }
            }
            .replace("<<beginOptional>>", "").replace("<<endOptional>>", "")
            .lines().joinToString("\n") { it.trimEnd() }.trim()

        val byLicense = libraries
            .flatMap { library -> (library["licenses"] as List<*>).map { it as String to library } }
            .groupBy({ it.first }, { it.second })
            .toSortedMap(compareByDescending<String> { id -> libraries.count { id in (it["licenses"] as List<*>) } }.thenBy { it })
        val rule = "=".repeat(80)
        val out = StringBuilder()
        out.append("LIBRARIES OF THE APP\n\n")
        out.append("The Android app also contains the following open-source libraries (Kotlin\n")
        out.append("and AndroidX). Each license below is followed by the libraries that use it,\n")
        out.append("with their version and the place where their source code is available.\n\n")
        out.append("Overview:\n")
        byLicense.forEach { (id, users) -> out.append("- ${licenses[id]?.get("name") ?: id}: ${users.size}\n") }
        byLicense.forEach { (id, users) ->
            val license = licenses[id] ?: throw GradleException("no text for the license $id")
            val content = license["content"] as String
            val perLibrary = variable.findAll(content).any { it.groupValues[1] == "copyright" }
            val groups = if (perLibrary) users.map { listOf(it) } else listOf(users)
            groups.forEach { group ->
                out.append("\n$rule\n${license["name"]}\n\nUsed by:\n")
                group.forEach { out.append(line(it)).append('\n') }
                out.append("-".repeat(80)).append("\n\n")
                out.append(text(content, if (perLibrary) group.single()["uniqueId"] as String else null))
                out.append('\n')
            }
        }
        output.get().asFile.writeText(out.toString())
    }
}

tasks.register<LibraryLicenses>("libraryLicenses") {
    description = "Writes the licenses of the app's libraries for android/licenses.sh."
    dependsOn("exportLibraryDefinitions")
    definitions.set(layout.buildDirectory.file("dodavo/libraries.json"))
    copyrights.put(
        "androidx.datastore:datastore-preferences-external-protobuf",
        "2008 Google Inc",
    )
    output.set(layout.buildDirectory.file("dodavo/libraries.txt"))
}

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
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

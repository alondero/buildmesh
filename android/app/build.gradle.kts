plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The shipped APK takes its version from the application manifests instead of a
// literal here, so an APK on a release can never disagree with the installer
// beside it. `npm run version:set` stays the single writer of that version.
val appVersion: String = File(rootProject.projectDir.parentFile, "package.json")
    .readText()
    .let { Regex("\"version\"\\s*:\\s*\"([^\"]+)\"").find(it)?.groupValues?.get(1) }
    ?: error("Could not read the app version from package.json")

// Android requires a strictly increasing integer. major*10000 + minor*100 +
// patch leaves room for 100 patch releases per minor and still orders 1.4.0
// after 1.3.9. Any `-0` suffix is dropped first, so a between-release build and
// the release it precedes share one code rather than bumping on every commit.
val appVersionParts = Regex("\\d+").findAll(appVersion.substringBefore('-')).map { it.value.toInt() }.toList()
val appVersionCode = appVersionParts.getOrElse(0) { 0 } * 10_000 +
    appVersionParts.getOrElse(1) { 0 } * 100 +
    appVersionParts.getOrElse(2) { 0 }

// Release signing arrives from the environment and never from the repository.
// The workflow base64-decodes ANDROID_KEYSTORE_BASE64 into a temporary file and
// points ANDROID_KEYSTORE_PATH at it; when that is absent the release type
// still builds, it is simply unsigned.
val signingStoreFile: String? = providers.environmentVariable("ANDROID_KEYSTORE_PATH").orNull

android {
    namespace = "dev.buildmesh.remote"
    compileSdk = 36
    defaultConfig {
        applicationId = "dev.buildmesh.remote"
        minSdk = 26
        targetSdk = 36
        versionCode = appVersionCode
        versionName = appVersion
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    signingConfigs {
        if (signingStoreFile != null) {
            create("release") {
                storeFile = file(signingStoreFile)
                storePassword = providers.environmentVariable("ANDROID_KEY_PASSWORD").orNull
                keyAlias = providers.environmentVariable("ANDROID_KEY_ALIAS").orNull
                keyPassword = providers.environmentVariable("ANDROID_KEY_PASSWORD").orNull
            }
        }
    }
    buildTypes {
        getByName("release") {
            // Mirrors the debug type deliberately: no R8 minification or resource
            // shrinking until keep rules are written and verified against the
            // live device suite, which would otherwise silently strip the
            // WebSocket and Compose paths this app depends on.
            isMinifyEnabled = false
            if (signingStoreFile != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
        create("live") {
            initWith(getByName("debug"))
            applicationIdSuffix = ".live"
            matchingFallbacks += "debug"
        }
    }
    testBuildType = providers.gradleProperty("buildmesh.testBuildType").getOrElse("debug")
    buildFeatures { compose = true; buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    sourceSets["main"].assets.srcDir(layout.buildDirectory.dir("generated/terminal"))
    sourceSets["main"].java.srcDir(layout.buildDirectory.dir("generated/circuitWire"))
    // The canonical mobile-route fixtures (issue #2318) are committed under
    // the unit test resources, because the JVM tests read them off the
    // classpath. The instrumentation dispatcher reads the very same files
    // from the test APK's assets, so the two suites can never drift apart.
    sourceSets["androidTest"].assets.srcDir(layout.projectDirectory.dir("src/test/resources"))
    packaging { resources.excludes += "/META-INF/{AL2.0,LGPL2.1}" }
    lint {
        abortOnError = true
        warningsAsErrors = true
        // Dependency versions are pinned and upgraded together after build/runtime verification.
        disable += "GradleDependency"
        disable += "AndroidGradlePluginVersion"
    }
}

kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }

val terminalAssets by tasks.registering(Exec::class) {
    workingDir(rootProject.projectDir.parentFile)
    commandLine(if (System.getProperty("os.name").startsWith("Windows")) "npm.cmd" else "npm", "run", "build:android-terminal")
    inputs.files(fileTree(rootProject.file("terminal")), rootProject.file("../package-lock.json"))
    outputs.dir(layout.buildDirectory.dir("generated/terminal"))
}
tasks.named("preBuild") { dependsOn(terminalAssets) }

val circuitWire by tasks.registering(Exec::class) {
    workingDir(rootProject.projectDir.parentFile)
    commandLine("node", "scripts/generate-android-circuit-wire.mjs")
    inputs.files(fileTree(rootProject.file("../src/types/generated")), rootProject.file("../scripts/generate-android-circuit-wire.mjs"))
    outputs.dir(layout.buildDirectory.dir("generated/circuitWire"))
}
tasks.named("preBuild") { dependsOn(circuitWire) }

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2025.05.01")
    implementation(composeBom)
    implementation("androidx.activity:activity-compose:1.10.1")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.9.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.9.1")
    implementation("androidx.webkit:webkit:1.13.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.10.2")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.8.1")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")
    testImplementation("junit:junit:4.13.2")
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
    testImplementation("com.squareup.okhttp3:okhttp-tls:4.12.0")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.10.2")
    androidTestImplementation(composeBom)
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.7.0")
    androidTestImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
    androidTestImplementation("com.squareup.okhttp3:okhttp-tls:4.12.0")
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}

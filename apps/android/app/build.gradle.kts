plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val releaseStorePath = providers.environmentVariable("ZERODROID_RELEASE_STORE_FILE").orNull
val releaseStorePassword = providers.environmentVariable("ZERODROID_RELEASE_STORE_PASSWORD").orNull
val releaseKeyAlias = providers.environmentVariable("ZERODROID_RELEASE_KEY_ALIAS").orNull
val releaseKeyPassword = providers.environmentVariable("ZERODROID_RELEASE_KEY_PASSWORD").orNull
val androidAppVersion = providers.gradleProperty("ZEROCLAW_ANDROID_VERSION").get()
val androidAppVersionCode = providers.gradleProperty("ZEROCLAW_ANDROID_VERSION_CODE").get().toInt()
val provenanceValuePattern = Regex("[A-Za-z0-9._/-]{1,128}")
val validatedProvenanceProperty = { name: String ->
    val value = providers.gradleProperty(name).getOrElse("unknown")
    require(provenanceValuePattern.matches(value)) {
        "$name must be 1-128 characters from [A-Za-z0-9._/-]"
    }
    value
}
val zeroclawRef = validatedProvenanceProperty("ZEROCLAW_REF")
val zeroclawCommit = validatedProvenanceProperty("ZEROCLAW_COMMIT")
val releaseSigningConfigured = listOf(
    releaseStorePath,
    releaseStorePassword,
    releaseKeyAlias,
    releaseKeyPassword,
).all { !it.isNullOrBlank() } && file(releaseStorePath.orEmpty()).isFile

android {
    namespace = "org.zerodroid.bridge"
    compileSdk = 36

    defaultConfig {
        applicationId = "org.zerodroid.bridge"
        minSdk = 30   // AccessibilityService screenshots require Android 11 (API 30)
        targetSdk = 36
        versionCode = androidAppVersionCode
        versionName = androidAppVersion
        // The bundled ZeroClaw executable is arm64-only. Filtering transitive native
        // dependencies prevents unsupported devices from installing a broken gateway.
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
        // Maps key is optional for the build; FusedLocation/sensors/BLE don't need it.
        manifestPlaceholders["MAPS_API_KEY"] =
            (project.findProperty("MAPS_API_KEY") as String?) ?: "unset"
        // Provenance of the bundled rust binary — surfaced in-app so the running thing is traceable.
        buildConfigField("String", "ZEROCLAW_REF", "\"$zeroclawRef\"")
        buildConfigField("String", "ZEROCLAW_COMMIT", "\"$zeroclawCommit\"")
        multiDexEnabled = true   // MINA SSHD pushes past the 64K method limit
    }

    buildFeatures {
        buildConfig = true
    }

    // full = modern devices (API 31+) with on-device Gemini Nano (AI Edge SDK).
    // lite = Android 11+ without AI Edge; full raises the floor to API 31 for AICore.
    flavorDimensions += "tier"
    productFlavors {
        create("full") {
            dimension = "tier"
            minSdk = 31
        }
        create("lite") {
            dimension = "tier"
            minSdk = 30
            versionNameSuffix = "-lite"
        }
    }

    // The bundled zeroclaw + busybox ride in jniLibs as lib*.so. They must be EXTRACTED to the
    // native lib dir (uncompressed, on disk) so they can be exec'd — W^X blocks exec from filesDir.
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
        resources {
            // MINA SSHD's jars (sshd-core + sshd-common) ship overlapping META-INF files.
            excludes += setOf(
                "META-INF/DEPENDENCIES", "META-INF/NOTICE", "META-INF/NOTICE.txt",
                "META-INF/LICENSE", "META-INF/LICENSE.txt", "META-INF/*.kotlin_module",
                "META-INF/INDEX.LIST", "META-INF/versions/**"
            )
        }
    }

    signingConfigs {
        if (releaseSigningConfigured) {
            create("release") {
                storeFile = file(releaseStorePath!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
                enableV1Signing = false
                enableV2Signing = true
                enableV3Signing = true
                enableV4Signing = true
            }
        }
    }

    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = false   // the bundled binary + reflection-y GMS/ML deps; keep simple
            if (releaseSigningConfigured) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.18.0")
    implementation("androidx.appcompat:appcompat:1.8.0")
    // Keystore-backed encrypted storage for the user's provider API keys.
    implementation("androidx.security:security-crypto:1.1.0")
    // Local HTTP bridge the agent's shell talks to (curl localhost:8470/...)
    implementation("org.nanohttpd:nanohttpd:2.3.1")
    implementation("org.json:json:20240303")
    // GMS — FusedLocation now; Maps/Drive/Auth slot in here for full capabilities.
    // 21.4.0 is compiled with Kotlin metadata 2.3; keep the latest line compatible with KGP 2.0.
    implementation("com.google.android.gms:play-services-location:21.3.0")
    implementation("com.google.android.gms:play-services-auth:21.6.0")  // Google Sign-In
    // ML Kit — on-device AI, free, no cloud setup (works on the S21).
    implementation("com.google.mlkit:text-recognition:16.0.1")        // OCR
    implementation("com.google.mlkit:language-id:17.0.6")             // language detection
    implementation("com.google.mlkit:translate:17.0.3")              // offline translation
    // Keep the validated entity-extraction line; both APK flavors now start at API 30.
    implementation("com.google.mlkit:entity-extraction:16.0.0-beta6") // addresses/dates/phones from text
    implementation("com.google.mlkit:barcode-scanning:17.3.0")        // QR / barcodes
    // In-process SSH server (Apache MINA SSHD) — replaces the dropbear native binary. No setuid/
    // setgid/seccomp issues; runs as the app inside the FGS. NIO2 transport needs API 26+.
    implementation("org.apache.sshd:sshd-core:2.19.0") {
        exclude(group = "org.bouncycastle")   // use Android's JCE; avoids the stripped-BC conflict
        exclude(group = "org.slf4j")
    }
    implementation("org.slf4j:slf4j-nop:1.7.36")        // silence MINA's slf4j logging

    // ML Kit's latest Translate/Entity Extraction artifacts still declare obsolete networking
    // libraries. OkHttp 4.x is binary-compatible with 3.x callers; these constraints preserve the
    // APIs while closing the wrong-certificate and malformed-gzip advisories in the resolved APK.
    constraints {
        implementation("com.squareup.okhttp3:okhttp:4.12.0") {
            because("OkHttp < 4.9.2 is affected by GHSA-3cqm-mf7h-prrj")
        }
        implementation("com.squareup.okio:okio:3.6.0") {
            because("Okio < 3.4.0 is affected by GHSA-w33c-445m-f8w7")
        }
        "fullImplementation"("com.google.guava:guava:33.4.8-android") {
            because("AICore exp02 declares vulnerable Guava 31.1-android")
        }
    }

    // Gemini Nano on-device via Google AI Edge SDK (needs AICore) — FULL flavor only (forces API 31).
    "fullImplementation"("com.google.ai.edge.aicore:aicore:0.0.1-exp02")
    "fullImplementation"("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")

    testImplementation("junit:junit:4.13.2")
}

val verifyReleaseSigning by tasks.registering {
    group = "verification"
    description = "Fails closed when a release artifact is requested without the permanent key."
    doLast {
        check(releaseSigningConfigured) {
            "Release signing is not configured. Set ZERODROID_RELEASE_STORE_FILE, " +
                "ZERODROID_RELEASE_STORE_PASSWORD, ZERODROID_RELEASE_KEY_ALIAS, and " +
                "ZERODROID_RELEASE_KEY_PASSWORD."
        }
    }
}

val verifyBundledPayload by tasks.registering {
    group = "verification"
    description = "Fails closed when an APK is packaged without its required generated payload."
    doLast {
        val required = listOf(
            file("src/main/jniLibs/arm64-v8a/libzeroclaw.so"),
            file("src/main/assets/web-dist/index.html"),
            file("src/main/assets/skills/android/SKILL.md"),
        )
        val missing = required.filterNot { it.isFile }
        check(missing.isEmpty()) {
            "Required Android payload is not staged: ${missing.joinToString { it.path }}. " +
                "Run apps/android/scripts/stage-jnilibs.sh from the repository root first."
        }
    }
}

tasks.matching {
    it.name.matches(Regex("^(assemble|bundle|package)(Full|Lite)(Debug|Release)$"))
}.configureEach {
    dependsOn(verifyBundledPayload)
}

tasks.matching {
    it.name in setOf(
        "assembleFullRelease",
        "assembleLiteRelease",
        "bundleFullRelease",
        "bundleLiteRelease",
        "packageFullRelease",
        "packageLiteRelease",
    )
}.configureEach {
    dependsOn(verifyReleaseSigning)
}

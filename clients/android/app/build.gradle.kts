plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "ai.zeroclaw.android"
    compileSdk = 34

    defaultConfig {
        applicationId = "ai.zeroclaw.android"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"

        vectorDrawables {
            useSupportLibrary = true
        }

        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            // Aggressive optimization
            ndk {
                debugSymbolLevel = "NONE"
            }
        }
        debug {
            isDebuggable = true
            applicationIdSuffix = ".debug"
        }
    }

    // Split APKs by ABI - users only download what they need
    splits {
        abi {
            isEnable = true
            reset()
            include("arm64-v8a", "armeabi-v7a", "x86_64")
            isUniversalApk = true  // Also build universal for fallback
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    composeOptions {
        kotlinCompilerExtensionVersion = "1.5.8"
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }

    // Task to build native library before APK
    tasks.register("buildRustLibrary") {
        doLast {
            exec {
                workingDir = rootProject.projectDir.parentFile.parentFile // zeroclaw root
                commandLine("cargo", "ndk",
                    "-t", "arm64-v8a",
                    "-t", "armeabi-v7a",
                    "-t", "x86_64",
                    "-o", "clients/android/app/src/main/jniLibs",
                    "build", "--release", "-p", "zeroclaw-android-bridge")
            }
        }
    }
}

dependencies {
    // Core Android
    implementation("androidx.core:core-ktx:1.12.0")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.7.0")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.7.0")
    implementation("androidx.activity:activity-compose:1.8.2")

    // Compose - minimal set
    implementation(platform("androidx.compose:compose-bom:2024.02.00"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-graphics")
    implementation("androidx.compose.material3:material3")
    // NOTE: Using material-icons-core (small) instead of extended (5MB+)
    // Add individual icons via drawable if needed

    // Navigation
    implementation("androidx.navigation:navigation-compose:2.7.7")

    // DataStore (preferences)
    implementation("androidx.datastore:datastore-preferences:1.0.0")

    // WorkManager (background tasks)
    implementation("androidx.work:work-runtime-ktx:2.9.0")

    // Security (Keystore)
    implementation("androidx.security:security-crypto:1.1.0-alpha06")

    // Coroutines
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.7.3")

    // NOTE: Serialization removed - not used yet, saves ~300KB
    // Add back when needed: implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.6.2")

    // Testing
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.5")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.1")
    androidTestImplementation(platform("androidx.compose:compose-bom:2024.02.00"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")

    // Debug
    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}

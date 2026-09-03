plugins {
    id("com.android.application") version "8.10.1" apply false
    id("org.jetbrains.kotlin.android") version "2.0.21" apply false
}

// Running `./gradlew` with no task produces the single supported debug APK.
defaultTasks(":app:assembleDebug")

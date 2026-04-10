plugins {
    id("com.android.application")
    kotlin("android")
}

android {
    namespace = "com.example.hello"
    compileSdk = 34
    defaultConfig {
        applicationId = "com.example.hello"
        minSdk = 24
        targetSdk = 34
        versionCode = 1
        versionName = "1.0"
    }
}

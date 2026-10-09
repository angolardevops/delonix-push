plugins {
    id("com.android.library")
    `maven-publish`
}

android {
    namespace = "ao.ngolacloud.push.android"
    compileSdk = 36
    defaultConfig { minSdk = 26 }
    publishing { singleVariant("release") { withSourcesJar() } }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin { compilerOptions { jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17 } }

dependencies {
    api(project(":core"))
}

group = "ao.ngolacloud.push"
version = "0.1.0-SNAPSHOT"

afterEvaluate {
    publishing {
        publications { create<MavenPublication>("release") { artifactId = "android"; from(components["release"]) } }
    }
}

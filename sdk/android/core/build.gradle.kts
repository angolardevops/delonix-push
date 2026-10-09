plugins {
    id("org.jetbrains.kotlin.jvm")
    `maven-publish`
    id("org.jetbrains.kotlin.plugin.serialization")
}

kotlin { compilerOptions { jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17 } }
java { sourceCompatibility = JavaVersion.VERSION_17; targetCompatibility = JavaVersion.VERSION_17 }

dependencies {
    api("com.squareup.okhttp3:okhttp:4.12.0")
    api("org.jetbrains.kotlinx:kotlinx-serialization-json:1.9.0")
    testImplementation(kotlin("test"))
    testImplementation("com.squareup.okhttp3:mockwebserver:4.12.0")
}

tasks.test { useJUnit() }

group = "ao.ngolacloud.push"
version = "0.1.0-SNAPSHOT"

java { withSourcesJar() }

publishing {
    publications { create<MavenPublication>("core") { artifactId = "core"; from(components["java"]) } }
}

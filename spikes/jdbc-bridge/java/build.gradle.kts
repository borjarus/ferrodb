// Phase 0 JDBC bridge spike — Java side (`ferrodb-jdbc-host` prototype).
// Build: `gradle fatJar` (Java 21+). Output: build/libs/jdbc-host-spike-all.jar
plugins {
    java
}

java {
    toolchain { languageVersion.set(JavaLanguageVersion.of(21)) }
}

repositories { mavenCentral() }

val arrowVersion = "18.3.0"

dependencies {
    implementation("org.apache.arrow:arrow-vector:$arrowVersion")
    implementation("org.apache.arrow:arrow-memory-netty:$arrowVersion")
    implementation("org.apache.arrow:arrow-jdbc:$arrowVersion")
    // In the product JDBC drivers are NOT build dependencies (downloaded on demand);
    // the spike embeds H2 to be self-contained.
    implementation("com.h2database:h2:2.3.232")
}

tasks.register<Jar>("fatJar") {
    archiveBaseName.set("jdbc-host-spike")
    archiveClassifier.set("all")
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    manifest { attributes["Main-Class"] = "dev.ferrodb.spike.JdbcHost" }
    from(sourceSets.main.get().output)
    dependsOn(configurations.runtimeClasspath)
    from(configurations.runtimeClasspath.get().map { if (it.isDirectory) it else zipTree(it) })
    exclude("META-INF/*.SF", "META-INF/*.DSA", "META-INF/*.RSA")
}

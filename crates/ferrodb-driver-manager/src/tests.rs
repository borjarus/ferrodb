use super::*;

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const ABCD: &str = "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589";

fn test_catalog() -> Catalog {
    Catalog::parse(&format!(
        r#"
[[artifact]]
id = "toy"
kind = "jar"
name = "Toy driver"
coordinates = "dev.toy:toy"
license = "MIT"
driver_class = "dev.toy.Driver"

[[artifact.versions]]
version = "2.0"
min_java = 17
[[artifact.versions.files]]
url = "https://repo1.maven.org/maven2/dev/toy/toy/2.0/toy-2.0.jar"
sha256 = "{ABCD}"

[[artifact.versions]]
version = "1.0"
min_java = 8
[[artifact.versions.files]]
url = "https://repo1.maven.org/maven2/dev/toy/toy/1.0/toy-1.0.jar"
sha256 = "{ABC}"
"#
    ))
    .unwrap()
}

#[test]
fn builtin_catalog_has_every_jdbc_database() {
    let c = Catalog::builtin();
    for id in [
        "h2", "derby", "hsqldb", "oracle", "db2", "sybase", "vertica", "exasol", "hive",
    ] {
        let a = c.get(id).unwrap_or_else(|| panic!("{id} missing"));
        assert_eq!(a.kind, ArtifactKind::Jar);
        assert!(!a.license.is_empty(), "{id}: license");
        assert!(!a.versions.is_empty(), "{id}: versions");
        for v in &a.versions {
            for f in &v.files {
                assert!(
                    f.url.starts_with("https://repo1.maven.org/maven2/"),
                    "{id}: {}",
                    f.url
                );
                assert!(f.size.unwrap_or(0) > 0);
            }
        }
    }
    // Derby: engine, shared code, tools and the network client.
    assert_eq!(c.get("derby").unwrap().versions[0].files.len(), 4);
    // Derby 10.17 needs Java 21: on Java 17 the previous release is offered.
    assert_eq!(
        c.get("derby").unwrap().latest(Some(17)).unwrap().version,
        "10.16.1.1"
    );
}

#[test]
fn offline_install_custom_files_and_update_detection() {
    let d = tempfile::tempdir().unwrap();
    let m = DriverManager::new(d.path().join("drivers"), test_catalog());
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    m.on_status(move |id, s| ev.lock().unwrap().push((id.to_owned(), s.clone())));
    assert_eq!(m.status("toy"), DriverStatus::NotDownloaded);
    assert_eq!(m.versions("toy"), ["2.0", "1.0"]);

    let old = d.path().join("toy-1.0.jar");
    fs::write(&old, b"abc").unwrap();
    // The wrong version for this file: refused.
    assert!(matches!(
        m.install_offline("toy", Some("2.0"), std::slice::from_ref(&old)),
        Err(ManagerError::Checksum { .. })
    ));
    // The version is found from the checksum.
    assert_eq!(
        m.install_offline("toy", None, std::slice::from_ref(&old))
            .unwrap(),
        "1.0"
    );
    match m.status("toy") {
        DriverStatus::UpdateAvailable { installed, latest } => {
            assert_eq!((installed.as_str(), latest.as_str()), ("1.0", "2.0"));
        }
        other => panic!("{other:?}"),
    }
    let files = m.driver_files("toy").unwrap();
    assert_eq!(files.driver_class.as_deref(), Some("dev.toy.Driver"));
    assert!(files.files[0].ends_with("jdbc/toy/1.0/toy-1.0.jar"));

    // Tampering after installation is caught before loading.
    let m2 = DriverManager::new(d.path().join("drivers"), test_catalog());
    fs::write(&files.files[0], b"evil").unwrap();
    assert!(matches!(
        m2.driver_files("toy"),
        Err(ManagerError::Checksum { .. })
    ));

    // Custom files (level C) with the user's class.
    let mine = d.path().join("mine.jar");
    fs::write(&mine, b"anything").unwrap();
    m2.use_custom("toy", &[mine], Some("my.Driver".into()))
        .unwrap();
    assert_eq!(m2.status("toy"), DriverStatus::Custom { files: 1 });
    let f = m2.driver_files("toy").unwrap();
    assert!(f.custom);
    assert_eq!(f.driver_class.as_deref(), Some("my.Driver"));

    // State survives a restart.
    let m3 = DriverManager::new(d.path().join("drivers"), test_catalog());
    assert_eq!(m3.status("toy"), DriverStatus::Custom { files: 1 });
    m3.remove("toy").unwrap();
    assert_eq!(m3.status("toy"), DriverStatus::NotDownloaded);
    assert!(!events.lock().unwrap().is_empty());
}

#[test]
fn unknown_driver_is_not_found() {
    let d = tempfile::tempdir().unwrap();
    let m = DriverManager::new(d.path(), test_catalog());
    assert!(matches!(
        m.download("nope", None),
        Err(ManagerError::NotFound(_))
    ));
    assert!(matches!(
        m.driver_files("toy"),
        Err(ManagerError::NotFound(_))
    ));
}

#[test]
fn extending_the_catalog_with_native_libraries() {
    let mut c = Catalog::builtin();
    c.extend([Artifact {
        id: "snowflake".into(),
        kind: ArtifactKind::NativeLibrary,
        name: "ADBC Snowflake".into(),
        coordinates: "adbc-driver-snowflake".into(),
        license: "Apache-2.0".into(),
        license_url: String::new(),
        driver_class: None,
        versions: vec![ArtifactVersion {
            version: "1.0.0".into(),
            min_java: 0,
            files: vec![ArtifactFile {
                url: "https://files.pythonhosted.org/x.whl".into(),
                sha256: ABC.into(),
                size: None,
                file_name: None,
                extract: Some("adbc_driver_snowflake/libadbc_driver_snowflake.so".into()),
                os: Some(std::env::consts::OS.into()),
                arch: None,
            }],
        }],
    }]);
    let a = c.get("snowflake").unwrap();
    assert_eq!(
        a.versions[0].files[0].local_name(),
        "libadbc_driver_snowflake.so"
    );
    assert!(a.versions[0].files[0].for_this_platform());
}

/// Real download from Maven Central (`FERRODB_NET_TESTS=1`).
#[test]
fn download_h2_from_maven_central() {
    if std::env::var_os("FERRODB_NET_TESTS").is_none() {
        eprintln!("skipped: set FERRODB_NET_TESTS=1");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let m = DriverManager::new(d.path(), Catalog::builtin());
    let v = m.download("h2", Some("2.3.232")).unwrap();
    assert_eq!(v, "2.3.232");
    assert!(matches!(
        m.status("h2"),
        DriverStatus::UpdateAvailable { .. }
    ));
    assert!(m.driver_files("h2").unwrap().files[0].is_file());
}

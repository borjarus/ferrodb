//! The artifact catalog (ADR-007): what can be downloaded, from where, with
//! which checksum and license. The built-in catalog is `catalog.toml`
//! compiled into the binary; other packages (ADBC libraries) add their own
//! artifacts with [`Catalog::extend`] or [`Catalog::parse`].

use serde::{Deserialize, Serialize};

/// What an artifact is used for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    /// A JDBC driver (one or more `.jar` files on the class path).
    Jar,
    /// A shared library loaded in-process (ADBC drivers).
    NativeLibrary,
    /// A Java runtime.
    Runtime,
}

/// One file of an artifact version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactFile {
    /// Trusted source only (Maven Central, vendor or Apache release sites).
    pub url: String,
    /// Lower-case hex SHA-256 of the downloaded file (the archive, when
    /// `extract` is set).
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// File name on disk; default: the last segment of `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// Member of a `.zip` / `.tar.gz` / `.whl` archive to keep (native
    /// libraries shipped inside wheels or tarballs). `None` = the file itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<String>,
    /// `windows` / `linux` / `macos`; `None` = every OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// `x86_64` / `aarch64`; `None` = every architecture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
}

impl ArtifactFile {
    /// Name of the file kept on disk.
    pub fn local_name(&self) -> String {
        if let Some(n) = &self.file_name {
            return n.clone();
        }
        if let Some(e) = &self.extract {
            return e.rsplit('/').next().unwrap_or(e).to_owned();
        }
        self.url
            .rsplit('/')
            .next()
            .unwrap_or("artifact")
            .split('?')
            .next()
            .unwrap_or("artifact")
            .to_owned()
    }

    /// The file applies to the running platform.
    pub fn for_this_platform(&self) -> bool {
        self.os.as_deref().is_none_or(|o| o == std::env::consts::OS)
            && self
                .arch
                .as_deref()
                .is_none_or(|a| a == std::env::consts::ARCH)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub version: String,
    /// Lowest Java feature version the files run on (JDBC drivers).
    #[serde(default)]
    pub min_java: u32,
    pub files: Vec<ArtifactFile>,
}

impl ArtifactVersion {
    pub fn files_for_this_platform(&self) -> impl Iterator<Item = &ArtifactFile> {
        self.files.iter().filter(|f| f.for_this_platform())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Driver id the artifact serves (`oracle`, `h2`, `snowflake`…).
    pub id: String,
    pub kind: ArtifactKind,
    pub name: String,
    /// Maven coordinates (`group:artifact`) or another stable identifier.
    #[serde(default)]
    pub coordinates: String,
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    /// JDBC driver class; `None` = found through `java.sql.Driver` services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_class: Option<String>,
    /// Newest first.
    pub versions: Vec<ArtifactVersion>,
}

impl Artifact {
    pub fn version(&self, v: &str) -> Option<&ArtifactVersion> {
        self.versions.iter().find(|x| x.version == v)
    }

    /// The newest version that runs on Java `java` (`None` = any).
    pub fn latest(&self, java: Option<u32>) -> Option<&ArtifactVersion> {
        self.versions
            .iter()
            .find(|v| java.is_none_or(|j| v.min_java <= j))
            .or_else(|| self.versions.first())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default, rename = "artifact")]
    pub artifacts: Vec<Artifact>,
}

impl Catalog {
    /// The catalog shipped with this build.
    pub fn builtin() -> Self {
        Self::parse(include_str!("../catalog.toml")).expect("built-in catalog.toml is valid")
    }

    pub fn parse(toml_text: &str) -> Result<Self, String> {
        let c: Catalog = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        for a in &c.artifacts {
            for v in &a.versions {
                for f in &v.files {
                    if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(format!("{} {}: bad sha256 {}", a.id, v.version, f.sha256));
                    }
                }
            }
        }
        Ok(c)
    }

    /// Adds artifacts (e.g. ADBC libraries); an id already present is replaced.
    pub fn extend(&mut self, artifacts: impl IntoIterator<Item = Artifact>) {
        for a in artifacts {
            self.artifacts.retain(|x| x.id != a.id);
            self.artifacts.push(a);
        }
    }

    pub fn get(&self, id: &str) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| a.id == id)
    }
}

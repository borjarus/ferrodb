//! [`ScriptLibrary`]: built-ins plus the scripts of `<config>/scripts`,
//! compiled once and recompiled when a file changes.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::SystemTime,
};

use ferrodb_api::{
    BoxFuture, Error, ExtScriptInfo, ExtScriptKind, FormatOptions, Result, ScriptService,
    ScriptTable,
};

use crate::{
    builtin::BUILTINS,
    script::{CompiledScript, compile, template},
};

struct Entry {
    info: ExtScriptInfo,
    script: Option<CompiledScript>,
    /// Modification time when compiled (user scripts).
    mtime: Option<SystemTime>,
}

#[derive(Default)]
struct State {
    /// (kind, id) → script; built-ins and user scripts.
    entries: BTreeMap<(Kind, String), Entry>,
}

/// `ExtScriptKind` is not `Ord`; this orders keys by kind.
type Kind = u8;

fn kind_key(k: ExtScriptKind) -> Kind {
    match k {
        ExtScriptKind::Extractor => 0,
        ExtScriptKind::Aggregator => 1,
    }
}

const KINDS: [ExtScriptKind; 2] = [ExtScriptKind::Extractor, ExtScriptKind::Aggregator];

fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::Other(format!("{}: {e}", path.display()))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn entry_from(kind: ExtScriptKind, id: &str, source: &str, path: Option<PathBuf>) -> Entry {
    let mt = path.as_deref().and_then(mtime);
    let (info, script) = compile(kind, id, source, path);
    Entry {
        info,
        script: script.ok(),
        mtime: mt,
    }
}

fn load_file(kind: ExtScriptKind, path: &Path) -> Option<(String, Entry)> {
    let id = path.file_stem()?.to_str()?.to_owned();
    let entry = match std::fs::read_to_string(path) {
        Ok(src) => entry_from(kind, &id, &src, Some(path.to_owned())),
        Err(e) => Entry {
            info: ExtScriptInfo {
                id: id.clone(),
                kind,
                name: id.clone(),
                description: String::new(),
                extension: "txt".into(),
                path: Some(path.to_owned()),
                builtin: false,
                error: Some(e.to_string()),
            },
            script: None,
            mtime: mtime(path),
        },
    };
    Some((id, entry))
}

struct Inner {
    dir: Option<PathBuf>,
    state: RwLock<State>,
}

impl Inner {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|p| p.into_inner())
    }

    /// Built-ins plus every `*.rhai` file of the scripts directory.
    fn scan(&self) -> Result<()> {
        let mut entries = BTreeMap::new();
        for (kind, stem, src) in BUILTINS {
            let id = format!("builtin:{stem}");
            let e = entry_from(*kind, &id, src, None);
            entries.insert((kind_key(*kind), id), e);
        }
        if let Some(dir) = &self.dir {
            for kind in KINDS {
                let sub = dir.join(kind.dir_name());
                let Ok(rd) = std::fs::read_dir(&sub) else {
                    continue;
                };
                for f in rd.flatten() {
                    let p = f.path();
                    if p.is_file()
                        && p.extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("rhai"))
                        && let Some((id, e)) = load_file(kind, &p)
                    {
                        entries.insert((kind_key(kind), id), e);
                    }
                }
            }
        }
        self.write().entries = entries;
        Ok(())
    }

    /// The compiled script; a user script whose file changed is recompiled.
    fn get(&self, kind: ExtScriptKind, id: &str) -> Result<CompiledScript> {
        let key = (kind_key(kind), id.to_owned());
        let stale = {
            let st = self.read();
            let e = st.entries.get(&key);
            match e {
                Some(e) => e
                    .info
                    .path
                    .as_deref()
                    .filter(|p| mtime(p) != e.mtime)
                    .map(Path::to_owned),
                // A file added since the last scan.
                None => self
                    .dir
                    .as_ref()
                    .map(|d| d.join(kind.dir_name()).join(format!("{id}.rhai")))
                    .filter(|p| p.is_file()),
            }
        };
        if let Some(path) = stale {
            match load_file(kind, &path) {
                Some((_, e)) if path.exists() => {
                    self.write().entries.insert(key.clone(), e);
                }
                _ => {
                    self.write().entries.remove(&key);
                }
            }
        }
        let st = self.read();
        let e = st.entries.get(&key).ok_or_else(|| {
            Error::NotFound(format!(
                "{} script `{id}`",
                match kind {
                    ExtScriptKind::Extractor => "extractor",
                    ExtScriptKind::Aggregator => "aggregator",
                }
            ))
        })?;
        match (&e.script, &e.info.error) {
            (Some(s), _) => Ok(s.clone()),
            (None, err) => Err(Error::Other(format!(
                "script `{id}` does not compile: {}",
                err.as_deref().unwrap_or("unknown error")
            ))),
        }
    }
}

/// Built-in and user extension scripts; implements [`ScriptService`].
/// Cheap to clone; clones share the cache.
#[derive(Clone)]
pub struct ScriptLibrary {
    inner: Arc<Inner>,
}

impl ScriptLibrary {
    /// Built-ins plus the scripts under `dir` (`<config>/scripts`); the
    /// directory is created on demand by [`ScriptService::create`].
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::build(Some(dir.into()))
    }

    /// Built-ins only, nothing on disk (mock backend, tests).
    pub fn in_memory() -> Self {
        Self::build(None)
    }

    fn build(dir: Option<PathBuf>) -> Self {
        let lib = Self {
            inner: Arc::new(Inner {
                dir,
                state: RwLock::default(),
            }),
        };
        let _ = lib.inner.scan();
        lib
    }

    /// An extractor ready to run (export to a file). Blocking: may read and
    /// recompile a changed file.
    pub fn extractor(&self, id: &str) -> Result<CompiledScript> {
        self.inner.get(ExtScriptKind::Extractor, id)
    }

    /// An aggregator ready to run. Blocking, like [`Self::extractor`].
    pub fn aggregator(&self, id: &str) -> Result<CompiledScript> {
        self.inner.get(ExtScriptKind::Aggregator, id)
    }

    /// Re-reads the scripts directory (blocking).
    pub fn reload_now(&self) -> Result<()> {
        self.inner.scan()
    }

    /// Writes a template for a new script (blocking); see
    /// [`ScriptService::create`].
    pub fn create_now(&self, kind: ExtScriptKind, name: &str) -> Result<PathBuf> {
        let dir = self
            .inner
            .dir
            .as_ref()
            .ok_or_else(|| Error::Unsupported("scripts are not stored on disk".into()))?
            .join(kind.dir_name());
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
        let base = slug(name);
        let src = template(kind, if name.trim().is_empty() { &base } else { name });
        for n in 1..1000 {
            let stem = if n == 1 {
                base.clone()
            } else {
                format!("{base}-{n}")
            };
            let path = dir.join(format!("{stem}.rhai"));
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path);
            match file {
                Ok(mut f) => {
                    use std::io::Write;
                    f.write_all(src.as_bytes()).map_err(|e| io_err(&path, e))?;
                    drop(f);
                    let e = entry_from(kind, &stem, &src, Some(path.clone()));
                    self.inner.write().entries.insert((kind_key(kind), stem), e);
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_err(&path, e)),
            }
        }
        Err(Error::Other(format!("too many scripts named `{base}`")))
    }

    /// Runs `f` on its own thread; the future only awaits a channel, so any
    /// executor can poll it.
    fn off_thread<T: Send + 'static>(
        &self,
        f: impl FnOnce(ScriptLibrary) -> Result<T> + Send + 'static,
    ) -> BoxFuture<'static, Result<T>> {
        let (tx, rx) = async_channel::bounded(1);
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("ferrodb-script".into())
            .spawn(move || {
                let _ = tx.send_blocking(f(me));
            });
        Box::pin(async move {
            spawned.map_err(|e| Error::Other(format!("script thread: {e}")))?;
            rx.recv()
                .await
                .map_err(|_| Error::Other("script thread stopped".into()))?
        })
    }
}

/// File stem for a script name: lower-case letters, digits and `-`.
fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            s.extend(c.to_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').to_owned();
    if s.is_empty() { "script".into() } else { s }
}

impl ScriptService for ScriptLibrary {
    fn list(&self, kind: ExtScriptKind) -> Vec<ExtScriptInfo> {
        let st = self.inner.read();
        let mut v: Vec<ExtScriptInfo> = st
            .entries
            .iter()
            .filter(|((k, _), _)| *k == kind_key(kind))
            .map(|(_, e)| e.info.clone())
            .collect();
        // Built-ins in their declared order, then user scripts by name.
        let order = |i: &ExtScriptInfo| {
            BUILTINS
                .iter()
                .position(|(_, stem, _)| i.id.strip_prefix("builtin:") == Some(stem))
                .unwrap_or(usize::MAX)
        };
        v.sort_by(|a, b| {
            (!a.builtin, order(a), a.name.to_lowercase(), &a.id).cmp(&(
                !b.builtin,
                order(b),
                b.name.to_lowercase(),
                &b.id,
            ))
        });
        v
    }

    fn reload(&self) -> BoxFuture<'_, Result<()>> {
        self.off_thread(|me| me.reload_now())
    }

    fn directory(&self) -> Option<PathBuf> {
        self.inner.dir.clone()
    }

    fn create(&self, kind: ExtScriptKind, name: &str) -> BoxFuture<'_, Result<PathBuf>> {
        let name = name.to_owned();
        self.off_thread(move |me| me.create_now(kind, &name))
    }

    fn aggregate(&self, id: &str, cells: ScriptTable) -> BoxFuture<'_, Result<String>> {
        let id = id.to_owned();
        self.off_thread(move |me| me.aggregator(&id)?.aggregate(&cells))
    }

    fn extract(&self, id: &str, table: ScriptTable) -> BoxFuture<'_, Result<String>> {
        let id = id.to_owned();
        self.off_thread(move |me| {
            me.extractor(&id)?
                .extract(&table, &FormatOptions::default())
        })
    }
}

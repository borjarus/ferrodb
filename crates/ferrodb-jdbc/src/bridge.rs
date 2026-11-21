//! The host process: started lazily on the first JDBC connection, watched
//! by a reader task, restarted after a crash. Connections of a dead process
//! fail with a connection error; new connections get a fresh process.

use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use ferrodb_api::{BridgeState, Error, Result};
use ferrodb_driver_manager::DriverManager;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::mpsc,
};

use crate::protocol::{self, HostError};

/// How long the JVM may take to answer `hello` (cold start, slow disks).
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// Lines of the host's stderr kept for crash reports.
const STDERR_TAIL: usize = 40;

/// What a request receives from the host.
#[derive(Debug)]
pub enum Incoming {
    Event(Value),
    Data(Vec<u8>),
    Done(std::result::Result<Value, HostError>),
}

/// A request in flight.
pub struct Pending {
    pub id: u64,
    pub rx: mpsc::UnboundedReceiver<Incoming>,
}

type Listener = Arc<dyn Fn(&BridgeState) + Send + Sync>;

/// One running `ferrodb-jdbc-host` JVM.
pub struct Process {
    pid: u32,
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<u64, mpsc::UnboundedSender<Incoming>>>,
    alive: AtomicBool,
    next_id: AtomicU64,
    connections: AtomicUsize,
    stderr: Mutex<VecDeque<String>>,
    child: Mutex<Option<Child>>,
    java: String,
}

impl Process {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// Why the process is gone, with the end of its stderr.
    pub fn lost(&self) -> Error {
        let tail: Vec<String> = self
            .stderr
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .rev()
            .take(5)
            .rev()
            .cloned()
            .collect();
        if tail.is_empty() {
            Error::Connection("the JDBC bridge process exited".into())
        } else {
            Error::Connection(format!(
                "the JDBC bridge process exited: {}",
                tail.join(" | ")
            ))
        }
    }

    /// Sends a request; answers arrive on the returned channel.
    pub fn request(&self, method: &str, params: Value) -> Result<Pending> {
        if !self.is_alive() {
            return Err(self.lost());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, tx);
        if self
            .out
            .send(protocol::request(id, method, params))
            .is_err()
        {
            self.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err(self.lost());
        }
        Ok(Pending { id, rx })
    }

    /// A request answered with one JSON value (no data).
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut p = self.request(method, params)?;
        loop {
            match p.rx.recv().await {
                Some(Incoming::Done(r)) => return r.map_err(|e| e.into_error(None)),
                Some(_) => continue,
                None => return Err(self.lost()),
            }
        }
    }

    /// Fire and forget (used from `Drop`).
    pub fn notify(&self, method: &str, params: Value) {
        if self.is_alive() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let _ = self.out.send(protocol::request(id, method, params));
        }
    }

    pub fn credit(&self, id: u64, n: i32) {
        let _ = self.out.send(protocol::credit(id, n));
    }

    /// Cancels request `target`; `truncate` = the consumer stopped reading.
    pub fn cancel(&self, target: u64, truncate: bool) {
        self.notify("cancel", json!({ "target": target, "truncate": truncate }));
    }

    pub(crate) fn connection_opened(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn connection_closed(&self) {
        let _ = self
            .connections
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    fn kill(&self) {
        if let Some(c) = self
            .child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_mut()
        {
            let _ = c.start_kill();
        }
    }
}

/// Supervises the host process.
pub struct Bridge {
    me: Weak<Bridge>,
    manager: Arc<DriverManager>,
    host_jar: Option<PathBuf>,
    current: tokio::sync::Mutex<Option<Arc<Process>>>,
    state: Mutex<BridgeState>,
    listeners: Mutex<Vec<Listener>>,
    shut_down: AtomicBool,
}

impl Bridge {
    /// `host_jar` = `ferrodb-jdbc-host.jar`; `None` = [`find_host_jar`].
    pub fn new(manager: Arc<DriverManager>, host_jar: Option<PathBuf>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            manager,
            host_jar,
            current: tokio::sync::Mutex::new(None),
            state: Mutex::new(BridgeState::NotStarted),
            listeners: Mutex::default(),
            shut_down: AtomicBool::new(false),
        })
    }

    pub fn manager(&self) -> &Arc<DriverManager> {
        &self.manager
    }

    pub fn state(&self) -> BridgeState {
        let s = self.state.lock().unwrap_or_else(|p| p.into_inner()).clone();
        // The connection count changes without a state transition.
        match s {
            BridgeState::Running { pid, .. } => {
                let n = self
                    .current
                    .try_lock()
                    .ok()
                    .and_then(|c| c.as_ref().map(|p| p.connections()));
                BridgeState::Running {
                    pid,
                    connections: n.unwrap_or(0),
                }
            }
            other => other,
        }
    }

    pub fn on_change(&self, f: impl Fn(&BridgeState) + Send + Sync + 'static) {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Arc::new(f));
    }

    fn set_state(&self, s: BridgeState) {
        *self.state.lock().unwrap_or_else(|p| p.into_inner()) = s.clone();
        let ls: Vec<Listener> = self
            .listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for l in ls {
            l(&s);
        }
    }

    /// The running process, started (or restarted) when needed.
    pub async fn process(&self) -> Result<Arc<Process>> {
        let mut cur = self.current.lock().await;
        if let Some(p) = cur.as_ref().filter(|p| p.is_alive()) {
            return Ok(p.clone());
        }
        self.shut_down.store(false, Ordering::Release);
        let restarting = cur.is_some();
        self.set_state(if restarting {
            BridgeState::Restarting
        } else {
            BridgeState::Starting
        });
        match self.start().await {
            Ok(p) => {
                self.set_state(BridgeState::Running {
                    pid: p.pid,
                    connections: p.connections(),
                });
                *cur = Some(p.clone());
                Ok(p)
            }
            Err(e) => {
                self.set_state(BridgeState::Failed(e.to_string()));
                *cur = None;
                Err(e)
            }
        }
    }

    fn jar(&self) -> Result<PathBuf> {
        match &self.host_jar {
            Some(j) if j.is_file() => Ok(j.clone()),
            Some(j) => Err(Error::Connection(format!(
                "JDBC bridge: {} not found",
                j.display()
            ))),
            None => find_host_jar().ok_or_else(|| {
                Error::Connection(
                    "JDBC bridge: ferrodb-jdbc-host.jar not found (build it with \
                     `mvn -f java/jdbc-host package` or set FERRODB_JDBC_HOST_JAR)"
                        .into(),
                )
            }),
        }
    }

    async fn start(&self) -> Result<Arc<Process>> {
        let jar = self.jar()?;
        let manager = self.manager.clone();
        let java = tokio::task::spawn_blocking(move || manager.java())
            .await
            .map_err(|e| Error::Other(e.to_string()))?
            .ok_or_else(|| {
                Error::DriverMissing(format!(
                    "Java runtime ≥ {} (Drivers → Download Java)",
                    ferrodb_driver_manager::MIN_JAVA
                ))
            })?;
        let mut cmd = Command::new(&java.java);
        cmd.arg("--add-opens=java.base/java.nio=ALL-UNNAMED")
            .arg("-Dfile.encoding=UTF-8")
            .arg("-Dstdout.encoding=UTF-8")
            .arg("-XX:+UseSerialGC")
            .arg("-jar")
            .arg(&jar)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console window flashing up from the GUI.
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd.spawn().map_err(|e| {
            Error::Connection(format!(
                "JDBC bridge: cannot start {}: {e}",
                java.java.display()
            ))
        })?;
        let pid = child.id().unwrap_or(0);
        let mut stdin = child.stdin.take().expect("piped");
        let mut stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let proc = Arc::new(Process {
            pid,
            out: out_tx,
            pending: Mutex::default(),
            alive: AtomicBool::new(true),
            next_id: AtomicU64::new(1),
            connections: AtomicUsize::new(0),
            stderr: Mutex::default(),
            child: Mutex::new(Some(child)),
            java: java.version.clone(),
        });
        // Writer: frames in order, one task owning stdin.
        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        // Stderr: logged, the tail kept for error messages.
        let weak = Arc::downgrade(&proc);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "jdbc-host", "{line}");
                let Some(p) = weak.upgrade() else { break };
                let mut t = p.stderr.lock().unwrap_or_else(|x| x.into_inner());
                if t.len() == STDERR_TAIL {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });
        // Reader: routes frames to requests; on exit fails them all.
        let reader_proc = proc.clone();
        let bridge = self.me.clone();
        tokio::spawn(async move {
            let p = reader_proc;
            while let Ok(Some(f)) = protocol::read_frame(&mut stdout).await {
                {
                    {
                        let msg = match f.kind {
                            protocol::DATA => Incoming::Data(f.payload),
                            protocol::EVENT | protocol::RESPONSE => {
                                let v: Value = serde_json::from_slice(&f.payload)
                                    .unwrap_or_else(|e| json!({ "error": {"kind": "protocol", "message": e.to_string()} }));
                                if f.kind == protocol::EVENT {
                                    Incoming::Event(v)
                                } else if let Some(err) = v.get("error") {
                                    Incoming::Done(Err(HostError::from_json(err)))
                                } else {
                                    Incoming::Done(Ok(v
                                        .get("result")
                                        .cloned()
                                        .unwrap_or(Value::Null)))
                                }
                            }
                            _ => continue,
                        };
                        let done = matches!(msg, Incoming::Done(_));
                        let mut pending = p.pending.lock().unwrap_or_else(|x| x.into_inner());
                        if let Some(tx) = pending.get(&f.id) {
                            let _ = tx.send(msg);
                        }
                        if done {
                            pending.remove(&f.id);
                        }
                    }
                }
            }
            p.alive.store(false, Ordering::Release);
            p.pending.lock().unwrap_or_else(|x| x.into_inner()).clear();
            if let Some(b) = bridge.upgrade() {
                b.exited(&p);
            }
        });
        let hello = tokio::time::timeout(START_TIMEOUT, proc.call("hello", json!({})))
            .await
            .map_err(|_| {
                proc.kill();
                Error::Connection("JDBC bridge: the Java host did not start in time".into())
            })??;
        if hello["protocol"].as_u64() != Some(protocol::PROTOCOL) {
            proc.kill();
            return Err(Error::Connection(format!(
                "JDBC bridge: protocol {} spoken by the host, {} expected (rebuild ferrodb-jdbc-host)",
                hello["protocol"],
                protocol::PROTOCOL
            )));
        }
        tracing::info!(pid, java = %proc.java, "JDBC bridge started");
        Ok(proc)
    }

    /// The reader saw the process exit.
    fn exited(&self, p: &Arc<Process>) {
        if self.shut_down.load(Ordering::Acquire) {
            self.set_state(BridgeState::NotStarted);
            return;
        }
        tracing::warn!(pid = p.pid, "JDBC bridge process exited; restarting");
        self.set_state(BridgeState::Restarting);
        // Restart at once so the Services panel shows a running bridge again;
        // sessions of the dead process stay disconnected.
        if let Some(me) = self.me.upgrade() {
            tokio::spawn(async move {
                let _ = me.process().await;
            });
        }
    }

    /// Stops the host (application exit). The next connection starts it again.
    pub async fn shutdown(&self) {
        self.shut_down.store(true, Ordering::Release);
        let p = self.current.lock().await.take();
        if let Some(p) = p {
            let _ =
                tokio::time::timeout(Duration::from_secs(2), p.call("shutdown", json!({}))).await;
            p.kill();
        }
        self.set_state(BridgeState::NotStarted);
    }

    /// Kills the process as a crash would (diagnostics, tests).
    pub async fn kill(&self) {
        if let Some(p) = self.current.lock().await.as_ref() {
            p.kill();
        }
    }
}

/// Where `ferrodb-jdbc-host.jar` is looked for: `FERRODB_JDBC_HOST_JAR`, next
/// to the executable (and in `../lib`, `../Resources` of an app bundle), and
/// the Maven output of a source checkout.
pub fn find_host_jar() -> Option<PathBuf> {
    const NAME: &str = "ferrodb-jdbc-host.jar";
    if let Some(p) = std::env::var_os("FERRODB_JDBC_HOST_JAR").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.clone());
        dirs.push(exe_dir.join("../lib"));
        dirs.push(exe_dir.join("../Resources"));
    }
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../java/jdbc-host/target"));
    dirs.into_iter().map(|d| d.join(NAME)).find(|p| p.is_file())
}

//! Phase 0 JDBC bridge spike — Rust side (plan 5.9 and section 7).
//!
//! Listens on a loopback socket, starts the Java host with a one-time token
//! (passed via environment, never via arguments), authenticates the connection
//! and reads the Arrow IPC stream, reporting throughput and memory.
//!
//! ```text
//! # Java side (Java 21+, Gradle):
//! cd spikes/jdbc-bridge/java && gradle fatJar
//! cargo xtask spike jdbc-reader -- --jar spikes/jdbc-bridge/java/build/libs/jdbc-host-spike-all.jar --rows 1000000
//!
//! # Rust side only (synthetic producer in-process, no Java needed):
//! cargo xtask spike jdbc-reader -- --self-test --rows 1000000
//! ```
//!
//! Go/no-go (plan, section 7): ≥ 200k rows/s at constant memory.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io::{BufRead, BufReader, BufWriter, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

use arrow_ipc::{reader::StreamReader, writer::StreamWriter};

struct Args {
    jar: Option<String>,
    java: String,
    rows: u64,
    self_test: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        jar: None,
        java: "java".into(),
        rows: 1_000_000,
        self_test: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--jar" => a.jar = it.next(),
            "--java" => a.java = it.next().unwrap_or(a.java),
            "--rows" => a.rows = it.next().and_then(|s| s.parse().ok()).unwrap_or(a.rows),
            "--self-test" => a.self_test = true,
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    a
}

fn token() -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(Instant::now().elapsed().as_nanos() ^ std::process::id() as u128);
    let a = h.finish();
    let mut h = RandomState::new().build_hasher();
    h.write_u64(a);
    format!("{a:016x}{:016x}", h.finish())
}

fn rss_mb() -> f64 {
    memory_stats::memory_stats()
        .map(|m| m.physical_mem as f64 / 1_048_576.0)
        .unwrap_or(0.0)
}

/// In-process producer used by `--self-test`: same wire format as the Java host.
fn synthetic_producer(port: u16, token: String, rows: u64) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
        let mut out = BufWriter::with_capacity(1 << 20, stream);
        out.write_all(format!("{token}\n").as_bytes()).unwrap();
        let schema = ferrodb_mock::data::table_schema(5);
        let mut w = StreamWriter::try_new(&mut out, &schema).expect("writer");
        let mut at = 0;
        while at < rows {
            let n = (rows - at).min(4096) as usize;
            w.write(&ferrodb_mock::data::table_batch(&schema, at, n))
                .expect("write batch");
            at += n as u64;
        }
        w.finish().expect("finish");
    })
}

fn spawn_java(args: &Args, port: u16, token: &str) -> std::io::Result<Child> {
    let jar = args
        .jar
        .as_deref()
        .expect("--jar required without --self-test");
    Command::new(&args.java)
        .args(["--add-opens=java.base/java.nio=ALL-UNNAMED", "-jar", jar])
        .arg(port.to_string())
        .arg(args.rows.to_string())
        .env("FERRODB_BRIDGE_TOKEN", token)
        .spawn()
}

fn main() {
    let args = parse_args();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let token = token();
    let rss_before = rss_mb();

    let started = Instant::now();
    let mut child = None;
    let producer = if args.self_test {
        Some(synthetic_producer(port, token.clone(), args.rows))
    } else {
        child = Some(spawn_java(&args, port, &token).expect("start java"));
        None
    };

    listener.set_nonblocking(false).unwrap();
    let (stream, peer) = listener.accept().expect("accept");
    let connected = started.elapsed();
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut reader = BufReader::with_capacity(1 << 20, stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read token");
    if line.trim_end() != token {
        eprintln!("rejected connection from {peer}: bad token");
        std::process::exit(2);
    }
    println!("[jdbc] producer connected after {connected:?} (process start + JVM + driver)");

    let t = Instant::now();
    let stream = StreamReader::try_new(reader, None).expect("arrow stream");
    println!(
        "[jdbc] schema: {:?}",
        stream
            .schema()
            .fields()
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
    );
    let (mut rows, mut batches, mut peak_rss, mut first) = (0usize, 0usize, 0f64, None);
    for batch in stream {
        let batch = batch.expect("batch");
        first.get_or_insert(t.elapsed());
        rows += batch.num_rows();
        batches += 1;
        if batches % 32 == 0 {
            peak_rss = peak_rss.max(rss_mb());
        }
    }
    let elapsed = t.elapsed();
    peak_rss = peak_rss.max(rss_mb());
    if let Some(p) = producer {
        p.join().unwrap();
    }
    if let Some(mut c) = child {
        let _ = c.wait();
    }
    let rate = rows as f64 / elapsed.as_secs_f64();
    println!(
        "[jdbc] {rows} rows in {batches} batches; first batch {:?}; total {elapsed:?} → {rate:.0} rows/s",
        first.unwrap_or_default()
    );
    println!("[jdbc] RSS before {rss_before:.1} MB, peak while streaming {peak_rss:.1} MB");
    println!(
        "[jdbc] go/no-go (≥ 200k rows/s): {}",
        if rate >= 200_000.0 { "GO" } else { "NO-GO" }
    );
}

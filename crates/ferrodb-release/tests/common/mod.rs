//! A tiny HTTP/1.1 server on loopback (std only): serves files from a map
//! and records POST bodies.
#![allow(dead_code)] // each test binary uses part of it

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
};

/// (path, body) of each POST.
type Posts = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

#[derive(Clone, Default)]
pub struct Server {
    pub port: u16,
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub posts: Posts,
}

impl Server {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let s = server.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || s.handle(stream));
            }
        });
        server
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub fn put(&self, path: &str, body: impl Into<Vec<u8>>) {
        self.files.lock().unwrap().insert(path.into(), body.into());
    }

    pub fn remove(&self, path: &str) {
        self.files.lock().unwrap().remove(path);
    }

    fn handle(&self, stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_owned();
        let path = parts.next().unwrap_or("").to_owned();
        let mut length = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut stream = stream;
        if method == "POST" {
            let mut body = vec![0u8; length];
            let _ = reader.read_exact(&mut body);
            self.posts.lock().unwrap().push((path, body));
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
        let body = self.files.lock().unwrap().get(&path).cloned();
        let _ = match body {
            Some(b) => stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        b.len()
                    )
                    .as_bytes(),
                )
                .and_then(|_| stream.write_all(&b)),
            None => stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ),
        };
    }
}

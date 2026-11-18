use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use tracing_subscriber::{Layer, filter, layer::SubscriberExt, util::SubscriberInitExt};

const MAX_FILE: u64 = 5 * 1024 * 1024;
const MAX_LINE: usize = 8 * 1024;
const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

pub(crate) fn init(directory: &Path) {
    let log = Arc::new(Mutex::new(Log {
        current: directory.join("friday.jsonl"),
        previous: directory.join("friday.previous.jsonl"),
    }));
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_writer(move || LogLine {
            bytes: Vec::new(),
            log: log.clone(),
            overflow: false,
        })
        .with_filter(filter::filter_fn(|metadata| {
            *metadata.level() <= tracing::Level::INFO
                && ["friday_desktop", "friday_engine", "friday_adapters"]
                    .iter()
                    .any(|name| metadata.target().starts_with(name))
        }));
    let _ = tracing_subscriber::registry().with(layer).try_init();
    // The panic message and stack may contain user-controlled content.
    std::panic::set_hook(Box::new(|_| {
        tracing::error!(build = env!("CARGO_PKG_VERSION"), "unexpected panic")
    }));
}

struct Log {
    current: PathBuf,
    previous: PathBuf,
}

fn expired(path: &Path) -> bool {
    path.metadata()
        .and_then(|meta| meta.modified())
        .is_ok_and(|time| time.elapsed().is_ok_and(|age| age > RETENTION))
}

impl Log {
    fn append(&self, bytes: &[u8]) -> io::Result<()> {
        if expired(&self.current) {
            let _ = fs::remove_file(&self.current);
        }
        if expired(&self.previous) {
            let _ = fs::remove_file(&self.previous);
        }
        if self
            .current
            .metadata()
            .is_ok_and(|meta| meta.len() + bytes.len() as u64 > MAX_FILE)
        {
            let _ = fs::remove_file(&self.previous);
            fs::rename(&self.current, &self.previous)?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.current)?
            .write_all(bytes)
    }
}

struct LogLine {
    bytes: Vec<u8>,
    log: Arc<Mutex<Log>>,
    overflow: bool,
}

impl Write for LogLine {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + data.len() <= MAX_LINE && !self.overflow {
            self.bytes.extend_from_slice(data);
        } else {
            self.overflow = true;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogLine {
    fn drop(&mut self) {
        if !self.overflow
            && !self.bytes.is_empty()
            && let Ok(log) = self.log.lock()
        {
            let _ = log.append(&self.bytes);
        }
    }
}

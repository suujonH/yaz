use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use chrono::Local;
use log::{LevelFilter, Log, Metadata, Record, SetLoggerError};
use parking_lot::Mutex;

/// Simple file logging: appends to `logs\yyyyMMdd.log`, one line per entry as `HH:mm:ss.fff [level]
/// message`. Write failures are ignored.
pub struct FileLogger {
    dir: PathBuf,
    level: LevelFilter,
    gate: Mutex<()>,
}

impl FileLogger {
    pub fn new(dir: impl Into<PathBuf>, level: LevelFilter) -> Self {
        Self {
            dir: dir.into(),
            level,
            gate: Mutex::new(()),
        }
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self
            .dir
            .join(format!("{}.log", Local::now().format("%Y%m%d")));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{line}")
    }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} [{}] {}",
            Local::now().format("%H:%M:%S%.3f"),
            record.level(),
            record.args()
        );
        let _guard = self.gate.lock();
        // Failure to write the log should not affect the app
        let _ = self.append(&line);
    }

    fn flush(&self) {}
}

/// Makes the `log` macros write to `dir`; may be called only once.
pub fn init_logging(dir: impl Into<PathBuf>, level: LevelFilter) -> Result<(), SetLoggerError> {
    log::set_boxed_logger(Box::new(FileLogger::new(dir, level)))?;
    log::set_max_level(level);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(logger: &FileLogger, level: log::Level, message: &str) {
        logger.log(
            &Record::builder()
                .level(level)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    #[test]
    fn appends_lines_to_the_daily_file() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let logger = FileLogger::new(&logs, LevelFilter::Info);
        write(&logger, log::Level::Info, "第一行");
        write(&logger, log::Level::Warn, "第二行");
        write(&logger, log::Level::Debug, "不记录");

        let name = format!("{}.log", Local::now().format("%Y%m%d"));
        let text = std::fs::read_to_string(logs.join(name)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("[INFO] 第一行"), "{}", lines[0]);
        assert!(lines[1].ends_with("[WARN] 第二行"));
        // HH:mm:ss.fff
        let time = lines[0].split(' ').next().unwrap();
        assert_eq!(time.len(), 12);
        assert_eq!(&time[2..3], ":");
        assert_eq!(&time[8..9], ".");
    }

    #[test]
    fn unwritable_directory_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, "").unwrap();
        // A file sits at the log directory's location: writing fails, but no panic
        write(
            &FileLogger::new(blocker.join("logs"), LevelFilter::Info),
            log::Level::Error,
            "x",
        );
    }
}

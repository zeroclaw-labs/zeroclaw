use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Daily-rotating file writer.
///
/// Produces files named `{stem}.{YYMMDD}.{ext}` (e.g. `zeroclaw.260517.log`).
/// On each `write()` call the current date is compared against the last-seen
/// date; when the day rolls over the old file is flushed and a new one opened.
pub struct DailyRotatingFile {
    dir: PathBuf,
    stem: String,
    ext: String,
    current_date: String,
    file: BufWriter<File>,
}

impl DailyRotatingFile {
    pub fn new(dir: impl Into<PathBuf>, filename: &str) -> io::Result<Self> {
        let dir = dir.into();
        let (stem, ext) = split_stem_ext(filename);
        let date = today_yymmdd();
        let file = open_file(&dir, &stem, &ext, &date)?;
        Ok(Self { dir, stem, ext, current_date: date, file })
    }
}

fn today_yymmdd() -> String {
    chrono::Local::now().format("%y%m%d").to_string()
}

fn split_stem_ext(filename: &str) -> (String, String) {
    match filename.rsplit_once('.') {
        Some((stem, ext)) => (stem.to_owned(), ext.to_owned()),
        None => (filename.to_owned(), String::new()),
    }
}

fn open_file(dir: &Path, stem: &str, ext: &str, date: &str) -> io::Result<BufWriter<File>> {
    let name = if ext.is_empty() {
        format!("{}.{}", stem, date)
    } else {
        format!("{}.{}.{}", stem, date, ext)
    };
    std::fs::create_dir_all(dir)?;
    let f = OpenOptions::new().create(true).append(true).open(dir.join(name))?;
    Ok(BufWriter::new(f))
}

impl Write for DailyRotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let today = today_yymmdd();
        if today != self.current_date {
            self.file.flush().ok();
            match open_file(&self.dir, &self.stem, &self.ext, &today) {
                Ok(f) => {
                    self.file = f;
                    self.current_date = today;
                }
                Err(e) => {
                    // Keep writing to old file rather than losing data
                    eprintln!("Warning: failed to rotate log file: {e}");
                }
            }
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

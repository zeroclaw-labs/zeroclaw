use rusqlite::Connection;
use std::sync::Mutex;

pub struct DbPool {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
}

impl DbPool {
    pub fn open(path: &str, reader_count: usize) -> anyhow::Result<Self> {
        let writer = Connection::open(path)?;
        writer.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;

        let mut readers = Vec::with_capacity(reader_count);
        for _ in 0..reader_count {
            let r = Connection::open(path)?;
            r.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;",
            )?;
            readers.push(Mutex::new(r));
        }

        Ok(Self {
            writer: Mutex::new(writer),
            readers,
        })
    }

    pub fn write<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&Connection) -> anyhow::Result<T>,
    {
        let conn = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("writer lock poisoned"))?;
        f(&conn)
    }

    pub fn read<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&Connection) -> anyhow::Result<T>,
    {
        for reader in &self.readers {
            if let Ok(conn) = reader.try_lock() {
                return f(&conn);
            }
        }
        let conn = self.readers[0]
            .lock()
            .map_err(|_| anyhow::anyhow!("reader lock poisoned"))?;
        f(&conn)
    }
}

//! SQLite FTS5 is a disposable read model for both durable backends. Each
//! index contains only the caller's authorized, current-erasure message view.
//! Keeping it in memory avoids a second retained copy of sensitive history.
use super::*;
use zeroclaw_api::session_events::search::SessionSearchPort;

pub struct FullTextSearch;
impl SessionSearchPort for FullTextSearch {
    fn matching_positions(
        &self,
        texts: &[&str],
        query: &str,
        limit: usize,
    ) -> Result<Vec<usize>, ReadError> {
        if limit == 0 || limit > 1000 || query.trim().is_empty() || query.len() > 4096 {
            return Err(ReadError::TooLarge);
        }
        let mut conn = Connection::open_in_memory().map_err(read_error)?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE docs USING fts5(text, tokenize='trigram case_sensitive 1');",
        )
        .map_err(read_error)?;
        {
            let transaction = conn.transaction().map_err(read_error)?;
            let mut insert = transaction
                .prepare("INSERT INTO docs(rowid,text) VALUES (?1,?2)")
                .map_err(read_error)?;
            for (position, text) in texts.iter().enumerate() {
                let row = i64::try_from(position).map_err(|_| ReadError::TooLarge)?;
                insert
                    .execute(params![row, text.to_lowercase()])
                    .map_err(read_error)?;
            }
            drop(insert);
            transaction.commit().map_err(read_error)?;
        }
        let query = query.trim().to_lowercase();
        // Trigram FTS cannot match one/two-character terms. Preserve substring
        // semantics (including Chinese) with a bounded SQL scan for those.
        let (sql, parameter) = if query.chars().count() < 3 {
            (
                "SELECT rowid FROM docs WHERE instr(text,?1)>0 ORDER BY rowid LIMIT ?2",
                query,
            )
        } else {
            (
                "SELECT rowid FROM docs WHERE docs MATCH ?1 ORDER BY rowid LIMIT ?2",
                format!("\"{}\"", query.replace('"', "\"\"")),
            )
        };
        let mut statement = conn.prepare(sql).map_err(read_error)?;
        let rows = statement
            .query_map(params![parameter, limit as i64], |row| row.get::<_, i64>(0))
            .map_err(read_error)?;
        rows.map(|row| {
            usize::try_from(row.map_err(read_error)?)
                .map_err(|error| ReadError::Corrupt(error.to_string()))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_text_uses_literal_unicode_substrings_and_bounded_positions() {
        let index = FullTextSearch;
        let texts = [
            "Reasoning stays private",
            "中文会话事件",
            "literal \"quote\" OR token",
            "another TOKEN",
        ];
        assert_eq!(
            index.matching_positions(&texts, "TOKEN", 1).unwrap(),
            vec![2]
        );
        assert_eq!(
            index.matching_positions(&texts, "文会", 10).unwrap(),
            vec![1]
        );
        assert_eq!(
            index.matching_positions(&texts, "中文会话", 10).unwrap(),
            vec![1]
        );
        assert_eq!(
            index
                .matching_positions(&texts, "\"quote\" OR", 10)
                .unwrap(),
            vec![2]
        );
        assert!(
            index
                .matching_positions(&texts, "nonexistent", 10)
                .unwrap()
                .is_empty()
        );
        assert!(index.matching_positions(&texts, "token", 1001).is_err());
    }
}

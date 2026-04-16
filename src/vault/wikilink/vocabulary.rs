// @Ref: SUMMARY §3 Step 6 — vocabulary relation learning.
//
// For every pair of kept keywords, increment `co_pairs.count`. For
// gatekeeper-confirmed synonym pairs, upsert `vocabulary_relations`
// with `relation_type='synonym'` and bump `source_count`/`confidence`.

use anyhow::Result;
use rusqlite::{params, Connection};

/// Learn from one document's kept keywords + synonym pairs.
///
/// Co-pair ordering is alphabetised so `(A,B)` and `(B,A)` converge.
pub fn learn(
    conn: &mut Connection,
    keywords: &[String],
    synonym_pairs: &[(String, String)],
    domain: &str,
) -> Result<()> {
    let tx = conn.transaction()?;

    // Co-pairs: upsert count++
    for i in 0..keywords.len() {
        for j in (i + 1)..keywords.len() {
            let (a, b) = sorted_pair(&keywords[i], &keywords[j]);
            tx.execute(
                "INSERT INTO co_pairs (word_a, word_b, count) VALUES (?1, ?2, 1)
                 ON CONFLICT(word_a, word_b) DO UPDATE SET count = count + 1",
                params![a, b],
            )?;
        }
    }

    // Synonym relations.
    let now = unix_epoch();
    for (rep, alias) in synonym_pairs {
        let (a, b) = sorted_pair(rep, alias);
        tx.execute(
            "INSERT INTO vocabulary_relations
                (word_a, word_b, relation_type, representative, confidence, source_count,
                 domain, created_at, updated_at)
             VALUES (?1, ?2, 'synonym', ?3, 0.7, 1, ?4, ?5, ?5)
             ON CONFLICT(word_a, word_b, relation_type) DO UPDATE SET
                source_count = source_count + 1,
                confidence = MIN(1.0, confidence + 0.05),
                representative = excluded.representative,
                updated_at = excluded.updated_at",
            params![a, b, rep, domain, now as i64],
        )?;
    }

    tx.commit()?;
    Ok(())
}

fn sorted_pair<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn unix_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::schema::init_schema;

    fn mem_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_schema(&c).unwrap();
        c
    }

    #[test]
    fn co_pairs_count_increments_on_repeat() {
        let mut conn = mem_conn();
        let kws = vec!["A".into(), "B".into()];
        learn(&mut conn, &kws, &[], "legal").unwrap();
        learn(&mut conn, &kws, &[], "legal").unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count FROM co_pairs WHERE word_a = 'A' AND word_b = 'B'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn synonym_pair_creates_relation_with_confidence() {
        let mut conn = mem_conn();
        let pairs = vec![("민법 제750조".into(), "750조".into())];
        learn(&mut conn, &[], &pairs, "legal").unwrap();
        let (rep, conf): (String, f64) = conn
            .query_row(
                "SELECT representative, confidence FROM vocabulary_relations
                 WHERE relation_type = 'synonym'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rep, "민법 제750조");
        assert!(conf >= 0.7);
    }

    #[test]
    fn repeat_synonym_bumps_confidence_up_to_one() {
        let mut conn = mem_conn();
        let pairs = vec![("A".to_string(), "B".to_string())];
        for _ in 0..10 {
            learn(&mut conn, &[], &pairs, "legal").unwrap();
        }
        let conf: f64 = conn
            .query_row(
                "SELECT confidence FROM vocabulary_relations LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(conf <= 1.0);
        assert!(conf > 0.7);
    }
}

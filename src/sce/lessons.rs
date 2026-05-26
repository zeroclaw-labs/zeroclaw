use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lesson {
    pub id: String,
    pub pattern: String,
    pub insight: String,
    pub confidence: f64,
    pub source_tick: u64,
    pub learned_at: DateTime<Utc>,
    pub applied_count: u64,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LessonsLog {
    lessons: Vec<Lesson>,
    next_id: u64,
}

impl LessonsLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(
        &mut self,
        pattern: String,
        insight: String,
        confidence: f64,
        source_tick: u64,
        tags: Vec<String>,
    ) -> &Lesson {
        self.next_id += 1;
        self.lessons.push(Lesson {
            id: format!("lesson_{}", self.next_id),
            pattern,
            insight,
            confidence: confidence.clamp(0.0, 1.0),
            source_tick,
            learned_at: Utc::now(),
            applied_count: 0,
            tags,
        });
        self.lessons.last().unwrap()
    }

    pub fn mark_applied(&mut self, id: &str) -> bool {
        if let Some(lesson) = self.lessons.iter_mut().find(|l| l.id == id) {
            lesson.applied_count += 1;
            true
        } else {
            false
        }
    }

    pub fn search(&self, query: &str) -> Vec<&Lesson> {
        let query_lower = query.to_lowercase();
        self.lessons
            .iter()
            .filter(|l| {
                l.pattern.to_lowercase().contains(&query_lower)
                    || l.insight.to_lowercase().contains(&query_lower)
                    || l.tags
                        .iter()
                        .any(|t| t.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    pub fn by_tag(&self, tag: &str) -> Vec<&Lesson> {
        self.lessons
            .iter()
            .filter(|l| l.tags.iter().any(|t| t == tag))
            .collect()
    }

    pub fn recent(&self, count: usize) -> &[Lesson] {
        let start = self.lessons.len().saturating_sub(count);
        &self.lessons[start..]
    }

    pub fn all(&self) -> &[Lesson] {
        &self.lessons
    }

    pub fn len(&self) -> usize {
        self.lessons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lessons.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_search() {
        let mut log = LessonsLog::new();
        log.record(
            "retry pattern".into(),
            "retrying 3x avoids transient failures".into(),
            0.8,
            42,
            vec!["resilience".into()],
        );
        assert_eq!(log.len(), 1);
        let results = log.search("retry");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn mark_applied_increments() {
        let mut log = LessonsLog::new();
        log.record("p".into(), "i".into(), 0.5, 1, vec![]);
        let id = log.all()[0].id.clone();
        assert!(log.mark_applied(&id));
        assert_eq!(log.all()[0].applied_count, 1);
    }

    #[test]
    fn by_tag_filters_correctly() {
        let mut log = LessonsLog::new();
        log.record("a".into(), "b".into(), 0.5, 1, vec!["perf".into()]);
        log.record("c".into(), "d".into(), 0.5, 2, vec!["safety".into()]);
        assert_eq!(log.by_tag("perf").len(), 1);
    }
}

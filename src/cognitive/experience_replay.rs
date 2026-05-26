use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayEntry {
    pub episode: String,
    pub priority: f64,
    pub replayed_count: u32,
    pub reward: f64,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExperienceReplay {
    pub buffer: Vec<ReplayEntry>,
    pub capacity: usize,
}

impl ExperienceReplay {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: Vec::new(),
            capacity,
        }
    }

    pub fn add(&mut self, episode: &str, reward: f64) {
        let priority = reward.abs();
        if self.buffer.len() >= self.capacity {
            if let Some(min_idx) = self
                .buffer
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| a.priority.partial_cmp(&b.priority).unwrap())
                .map(|(i, _)| i)
            {
                if priority > self.buffer[min_idx].priority {
                    self.buffer.swap_remove(min_idx);
                } else {
                    return;
                }
            }
        }
        self.buffer.push(ReplayEntry {
            episode: episode.to_string(),
            priority,
            replayed_count: 0,
            reward,
            source_id: String::new(),
            timestamp: 0,
            confidence: priority.clamp(0.0, 1.0),
        });
    }

    pub fn sample_prioritized(&mut self, n: usize) -> Vec<&ReplayEntry> {
        self.buffer
            .sort_by(|a, b| b.priority.partial_cmp(&a.priority).unwrap());
        let count = n.min(self.buffer.len());
        for entry in self.buffer.iter_mut().take(count) {
            entry.replayed_count += 1;
        }
        self.buffer.iter().take(count).collect()
    }

    pub fn update_priority(&mut self, episode: &str, new_priority: f64) {
        if let Some(entry) = self.buffer.iter_mut().find(|e| e.episode == episode) {
            entry.priority = new_priority;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prioritized_sampling() {
        let mut er = ExperienceReplay::new(100);
        er.add("low", 0.1);
        er.add("high", 0.9);
        er.add("mid", 0.5);
        let samples = er.sample_prioritized(2);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].episode, "high");
        assert_eq!(samples[1].episode, "mid");
    }

    #[test]
    fn capacity_evicts_lowest() {
        let mut er = ExperienceReplay::new(2);
        er.add("a", 0.5);
        er.add("b", 0.3);
        er.add("c", 0.9);
        assert_eq!(er.buffer.len(), 2);
        assert!(er.buffer.iter().any(|e| e.episode == "c"));
        assert!(er.buffer.iter().any(|e| e.episode == "a"));
    }
}

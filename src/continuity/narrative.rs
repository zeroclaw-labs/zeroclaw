use super::types::Episode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NarrativeStore {
    episodes: Vec<Episode>,
    max_episodes: usize,
}

impl NarrativeStore {
    pub fn new(max_episodes: usize) -> Self {
        Self {
            episodes: Vec::new(),
            max_episodes,
        }
    }

    pub fn append(&mut self, episode: Episode) {
        if let Some(existing) = self.find_similar(&episode) {
            let idx = existing;
            self.episodes[idx].significance =
                f64::midpoint(self.episodes[idx].significance, episode.significance);
            self.episodes[idx].verified = self.episodes[idx].verified || episode.verified;
            for tag in &episode.tags {
                if !self.episodes[idx].tags.contains(tag) {
                    self.episodes[idx].tags.push(tag.clone());
                }
            }
            if episode.emotional_tag.is_some() {
                self.episodes[idx].emotional_tag = episode.emotional_tag.clone();
            }
            if let (Some(existing_v), Some(new_v)) =
                (self.episodes[idx].valence_score, episode.valence_score)
            {
                self.episodes[idx].valence_score = Some(f64::midpoint(existing_v, new_v));
            } else if episode.valence_score.is_some() {
                self.episodes[idx].valence_score = episode.valence_score;
            }
            return;
        }

        self.episodes.push(episode);
        self.compress_if_needed();
    }

    pub fn set_max_episodes(&mut self, max: usize) {
        self.max_episodes = max;
    }

    pub fn max_episodes(&self) -> usize {
        self.max_episodes
    }

    pub fn episodes(&self) -> &[Episode] {
        &self.episodes
    }

    pub fn verified_episodes(&self) -> Vec<&Episode> {
        self.episodes.iter().filter(|e| e.verified).collect()
    }

    pub fn verify_recent(&mut self, tool_names: &[String]) {
        if tool_names.is_empty() {
            return;
        }
        if let Some(episode) = self.episodes.iter_mut().rev().find(|e| !e.verified) {
            episode.verified = true;
            for name in tool_names {
                let tag = format!("tool:{}", name);
                if !episode.tags.contains(&tag) {
                    episode.tags.push(tag);
                }
            }
        }
    }

    pub fn episodes_by_emotion(&self, tag: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|e| e.emotional_tag.as_deref() == Some(tag))
            .collect()
    }

    pub fn high_valence_episodes(&self, threshold: f64) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|e| e.valence_score.map_or(false, |v| v.abs() >= threshold))
            .collect()
    }

    fn find_similar(&self, episode: &Episode) -> Option<usize> {
        self.episodes
            .iter()
            .position(|e| e.summary == episode.summary)
    }

    fn compress_if_needed(&mut self) {
        if self.episodes.len() <= self.max_episodes {
            return;
        }
        self.episodes.sort_by(|a, b| {
            b.significance
                .partial_cmp(&a.significance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.episodes.truncate(self.max_episodes);
        self.episodes.sort_by_key(|e| e.timestamp);
    }
}

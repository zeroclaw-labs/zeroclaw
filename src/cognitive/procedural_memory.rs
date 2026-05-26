use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub trigger: String,
    pub steps: Vec<String>,
    pub frequency: u64,
    pub last_used: u64,
    pub source_id: String,
    pub timestamp: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProceduralMemory {
    pub routines: Vec<Routine>,
}

impl ProceduralMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, name: &str, trigger: &str, steps: Vec<String>) {
        self.routines.push(Routine {
            name: name.to_string(),
            trigger: trigger.to_string(),
            steps,
            frequency: 0,
            last_used: 0,
            source_id: String::new(),
            timestamp: 0,
            confidence: 1.0,
        });
    }

    pub fn match_trigger(&self, input: &str) -> Vec<&Routine> {
        let lower = input.to_lowercase();
        self.routines
            .iter()
            .filter(|r| lower.contains(&r.trigger.to_lowercase()))
            .collect()
    }

    pub fn record_use(&mut self, name: &str, tick: u64) {
        if let Some(r) = self.routines.iter_mut().find(|r| r.name == name) {
            r.frequency += 1;
            r.last_used = tick;
        }
    }

    pub fn most_frequent(&self) -> Option<&Routine> {
        self.routines.iter().max_by_key(|r| r.frequency)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_trigger_and_frequency() {
        let mut pm = ProceduralMemory::new();
        pm.add("greet", "hello", vec!["wave".into(), "smile".into()]);
        let matches = pm.match_trigger("Hello there!");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "greet");
        pm.record_use("greet", 5);
        assert_eq!(pm.routines[0].frequency, 1);
        assert_eq!(pm.routines[0].last_used, 5);
    }

    #[test]
    fn most_frequent_returns_top() {
        let mut pm = ProceduralMemory::new();
        pm.add("a", "x", vec![]);
        pm.add("b", "y", vec![]);
        pm.record_use("b", 1);
        pm.record_use("b", 2);
        pm.record_use("a", 3);
        assert_eq!(pm.most_frequent().unwrap().name, "b");
    }
}

use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

const STATE_FILE: &str = ".reviewed_trades.json";

pub struct ReviewState {
    reviewed_ids: HashSet<String>,
}

impl ReviewState {
    pub fn load() -> Result<Self> {
        let path = Path::new(STATE_FILE);
        let reviewed_ids = if path.exists() {
            let data = std::fs::read_to_string(path)?;
            serde_json::from_str::<Vec<String>>(&data)
                .unwrap_or_default()
                .into_iter()
                .collect()
        } else {
            HashSet::new()
        };
        Ok(Self { reviewed_ids })
    }

    pub fn is_reviewed(&self, transaction_id: &str) -> bool {
        self.reviewed_ids.contains(transaction_id)
    }

    pub fn mark_reviewed(&mut self, transaction_id: &str) -> Result<()> {
        self.reviewed_ids.insert(transaction_id.to_string());
        self.save()
    }

    fn save(&self) -> Result<()> {
        let ids: Vec<&String> = self.reviewed_ids.iter().collect();
        let json = serde_json::to_string_pretty(&ids)?;
        std::fs::write(STATE_FILE, json)?;
        Ok(())
    }
}

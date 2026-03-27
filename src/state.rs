use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

const STATE_FILE: &str = ".reviewed_trades.json";
const CHAT_STATE_FILE: &str = ".chat_state.json";
const MAX_RECENT_EXCHANGES: usize = 3;

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

/// Tracks which chat messages the bot has already responded to,
/// and maintains a sliding window of recent exchanges per user for follow-up context.
pub struct ChatState {
    responded_ids: HashSet<String>,
    /// user_id → last N (question, answer) pairs for conversation continuity.
    recent_exchanges: HashMap<String, VecDeque<(String, String)>>,
}

impl ChatState {
    pub fn load() -> Result<Self> {
        let path = Path::new(CHAT_STATE_FILE);
        let responded_ids = if path.exists() {
            let data = std::fs::read_to_string(path)?;
            serde_json::from_str::<Vec<String>>(&data)
                .unwrap_or_default()
                .into_iter()
                .collect()
        } else {
            HashSet::new()
        };
        Ok(Self {
            responded_ids,
            recent_exchanges: HashMap::new(),
        })
    }

    pub fn has_responded(&self, message_id: &str) -> bool {
        self.responded_ids.contains(message_id)
    }

    pub fn mark_responded(&mut self, message_id: &str) -> Result<()> {
        self.responded_ids.insert(message_id.to_string());
        self.save()
    }

    /// Record a (question, answer) exchange for a user. Keeps the last N exchanges.
    pub fn add_exchange(&mut self, user_id: &str, question: String, answer: String) {
        let exchanges = self
            .recent_exchanges
            .entry(user_id.to_string())
            .or_default();
        if exchanges.len() >= MAX_RECENT_EXCHANGES {
            exchanges.pop_front();
        }
        exchanges.push_back((question, answer));
    }

    /// Get the recent exchanges for a user (oldest first).
    pub fn get_exchanges(&self, user_id: &str) -> Vec<(&str, &str)> {
        self.recent_exchanges
            .get(user_id)
            .map(|exs| exs.iter().map(|(q, a)| (q.as_str(), a.as_str())).collect())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<()> {
        let ids: Vec<&String> = self.responded_ids.iter().collect();
        let json = serde_json::to_string_pretty(&ids)?;
        std::fs::write(CHAT_STATE_FILE, json)?;
        Ok(())
    }
}

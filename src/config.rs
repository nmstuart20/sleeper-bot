use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub league: LeagueConfig,
}

#[derive(Debug, Deserialize)]
pub struct LeagueConfig {
    pub rules: String,
    #[serde(default = "default_scoring")]
    pub scoring: String,
    #[serde(default = "default_bot_username")]
    pub bot_username: String,
}

fn default_scoring() -> String {
    "half_ppr".to_string()
}

fn default_bot_username() -> String {
    "tradegimp210".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}

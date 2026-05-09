use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub league: LeagueConfig,
}

#[derive(Debug, Deserialize)]
pub struct LeagueConfig {
    /// Optional free-form notes about the league (payouts, custom rules,
    /// etc.) appended to the API-derived format summary. The lineup and
    /// scoring format are auto-detected from the Sleeper API, so this only
    /// needs to capture things the API doesn't expose.
    #[serde(default)]
    pub rules: Option<String>,
    /// Optional override for the scoring format used when looking up
    /// projections. When unset, the bot derives this from the league's
    /// `scoring_settings.rec` value (0 = std, 0.5 = half_ppr, 1 = ppr).
    #[serde(default)]
    pub scoring: Option<String>,
    #[serde(default = "default_bot_username")]
    pub bot_username: String,
}

fn default_bot_username() -> String {
    "tradebot123".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}

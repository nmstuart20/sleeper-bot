use std::collections::HashMap;

use crate::sleeper::{League, NflState, Roster, User};

/// Check if a message text mentions the bot's username.
pub fn is_mention(text: &str, bot_username: &str) -> bool {
    let lower = text.to_lowercase();
    let bot_lower = bot_username.to_lowercase();
    lower.contains(&format!("@{bot_lower}")) || lower.contains(&bot_lower)
}

/// Strip the bot mention from the message to get the actual question.
pub fn strip_mention(text: &str, bot_username: &str) -> String {
    let result = text
        .replace(&format!("@{bot_username}"), "")
        .replace(bot_username, "");
    result.trim().to_string()
}

/// Build a short baseline context string (under 500 tokens) so the LLM can make
/// intelligent first tool calls without needing the full league data dump.
pub fn build_lightweight_context(
    league: &League,
    users: &[User],
    rosters: &[Roster],
    nfl_state: &NflState,
    scoring: &str,
) -> String {
    let league_name = league.name.as_deref().unwrap_or("Unknown League");
    let num_teams = league.total_rosters.unwrap_or(0);

    let league_type = league
        .settings
        .as_ref()
        .map(|s| match s.league_type {
            Some(0) => "Redraft",
            Some(1) => "Keeper",
            Some(2) => "Dynasty",
            _ => "Unknown",
        })
        .unwrap_or("Unknown");

    let scoring_fmt = match scoring {
        "ppr" => "Full PPR",
        "std" => "Standard (non-PPR)",
        _ => "Half PPR",
    };

    let user_map: HashMap<&str, &User> = users.iter().map(|u| (u.user_id.as_str(), u)).collect();

    // Build compact standings: "1. Nick 8-2, 2. Mike 6-4, ..."
    let mut entries: Vec<(String, u32, f64)> = Vec::new();
    for roster in rosters {
        let owner_id = match roster.owner_id.as_deref() {
            Some(id) => id,
            None => continue,
        };
        let display_name = user_map
            .get(owner_id)
            .and_then(|u| u.display_name.as_deref())
            .unwrap_or("Unknown");
        let settings = roster.settings.as_ref();
        let wins = settings.and_then(|s| s.wins).unwrap_or(0);
        let record = settings
            .map(|s| s.record())
            .unwrap_or_else(|| "0-0".to_string());
        let points = settings.map(|s| s.total_points()).unwrap_or(0.0);
        entries.push((format!("{display_name} {record}"), wins, points));
    }
    entries.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
    });
    let standings_line: Vec<String> = entries
        .iter()
        .enumerate()
        .map(|(i, (s, _, _))| format!("{}. {}", i + 1, s))
        .collect();

    format!(
        "League: {league_name} ({league_type}, {num_teams} teams, {scoring_fmt})\n\
         NFL {season} — Week {week}\n\
         Standings: {standings}",
        season = nfl_state.season,
        week = nfl_state.week,
        standings = standings_line.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_mention() {
        let bot = "tradegimp210";
        assert!(is_mention("hey @tradegimp210 who should I start?", bot));
        assert!(is_mention("@tradegimp210", bot));
        assert!(is_mention("yo tradegimp210 what do you think?", bot));
        assert!(!is_mention("hey guys what's up", bot));
    }

    #[test]
    fn test_strip_mention() {
        let bot = "tradegimp210";
        assert_eq!(
            strip_mention("@tradegimp210 who should I start at flex?", bot),
            "who should I start at flex?"
        );
        assert_eq!(
            strip_mention("hey @tradegimp210 thoughts on this trade?", bot),
            "hey  thoughts on this trade?"
        );
    }
}

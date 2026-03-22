use crate::sleeper::{self, Player, Transaction};
use std::collections::HashMap;

pub struct TradeSummary {
    pub team_a_name: String,
    pub team_a_record: String,
    pub team_a_receives: Vec<String>,
    pub team_b_name: String,
    pub team_b_record: String,
    pub team_b_receives: Vec<String>,
}

/// Parse a completed trade transaction into a human-readable TradeSummary.
/// Supports 2+ team trades by grouping adds and picks by receiving roster_id.
pub fn parse_trade(
    tx: &Transaction,
    roster_names: &HashMap<u32, String>,
    roster_records: &HashMap<u32, String>,
    players: &HashMap<String, Player>,
) -> Option<TradeSummary> {
    let roster_ids = tx.roster_ids.as_ref()?;
    if roster_ids.len() < 2 {
        return None;
    }

    // Build per-roster receive lists
    let mut receives: HashMap<u32, Vec<String>> = HashMap::new();
    for &rid in roster_ids {
        receives.entry(rid).or_default();
    }

    // Group player adds by receiving roster
    if let Some(ref adds) = tx.adds {
        for (player_id, &receiving_roster) in adds {
            let name = sleeper::format_player_name(player_id, players);
            receives.entry(receiving_roster).or_default().push(name);
        }
    }

    // Group draft picks by new owner
    if let Some(ref picks) = tx.draft_picks {
        for pick in picks {
            let owner = pick.owner_id.unwrap_or(0);
            let season = pick.season.as_deref().unwrap_or("????");
            let round = pick.round.unwrap_or(0);
            let original_roster = pick.roster_id.unwrap_or(0);

            let pick_str = if original_roster != owner {
                let orig_name = roster_names
                    .get(&original_roster)
                    .cloned()
                    .unwrap_or_else(|| format!("Team {original_roster}"));
                format!("{season} Round {round} Pick (originally {orig_name}'s)")
            } else {
                format!("{season} Round {round} Pick")
            };

            receives.entry(owner).or_default().push(pick_str);
        }
    }

    // For the standard 2-team trade, map to team_a / team_b
    let team_a_id = roster_ids[0];
    let team_b_id = roster_ids[1];

    let get_name = |rid: u32| {
        roster_names
            .get(&rid)
            .cloned()
            .unwrap_or_else(|| format!("Team {rid}"))
    };
    let get_record = |rid: u32| {
        roster_records
            .get(&rid)
            .cloned()
            .unwrap_or_else(|| "0-0".to_string())
    };

    Some(TradeSummary {
        team_a_name: get_name(team_a_id),
        team_a_record: get_record(team_a_id),
        team_a_receives: receives.remove(&team_a_id).unwrap_or_default(),
        team_b_name: get_name(team_b_id),
        team_b_record: get_record(team_b_id),
        team_b_receives: receives.remove(&team_b_id).unwrap_or_default(),
    })
}

/// Build the user message for the LLM prompt from a TradeSummary.
pub fn build_prompt(summary: &TradeSummary) -> String {
    let mut msg = String::from("Analyze this fantasy football trade:\n\nTRADE:\n");

    msg.push_str(&format!(
        "{} (Record: {}) receives:\n",
        summary.team_a_name, summary.team_a_record
    ));
    if summary.team_a_receives.is_empty() {
        msg.push_str("  - (nothing)\n");
    } else {
        for item in &summary.team_a_receives {
            msg.push_str(&format!("  - {item}\n"));
        }
    }

    msg.push('\n');
    msg.push_str(&format!(
        "{} (Record: {}) receives:\n",
        summary.team_b_name, summary.team_b_record
    ));
    if summary.team_b_receives.is_empty() {
        msg.push_str("  - (nothing)\n");
    } else {
        for item in &summary.team_b_receives {
            msg.push_str(&format!("  - {item}\n"));
        }
    }

    msg.push_str("\nWho won this trade and why?");
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sleeper::{DraftPick, Player};

    fn sample_players() -> HashMap<String, Player> {
        let mut m = HashMap::new();
        m.insert(
            "4035".to_string(),
            Player {
                player_id: Some("4035".to_string()),
                first_name: Some("Jaylen".to_string()),
                last_name: Some("Waddle".to_string()),
                position: Some("WR".to_string()),
                team: Some("MIA".to_string()),
            },
        );
        m.insert(
            "2257".to_string(),
            Player {
                player_id: Some("2257".to_string()),
                first_name: Some("Derrick".to_string()),
                last_name: Some("Henry".to_string()),
                position: Some("RB".to_string()),
                team: Some("TEN".to_string()),
            },
        );
        m
    }

    fn sample_names() -> HashMap<u32, String> {
        let mut m = HashMap::new();
        m.insert(1, "Nick (Touchdown Tyrants)".to_string());
        m.insert(2, "Mike (Dumpster Fire)".to_string());
        m
    }

    fn sample_records() -> HashMap<u32, String> {
        let mut m = HashMap::new();
        m.insert(1, "8-2".to_string());
        m.insert(2, "3-7".to_string());
        m
    }

    #[test]
    fn test_parse_trade_with_players_and_picks() {
        let mut adds = HashMap::new();
        adds.insert("4035".to_string(), 1u32); // Waddle → roster 1
        adds.insert("2257".to_string(), 2u32); // Henry → roster 2

        let tx = Transaction {
            tx_type: Some("trade".to_string()),
            transaction_id: Some("123".to_string()),
            status: Some("complete".to_string()),
            roster_ids: Some(vec![2, 1]),
            adds: Some(adds),
            drops: None,
            draft_picks: Some(vec![DraftPick {
                season: Some("2025".to_string()),
                round: Some(3),
                roster_id: Some(2),
                previous_owner_id: Some(2),
                owner_id: Some(1),
            }]),
            created: Some(1558039391576),
            status_updated: None,
        };

        let summary =
            parse_trade(&tx, &sample_names(), &sample_records(), &sample_players()).unwrap();

        // Team A is roster 2 (first in roster_ids)
        assert_eq!(summary.team_a_name, "Mike (Dumpster Fire)");
        assert_eq!(summary.team_a_record, "3-7");
        assert!(summary
            .team_a_receives
            .iter()
            .any(|s| s.contains("Derrick Henry")));

        assert_eq!(summary.team_b_name, "Nick (Touchdown Tyrants)");
        assert!(summary
            .team_b_receives
            .iter()
            .any(|s| s.contains("Jaylen Waddle")));
        assert!(summary
            .team_b_receives
            .iter()
            .any(|s| s.contains("Round 3")));
    }

    #[test]
    fn test_build_prompt_format() {
        let summary = TradeSummary {
            team_a_name: "Alice".to_string(),
            team_a_record: "5-5".to_string(),
            team_a_receives: vec!["Player A (WR - NYG)".to_string()],
            team_b_name: "Bob".to_string(),
            team_b_record: "7-3".to_string(),
            team_b_receives: vec![
                "Player B (RB - DAL)".to_string(),
                "2025 Round 1 Pick".to_string(),
            ],
        };

        let prompt = build_prompt(&summary);
        assert!(prompt.contains("Alice (Record: 5-5) receives:"));
        assert!(prompt.contains("Bob (Record: 7-3) receives:"));
        assert!(prompt.contains("Player A (WR - NYG)"));
        assert!(prompt.contains("2025 Round 1 Pick"));
        assert!(prompt.contains("Who won this trade and why?"));
    }
}

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use crate::graphql::SleeperGraphql;
use crate::sleeper;
use crate::sleeper::{
    AllTimeUserStats, NflState, Player, PlayerSeasonEntry, PlayerStats, Roster, SeasonChampion,
    SleeperClient, Transaction, User,
};

fn normalize_name(name: &str) -> String {
    let normalized: String = name
        .chars()
        .filter(|c| !matches!(c, '.' | '-' | '\'' | '’'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_active_nfl_player(player: &Player) -> bool {
    matches!(player.team.as_deref(), Some(team) if !team.trim().is_empty())
}

fn format_player_match(player: &Player) -> String {
    let pos = player.position.as_deref().unwrap_or("??");
    let team = player.team.as_deref().unwrap_or("FA");
    format!("{} ({}, {})", player.full_name(), pos, team)
}

/// Represents each tool the LLM can call during an agentic conversation.
#[derive(Debug, Clone)]
pub enum ToolName {
    /// Returns current league standings (team name, record, points for/against).
    GetLeagueStandings,
    /// Returns full roster (starters + bench) for a specific team, matched fuzzily.
    GetTeamRoster {
        team_name: String,
        position: Option<String>,
    },
    /// Returns detailed info for a named player: position, team, age, injury,
    /// depth chart, historical stats, and current projections.
    GetPlayerInfo { player_name: String },
    /// Returns top unrostered players sorted by projected points,
    /// optionally filtered by position (QB/RB/WR/TE/K/DEF).
    SearchWaiverWire {
        position: Option<String>,
        limit: Option<u32>,
    },
    /// Returns recent transactions, optionally filtered by type (trade/waiver/free_agent).
    GetRecentTransactions {
        tx_type: Option<String>,
        limit: Option<u32>,
    },
    /// Returns current week matchup for a team with opponent and scores if available.
    GetMatchup { team_name: String },
    /// Returns past season champions and all-time stats summary.
    GetLeagueHistory,
    /// Returns detailed results for a past season: final standings, playoff bracket,
    /// and champion. Walks the previous_league_id chain to find the target season.
    GetPastSeasonResults { seasons_ago: u32 },
    /// Searches league chat message history with optional filters for username, keyword, and date range.
    SearchLeagueMessages {
        username: Option<String>,
        keyword: Option<String>,
        after_date: Option<String>,
        before_date: Option<String>,
        limit: Option<u32>,
    },
}

/// Returns the Anthropic API tool JSON schema for each tool.
fn tool_definition(tool: &str) -> Value {
    match tool {
        "get_league_standings" => json!({
            "name": "get_league_standings",
            "description": "Get current league standings including each team's record (wins-losses), total points scored (PF), and total points scored against (PA). Use this to answer questions about who's leading the league, playoff positioning, team comparisons, or any standings-related question.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        "get_team_roster" => json!({
            "name": "get_team_roster",
            "description": "Get the full roster for a specific team, including starters and bench players with their positions. The team is matched fuzzily by display name or team name (case-insensitive). Use this when asked about a specific team's players, lineup, roster composition, or positional depth.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "team_name": {
                        "type": "string",
                        "description": "The team or owner name to look up (fuzzy matched against display names and team names)"
                    },
                    "position": {
                        "type": "string",
                        "description": "Optional: filter roster to only show players at this position (QB, RB, WR, TE, K, DEF). Useful for start/sit decisions or positional depth questions.",
                        "enum": ["QB", "RB", "WR", "TE", "K", "DEF"]
                    }
                },
                "required": ["team_name"]
            }
        }),
        "get_player_info" => json!({
            "name": "get_player_info",
            "description": "Get detailed information about a specific NFL player including: position, NFL team, age, injury status, depth chart position, historical season stats, and current season projections. Use this when asked about a specific player's value, performance, injury, or outlook.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "player_name": {
                        "type": "string",
                        "description": "The player's name to look up (fuzzy matched against full player names)"
                    }
                },
                "required": ["player_name"]
            }
        }),
        "search_waiver_wire" => json!({
            "name": "search_waiver_wire",
            "description": "Search for the best available unrostered players on the waiver wire, sorted by projected fantasy points. Optionally filter by position and limit the number of results. Use this when asked about waiver wire pickups, free agents, or available players at a position.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "position": {
                        "type": "string",
                        "description": "Optional position filter: QB, RB, WR, TE, K, or DEF",
                        "enum": ["QB", "RB", "WR", "TE", "K", "DEF"]
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of players to return (default: 15)",
                        "default": 15
                    }
                },
                "required": []
            }
        }),
        "get_recent_transactions" => json!({
            "name": "get_recent_transactions",
            "description": "Get recent league transactions including trades, waiver claims, and free agent pickups. Optionally filter by transaction type and limit results. Use this when asked about recent moves, trades, waiver activity, or who picked up/dropped a player.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "tx_type": {
                        "type": "string",
                        "description": "Optional filter by transaction type",
                        "enum": ["trade", "waiver", "free_agent"]
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of transactions to return (default: 10)",
                        "default": 10
                    }
                },
                "required": []
            }
        }),
        "get_matchup" => json!({
            "name": "get_matchup",
            "description": "Get the current week's matchup for a specific team, including the opponent and scores if available. Use this when asked about who a team is playing this week, current matchup scores, or head-to-head details.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "team_name": {
                        "type": "string",
                        "description": "The team or owner name to look up (fuzzy matched against display names and team names)"
                    }
                },
                "required": ["team_name"]
            }
        }),
        "get_league_history" => json!({
            "name": "get_league_history",
            "description": "Get the league's historical data including past season champions and all-time user statistics (total wins, losses, points, championships). Use this when asked about league history, past champions, dynasty records, or all-time rankings.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        "get_past_season_results" => json!({
            "name": "get_past_season_results",
            "description": "Get detailed results for a specific past season including final standings (records and points), the full playoff bracket with winners/losers of each round, and the champion. Use seasons_ago=1 for last season, seasons_ago=2 for two seasons ago, etc. Use this when asked about a specific past season's results, final standings, who won the championship that year, or playoff matchup details.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "seasons_ago": {
                        "type": "integer",
                        "description": "How many seasons back to look (1 = last season, 2 = two seasons ago, etc.)",
                        "minimum": 1
                    }
                },
                "required": ["seasons_ago"]
            }
        }),
        "search_league_messages" => json!({
            "name": "search_league_messages",
            "description": "Search through the league's chat message history. Use this when someone asks about what a league member said in the past, such as bets, predictions, claims, trash talk, or any prior statements. You can filter by username, keyword/phrase, and/or date range. Returns up to 1000 messages worth of history.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "username": {
                        "type": "string",
                        "description": "Optional: filter messages by author display name (case-insensitive, partial match)"
                    },
                    "keyword": {
                        "type": "string",
                        "description": "Optional: filter messages containing this keyword or phrase (case-insensitive)"
                    },
                    "after_date": {
                        "type": "string",
                        "description": "Optional: only include messages sent after this date (format: YYYY-MM-DD)"
                    },
                    "before_date": {
                        "type": "string",
                        "description": "Optional: only include messages sent before this date (format: YYYY-MM-DD)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matching messages to return (default: 15)",
                        "default": 15
                    }
                },
                "required": []
            }
        }),
        _ => json!({}),
    }
}

/// Returns all tool definitions as a Vec suitable for the Anthropic API `tools` parameter.
pub fn all_tool_definitions() -> Vec<Value> {
    vec![
        tool_definition("get_league_standings"),
        tool_definition("get_team_roster"),
        tool_definition("get_player_info"),
        tool_definition("search_waiver_wire"),
        tool_definition("get_recent_transactions"),
        tool_definition("get_matchup"),
        tool_definition("get_league_history"),
        tool_definition("get_past_season_results"),
        tool_definition("search_league_messages"),
    ]
}

/// Convert an Anthropic-format tool definition to a Gemini `functionDeclaration`.
fn to_gemini_function_declaration(anthropic_tool: &Value) -> Value {
    let mut decl = json!({
        "name": anthropic_tool["name"],
        "description": anthropic_tool["description"],
    });
    // Gemini uses "parameters" instead of "input_schema"
    if let Some(schema) = anthropic_tool.get("input_schema") {
        decl["parameters"] = schema.clone();
    }
    decl
}

/// Returns tool definitions in Gemini's format (array of `functionDeclarations`).
pub fn all_gemini_tool_definitions() -> Vec<Value> {
    all_tool_definitions()
        .iter()
        .map(to_gemini_function_declaration)
        .collect()
}

/// Parse an LLM tool call (name + JSON input) into a ToolName variant.
pub fn parse_tool_call(name: &str, input: &Value) -> Result<ToolName> {
    match name {
        "get_league_standings" => Ok(ToolName::GetLeagueStandings),

        "get_team_roster" => {
            let team_name = input
                .get("team_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("get_team_roster requires 'team_name' (string)"))?;
            let position = input
                .get("position")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            Ok(ToolName::GetTeamRoster {
                team_name: team_name.to_string(),
                position,
            })
        }

        "get_player_info" => {
            let player_name = input
                .get("player_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("get_player_info requires 'player_name' (string)"))?;
            Ok(ToolName::GetPlayerInfo {
                player_name: player_name.to_string(),
            })
        }

        "search_waiver_wire" => {
            let position = input
                .get("position")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            Ok(ToolName::SearchWaiverWire { position, limit })
        }

        "get_recent_transactions" => {
            let tx_type = input
                .get("tx_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let limit = input
                .get("limit")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32);
            Ok(ToolName::GetRecentTransactions { tx_type, limit })
        }

        "get_matchup" => {
            let team_name = input
                .get("team_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("get_matchup requires 'team_name' (string)"))?;
            Ok(ToolName::GetMatchup {
                team_name: team_name.to_string(),
            })
        }

        "get_league_history" => Ok(ToolName::GetLeagueHistory),

        "get_past_season_results" => {
            let seasons_ago = input
                .get("seasons_ago")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    anyhow!("get_past_season_results requires 'seasons_ago' (integer)")
                })? as u32;
            Ok(ToolName::GetPastSeasonResults { seasons_ago })
        }

        "search_league_messages" => {
            let username = input
                .get("username")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let keyword = input
                .get("keyword")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let after_date = input
                .get("after_date")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let before_date = input
                .get("before_date")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let limit = input.get("limit").and_then(|v| v.as_u64()).map(|v| v as u32);
            Ok(ToolName::SearchLeagueMessages {
                username,
                keyword,
                after_date,
                before_date,
                limit,
            })
        }

        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

/// Holds references to all the data needed to execute tool calls.
pub struct ToolExecutor<'a> {
    pub sleeper: &'a SleeperClient,
    pub league_id: &'a str,
    pub players: &'a HashMap<String, Player>,
    pub users: &'a [User],
    pub rosters: &'a [Roster],
    pub roster_names: &'a HashMap<u32, String>,
    pub nfl_state: &'a NflState,
    pub historical_stats: &'a HashMap<String, Vec<PlayerSeasonEntry>>,
    pub projections: &'a HashMap<String, PlayerStats>,
    pub champions: &'a [SeasonChampion],
    pub all_time_stats: &'a [AllTimeUserStats],
    pub scoring: &'a str,
    pub recent_transactions: &'a [Transaction],
    pub gql: Option<&'a SleeperGraphql>,
}

impl<'a> ToolExecutor<'a> {
    /// Execute a tool call and return the formatted result string.
    pub async fn execute(&self, tool: &ToolName) -> Result<String> {
        match tool {
            ToolName::GetLeagueStandings => self.get_league_standings(),
            ToolName::GetTeamRoster { team_name, position } => {
                self.get_team_roster(team_name, position.as_deref())
            }
            ToolName::GetPlayerInfo { player_name } => self.get_player_info(player_name),
            ToolName::SearchWaiverWire { position, limit } => {
                self.search_waiver_wire(position.as_deref(), limit.unwrap_or(15))
            }
            ToolName::GetRecentTransactions { tx_type, limit } => {
                self.get_recent_transactions(tx_type.as_deref(), limit.unwrap_or(10))
            }
            ToolName::GetMatchup { team_name } => self.get_matchup(team_name).await,
            ToolName::GetLeagueHistory => self.get_league_history(),
            ToolName::GetPastSeasonResults { seasons_ago } => {
                self.get_past_season_results(*seasons_ago).await
            }
            ToolName::SearchLeagueMessages {
                username,
                keyword,
                after_date,
                before_date,
                limit,
            } => {
                self.search_league_messages(
                    username.as_deref(),
                    keyword.as_deref(),
                    after_date.as_deref(),
                    before_date.as_deref(),
                    limit.unwrap_or(15),
                )
                .await
            }
        }
    }

    fn get_league_standings(&self) -> Result<String> {
        let user_map: HashMap<&str, &User> =
            self.users.iter().map(|u| (u.user_id.as_str(), u)).collect();

        let mut entries: Vec<(String, u32, f64, f64)> = Vec::new();

        for roster in self.rosters {
            let owner_id = match roster.owner_id.as_deref() {
                Some(id) => id,
                None => continue,
            };
            let user = user_map.get(owner_id);
            let display_name = user
                .and_then(|u| u.display_name.as_deref())
                .unwrap_or("Unknown");
            let team_name = user
                .and_then(|u| u.metadata.as_ref())
                .and_then(|m| m.team_name.as_deref())
                .unwrap_or("");

            let name = if !team_name.is_empty() {
                format!("{display_name} ({team_name})")
            } else {
                display_name.to_string()
            };

            let settings = roster.settings.as_ref();
            let wins = settings.and_then(|s| s.wins).unwrap_or(0);
            let points = settings.map(|s| s.total_points()).unwrap_or(0.0);
            let pts_against = settings.map(|s| s.points_against()).unwrap_or(0.0);

            entries.push((name, wins, points, pts_against));
        }

        entries.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
        });

        let mut result = String::from("League Standings:\n");
        for (i, (name, wins, pf, pa)) in entries.iter().enumerate() {
            let record = self
                .rosters
                .iter()
                .find(|r| {
                    let owner_id = r.owner_id.as_deref().unwrap_or("");
                    let user = user_map.get(owner_id);
                    let dn = user.and_then(|u| u.display_name.as_deref()).unwrap_or("");
                    let tn = user
                        .and_then(|u| u.metadata.as_ref())
                        .and_then(|m| m.team_name.as_deref())
                        .unwrap_or("");
                    let n = if !tn.is_empty() {
                        format!("{dn} ({tn})")
                    } else {
                        dn.to_string()
                    };
                    &n == name
                })
                .and_then(|r| r.settings.as_ref())
                .map(|s| s.record())
                .unwrap_or_else(|| format!("{wins}-0"));

            result.push_str(&format!(
                "{}. {} — {}, {:.1} PF, {:.1} PA\n",
                i + 1,
                name,
                record,
                pf,
                pa
            ));
        }

        Ok(result)
    }

    fn get_team_roster(&self, team_name: &str, position: Option<&str>) -> Result<String> {
        let normalized_query = normalize_name(team_name);
        let user_map: HashMap<&str, &User> =
            self.users.iter().map(|u| (u.user_id.as_str(), u)).collect();

        // Find roster by fuzzy matching display_name or team_name
        let matched_roster = self.rosters.iter().find(|r| {
            let owner_id = match r.owner_id.as_deref() {
                Some(id) => id,
                None => return false,
            };
            let user = match user_map.get(owner_id) {
                Some(u) => u,
                None => return false,
            };
            let display = user.display_name.as_deref().unwrap_or("").to_lowercase();
            let team = user
                .metadata
                .as_ref()
                .and_then(|m| m.team_name.as_deref())
                .unwrap_or("");
            let normalized_display = normalize_name(display.as_str());
            let normalized_team = normalize_name(team);

            (!normalized_query.is_empty() && normalized_display.contains(&normalized_query))
                || (!normalized_query.is_empty() && normalized_team.contains(&normalized_query))
                || (!normalized_display.is_empty()
                    && normalized_query.contains(&normalized_display))
                || (!normalized_team.is_empty() && normalized_query.contains(&normalized_team))
        });

        let roster = match matched_roster {
            Some(r) => r,
            None => return Ok(format!("No team found matching \"{team_name}\".")),
        };

        let owner_id = roster.owner_id.as_deref().unwrap_or("");
        let user = user_map.get(owner_id);
        let display_name = user
            .and_then(|u| u.display_name.as_deref())
            .unwrap_or("Unknown");
        let team_label = user
            .and_then(|u| u.metadata.as_ref())
            .and_then(|m| m.team_name.as_deref())
            .unwrap_or("");
        let header = if !team_label.is_empty() {
            format!("{display_name} ({team_label})")
        } else {
            display_name.to_string()
        };

        let record = roster
            .settings
            .as_ref()
            .map(|s| s.record())
            .unwrap_or_else(|| "0-0".to_string());

        let mut result = format!("{header} ({record})\n");

        if let Some(pos) = position {
            result.push_str(&format!("\nShowing {pos} only:\n"));
        }

        let position_filter = position.map(|p| p.to_uppercase());
        let player_matches_filter = |id: &str| -> bool {
            match position_filter.as_deref() {
                None => true,
                Some(filter) => self
                    .players
                    .get(id)
                    .and_then(|p| p.position.as_deref())
                    .map(|pos| pos.eq_ignore_ascii_case(filter))
                    .unwrap_or(false),
            }
        };

        // Starters
        let starter_ids: Vec<&str> = roster
            .starters
            .as_ref()
            .map(|ids| {
                ids.iter()
                    .filter(|id| *id != "0")
                    .map(|s| s.as_str())
                    .collect()
            })
            .unwrap_or_default();

        let filtered_starter_ids: Vec<&str> = starter_ids
            .iter()
            .copied()
            .filter(|id| player_matches_filter(id))
            .collect();

        if !filtered_starter_ids.is_empty() {
            result.push_str("\nStarters:\n");
            for id in &filtered_starter_ids {
                let name = self
                    .players
                    .get(*id)
                    .map(|p| {
                        let pos = p.position.as_deref().unwrap_or("??");
                        let team = p.team.as_deref().unwrap_or("FA");
                        format!("  {} ({}, {})", p.full_name(), pos, team)
                    })
                    .unwrap_or_else(|| format!("  Unknown ({id})"));
                result.push_str(&name);
                result.push('\n');
            }
        }

        // Bench
        let bench: Vec<String> = roster
            .players
            .as_ref()
            .map(|ids| {
                ids.iter()
                    .filter(|id| !starter_ids.contains(&id.as_str()))
                    .filter(|id| player_matches_filter(id))
                    .filter_map(|id| {
                        self.players.get(id).map(|p| {
                            let pos = p.position.as_deref().unwrap_or("??");
                            let team = p.team.as_deref().unwrap_or("FA");
                            format!("  {} ({}, {})", p.full_name(), pos, team)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        if !bench.is_empty() {
            result.push_str("\nBench:\n");
            for p in &bench {
                result.push_str(p);
                result.push('\n');
            }
        }

        Ok(result)
    }

    fn get_player_info(&self, player_name: &str) -> Result<String> {
        let normalized_query = normalize_name(player_name);

        // Fuzzy match: find players whose normalized full name contains the normalized query
        let mut matched: Vec<(&String, &Player)> = self
            .players
            .iter()
            .filter(|(_, p)| {
                let normalized_name = normalize_name(&p.full_name());
                !normalized_query.is_empty()
                    && (normalized_name.contains(&normalized_query)
                        || normalized_query.contains(&normalized_name))
            })
            .collect();

        if matched.is_empty() {
            return Ok(format!("No player found matching \"{player_name}\"."));
        }

        matched.sort_by(|a, b| {
            let a_name = a.1.full_name();
            let b_name = b.1.full_name();
            a_name
                .cmp(&b_name)
                .then_with(|| a.1.position.cmp(&b.1.position))
                .then_with(|| a.1.team.cmp(&b.1.team))
                .then_with(|| a.0.cmp(b.0))
        });

        let exact_matches: Vec<(&String, &Player)> = matched
            .iter()
            .copied()
            .filter(|(_, p)| normalize_name(&p.full_name()) == normalized_query)
            .collect();

        let candidates = if exact_matches.is_empty() {
            &matched
        } else {
            &exact_matches
        };

        let (id, player) = if candidates.len() == 1 {
            candidates[0]
        } else {
            let active_matches: Vec<(&String, &Player)> = candidates
                .iter()
                .copied()
                .filter(|(_, p)| is_active_nfl_player(p))
                .collect();

            if active_matches.len() == 1 {
                active_matches[0]
            } else {
                let match_list = candidates
                    .iter()
                    .map(|(_, p)| format_player_match(p))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Ok(format!(
                    "Multiple players match '{player_name}': {match_list}. Please specify which one."
                ));
            }
        };

        let pos = player.position.as_deref().unwrap_or("??");
        let team = player.team.as_deref().unwrap_or("FA");
        let mut result = format!("{} ({}, {})\n", player.full_name(), pos, team);

        let summary = player.context_summary();
        if !summary.is_empty() {
            result.push_str(&format!("{summary}\n"));
        }

        // Historical stats
        if let Some(seasons) = self.historical_stats.get(id) {
            let mut sorted = seasons.clone();
            sorted.sort_by(|a, b| b.season.cmp(&a.season));
            for entry in &sorted {
                let s = entry.stats.summary(self.scoring);
                if !s.is_empty() {
                    result.push_str(&format!("{} season: {}\n", entry.season, s));
                }
            }
        }

        // Current projections
        if let Some(proj) = self.projections.get(id) {
            let s = proj.summary(self.scoring);
            if !s.is_empty() {
                result.push_str(&format!("Projected: {}\n", s));
            }
        }

        // Check if rostered
        let rostered_by = self.rosters.iter().find(|r| {
            r.players
                .as_ref()
                .map(|ps| ps.iter().any(|pid| pid == id))
                .unwrap_or(false)
        });
        if let Some(roster) = rostered_by {
            let team_name = self
                .roster_names
                .get(&roster.roster_id)
                .map(|s| s.as_str())
                .unwrap_or("Unknown");
            result.push_str(&format!("Rostered by: {team_name}\n"));
        } else {
            result.push_str("Status: Free agent (unrostered)\n");
        }

        Ok(result)
    }

    fn search_waiver_wire(&self, position: Option<&str>, limit: u32) -> Result<String> {
        let rostered: HashSet<&str> = self
            .rosters
            .iter()
            .flat_map(|r| {
                r.players
                    .as_ref()
                    .map(|ps| ps.iter().map(|s| s.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .collect();

        let pts_key = |stats: &PlayerStats| -> f64 {
            match self.scoring {
                "ppr" => stats.pts_ppr.unwrap_or(0.0),
                "std" => stats.pts_std.unwrap_or(0.0),
                _ => stats.pts_half_ppr.unwrap_or(0.0),
            }
        };

        let mut available: Vec<(String, &str, &str, f64)> = self
            .projections
            .iter()
            .filter(|(pid, _)| !rostered.contains(pid.as_str()))
            .filter_map(|(pid, proj)| {
                let pts = pts_key(proj);
                if pts < 5.0 {
                    return None;
                }
                let player = self.players.get(pid)?;
                let pos = player.position.as_deref().unwrap_or("??");
                if !matches!(pos, "QB" | "RB" | "WR" | "TE" | "K" | "DEF") {
                    return None;
                }
                if let Some(filter_pos) = position
                    && !pos.eq_ignore_ascii_case(filter_pos)
                {
                    return None;
                }
                let team = player.team.as_deref().unwrap_or("FA");
                Some((player.full_name(), pos, team, pts))
            })
            .collect();

        available.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));

        if available.is_empty() {
            let pos_note = position.map(|p| format!(" at {p}")).unwrap_or_default();
            return Ok(format!("No available players found{pos_note}."));
        }

        let mut result = String::from("Top available players:\n");
        for (i, (name, pos, team, pts)) in available.iter().take(limit as usize).enumerate() {
            result.push_str(&format!(
                "{}. {} ({}, {}) — {:.1} proj pts\n",
                i + 1,
                name,
                pos,
                team,
                pts
            ));
        }

        Ok(result)
    }

    fn get_recent_transactions(&self, tx_type: Option<&str>, limit: u32) -> Result<String> {
        let cutoff_ms = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            now.saturating_sub(14 * 24 * 60 * 60 * 1000) // 14 days
        };

        let mut shown = 0u32;
        let mut result = String::from("Recent transactions:\n");

        for tx in self.recent_transactions {
            if shown >= limit {
                break;
            }
            if tx.created.unwrap_or(0) < cutoff_ms {
                continue;
            }

            let this_type = tx.tx_type.as_deref().unwrap_or("unknown");

            // Filter by type if requested
            if let Some(filter) = tx_type
                && !this_type.eq_ignore_ascii_case(filter)
            {
                continue;
            }

            let team = tx
                .roster_ids
                .as_ref()
                .and_then(|ids| ids.first())
                .and_then(|id| self.roster_names.get(id))
                .map(|s| s.as_str())
                .unwrap_or("Unknown");

            match this_type {
                "trade" => {
                    let teams: Vec<&str> = tx
                        .roster_ids
                        .as_ref()
                        .map(|ids| {
                            ids.iter()
                                .filter_map(|id| self.roster_names.get(id).map(|s| s.as_str()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let mut line = format!("Trade: {} completed a trade", teams.join(" and "));

                    // Show what was traded
                    if let Some(adds) = &tx.adds {
                        let items: Vec<String> = adds
                            .iter()
                            .map(|(pid, roster_id)| {
                                let name = self
                                    .players
                                    .get(pid)
                                    .map(|p| p.full_name())
                                    .unwrap_or_else(|| pid.clone());
                                let to = self
                                    .roster_names
                                    .get(roster_id)
                                    .map(|s| s.as_str())
                                    .unwrap_or("?");
                                format!("{name} → {to}")
                            })
                            .collect();
                        if !items.is_empty() {
                            line.push_str(&format!(" ({})", items.join(", ")));
                        }
                    }
                    result.push_str(&format!("  {line}\n"));
                }
                "waiver" | "free_agent" => {
                    let adds: Vec<String> = tx
                        .adds
                        .as_ref()
                        .map(|m| {
                            m.keys()
                                .filter_map(|pid| self.players.get(pid))
                                .map(|p| p.full_name())
                                .collect()
                        })
                        .unwrap_or_default();
                    let drops: Vec<String> = tx
                        .drops
                        .as_ref()
                        .map(|m| {
                            m.keys()
                                .filter_map(|pid| self.players.get(pid))
                                .map(|p| p.full_name())
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut line = format!("  {team}");
                    if !adds.is_empty() {
                        line.push_str(&format!(" added {}", adds.join(", ")));
                    }
                    if !drops.is_empty() {
                        line.push_str(&format!(" dropped {}", drops.join(", ")));
                    }
                    result.push_str(&line);
                    result.push('\n');
                }
                _ => {}
            }
            shown += 1;
        }

        if shown == 0 {
            let type_note = tx_type
                .map(|t| format!(" of type \"{t}\""))
                .unwrap_or_default();
            return Ok(format!("No recent transactions found{type_note}."));
        }

        Ok(result)
    }

    async fn get_matchup(&self, team_name: &str) -> Result<String> {
        let lower_query = team_name.to_lowercase();

        // Find the roster_id for the queried team
        let matched_roster = self.rosters.iter().find(|r| {
            self.roster_names
                .get(&r.roster_id)
                .map(|n| n.to_lowercase().contains(&lower_query))
                .unwrap_or(false)
        });

        let roster = match matched_roster {
            Some(r) => r,
            None => return Ok(format!("No team found matching \"{team_name}\".")),
        };

        let week = self.nfl_state.week;
        let matchups = self.sleeper.get_matchups(self.league_id, week).await?;

        // Find this team's matchup
        let my_matchup = matchups.iter().find(|m| m.roster_id == roster.roster_id);

        let my_matchup = match my_matchup {
            Some(m) => m,
            None => return Ok(format!("No matchup found for this team in week {week}.")),
        };

        let matchup_id = match my_matchup.matchup_id {
            Some(id) => id,
            None => return Ok(format!("No matchup assigned for week {week}.")),
        };

        // Find opponent (same matchup_id, different roster_id)
        let opponent = matchups
            .iter()
            .find(|m| m.matchup_id == Some(matchup_id) && m.roster_id != roster.roster_id);

        let my_name = self
            .roster_names
            .get(&roster.roster_id)
            .map(|s| s.as_str())
            .unwrap_or("Unknown");
        let my_points = my_matchup.points.unwrap_or(0.0);

        let mut result = format!("Week {week} Matchup:\n");

        if let Some(opp) = opponent {
            let opp_name = self
                .roster_names
                .get(&opp.roster_id)
                .map(|s| s.as_str())
                .unwrap_or("Unknown");
            let opp_points = opp.points.unwrap_or(0.0);

            result.push_str(&format!(
                "{my_name} ({my_points:.1}) vs {opp_name} ({opp_points:.1})\n"
            ));

            // Show starters if available
            if let Some(starters) = &my_matchup.starters {
                let starter_pts = my_matchup.starters_points.as_deref().unwrap_or(&[]);
                result.push_str(&format!("\n{my_name} starters:\n"));
                for (j, sid) in starters.iter().enumerate() {
                    let name = self
                        .players
                        .get(sid)
                        .map(|p| {
                            let pos = p.position.as_deref().unwrap_or("??");
                            format!("{} ({})", p.full_name(), pos)
                        })
                        .unwrap_or_else(|| sid.clone());
                    let pts = starter_pts.get(j).unwrap_or(&0.0);
                    result.push_str(&format!("  {} — {:.1} pts\n", name, pts));
                }
            }

            if let Some(starters) = &opp.starters {
                let starter_pts = opp.starters_points.as_deref().unwrap_or(&[]);
                result.push_str(&format!("\n{opp_name} starters:\n"));
                for (j, sid) in starters.iter().enumerate() {
                    let name = self
                        .players
                        .get(sid)
                        .map(|p| {
                            let pos = p.position.as_deref().unwrap_or("??");
                            format!("{} ({})", p.full_name(), pos)
                        })
                        .unwrap_or_else(|| sid.clone());
                    let pts = starter_pts.get(j).unwrap_or(&0.0);
                    result.push_str(&format!("  {} — {:.1} pts\n", name, pts));
                }
            }
        } else {
            result.push_str(&format!(
                "{my_name} ({my_points:.1}) — no opponent found (bye?)\n"
            ));
        }

        Ok(result)
    }

    fn get_league_history(&self) -> Result<String> {
        let mut result = String::new();

        if !self.champions.is_empty() {
            result.push_str("League Champions:\n");
            for champ in self.champions {
                result.push_str(&format!("  {} — {}\n", champ.season, champ.display_name));
            }
        }

        if !self.all_time_stats.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("All-Time Stats:\n");
            for s in self.all_time_stats {
                let win_pct = if s.wins + s.losses > 0 {
                    s.wins as f64 / (s.wins + s.losses) as f64 * 100.0
                } else {
                    0.0
                };
                let champ_str = if s.championships > 0 {
                    format!(", {} ring(s)", s.championships)
                } else {
                    String::new()
                };
                result.push_str(&format!(
                    "  {} — {}-{} ({win_pct:.0}%), {:.1} PF, {:.1} PA{champ_str}\n",
                    s.display_name, s.wins, s.losses, s.points_for, s.points_against
                ));
            }
        }

        if result.is_empty() {
            result = "No league history available.".to_string();
        }

        Ok(result)
    }

    async fn get_past_season_results(&self, seasons_ago: u32) -> Result<String> {
        if seasons_ago == 0 {
            return Ok("Use get_league_standings for the current season.".to_string());
        }

        // Walk the previous_league_id chain `seasons_ago` times
        let mut league_id = self.league_id.to_string();
        for i in 0..seasons_ago {
            let league = self.sleeper.get_league(&league_id).await?;
            match league.previous_league_id {
                Some(ref prev) if !prev.is_empty() && prev != "0" => {
                    league_id = prev.clone();
                }
                _ => {
                    return Ok(format!(
                        "Could not go back {} season(s) — league history only goes back {} season(s).",
                        seasons_ago, i
                    ));
                }
            }
        }

        // Now fetch data for the target season's league
        let league = self.sleeper.get_league(&league_id).await?;
        let season = league.season.as_deref().unwrap_or("Unknown");
        let league_name = league.name.as_deref().unwrap_or("Unknown League");

        let users = self.sleeper.get_users(&league_id).await?;
        let rosters = self.sleeper.get_rosters(&league_id).await?;
        let roster_names = sleeper::build_roster_name_map(&users, &rosters);

        let mut result = format!("{league_name} — {season} Season Results\n\n");

        // Final standings
        let user_map: HashMap<&str, &User> =
            users.iter().map(|u| (u.user_id.as_str(), u)).collect();

        let mut entries: Vec<(String, String, f64, f64)> = Vec::new();
        for roster in &rosters {
            let owner_id = match roster.owner_id.as_deref() {
                Some(id) => id,
                None => continue,
            };
            let user = user_map.get(owner_id);
            let display_name = user
                .and_then(|u| u.display_name.as_deref())
                .unwrap_or("Unknown");
            let team_name = user
                .and_then(|u| u.metadata.as_ref())
                .and_then(|m| m.team_name.as_deref())
                .unwrap_or("");
            let name = if !team_name.is_empty() {
                format!("{display_name} ({team_name})")
            } else {
                display_name.to_string()
            };
            let settings = roster.settings.as_ref();
            let record = settings
                .map(|s| s.record())
                .unwrap_or_else(|| "0-0".to_string());
            let points = settings.map(|s| s.total_points()).unwrap_or(0.0);
            let pts_against = settings.map(|s| s.points_against()).unwrap_or(0.0);
            entries.push((name, record, points, pts_against));
        }
        // Sort by wins desc then points desc
        entries.sort_by(|a, b| {
            let a_wins: u32 =
                a.1.split('-')
                    .next()
                    .and_then(|w| w.parse().ok())
                    .unwrap_or(0);
            let b_wins: u32 =
                b.1.split('-')
                    .next()
                    .and_then(|w| w.parse().ok())
                    .unwrap_or(0);
            b_wins
                .cmp(&a_wins)
                .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
        });

        result.push_str("Final Standings:\n");
        for (i, (name, record, pf, pa)) in entries.iter().enumerate() {
            result.push_str(&format!(
                "{}. {} — {}, {:.1} PF, {:.1} PA\n",
                i + 1,
                name,
                record,
                pf,
                pa
            ));
        }

        // Playoff bracket
        if let Ok(bracket) = self.sleeper.get_winners_bracket(&league_id).await
            && !bracket.is_empty()
        {
            // Build roster_id → name map for bracket
            let rid_to_name: HashMap<u32, &str> = roster_names
                .iter()
                .map(|(id, name)| (*id, name.as_str()))
                .collect();

            // Group by round
            let max_round = bracket.iter().map(|b| b.r).max().unwrap_or(0);

            result.push_str("\nPlayoff Bracket:\n");
            for round in 1..=max_round {
                let round_label = if round == max_round {
                    "Championship".to_string()
                } else if round == max_round - 1 && max_round > 2 {
                    "Semifinals".to_string()
                } else {
                    format!("Round {round}")
                };
                result.push_str(&format!("  {round_label}:\n"));

                let mut matches: Vec<_> = bracket.iter().filter(|b| b.r == round).collect();
                matches.sort_by_key(|b| b.m);

                for m in matches {
                    let t1 =
                        m.t1.and_then(|id| rid_to_name.get(&id).copied())
                            .unwrap_or("TBD");
                    let t2 =
                        m.t2.and_then(|id| rid_to_name.get(&id).copied())
                            .unwrap_or("TBD");
                    let winner = m.w.and_then(|id| rid_to_name.get(&id).copied());

                    let mut line = format!("    {t1} vs {t2}");
                    if let Some(w) = winner {
                        line.push_str(&format!(" → Winner: {w}"));
                    }
                    result.push_str(&line);
                    result.push('\n');
                }
            }

            // Highlight champion
            if let Some(champ_match) = bracket.iter().find(|b| b.r == max_round && b.m == 1)
                && let Some(winner_id) = champ_match.w
            {
                let champ = rid_to_name.get(&winner_id).copied().unwrap_or("Unknown");
                result.push_str(&format!("\nChampion: {champ}\n"));
            }
        }

        Ok(result)
    }

    /// Search league chat messages with optional filters. Paginates through up to 1000 messages.
    async fn search_league_messages(
        &self,
        username: Option<&str>,
        keyword: Option<&str>,
        after_date: Option<&str>,
        before_date: Option<&str>,
        limit: u32,
    ) -> Result<String> {
        let gql = self
            .gql
            .ok_or_else(|| anyhow!("Message search is not available (no GraphQL client)"))?;

        // Parse date filters into timestamps
        let after_ts: Option<i64> = after_date.and_then(|d| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .ok()
                .and_then(|nd| nd.and_hms_opt(0, 0, 0))
                .map(|dt| dt.and_utc().timestamp_millis())
        });
        let before_ts: Option<i64> = before_date.and_then(|d| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .ok()
                .and_then(|nd| nd.and_hms_opt(23, 59, 59))
                .map(|dt| dt.and_utc().timestamp_millis())
        });

        let username_lower = username.map(|u| u.to_lowercase());
        let keyword_lower = keyword.map(|k| k.to_lowercase());

        let mut all_matches: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut total_fetched: usize = 0;
        const MAX_MESSAGES: usize = 1000;
        const MAX_PAGES: usize = 40; // safety limit on API calls

        for _ in 0..MAX_PAGES {
            if total_fetched >= MAX_MESSAGES {
                break;
            }

            let messages = gql
                .fetch_messages(self.league_id, cursor.as_deref())
                .await?;

            if messages.is_empty() {
                break;
            }

            // The oldest message in this batch becomes the cursor for the next page
            let oldest_id = messages.last().and_then(|m| m.message_id.clone());

            for msg in &messages {
                total_fetched += 1;

                let created = msg.created.unwrap_or(0);

                // Date filters — Sleeper timestamps appear to be in milliseconds
                if let Some(after) = after_ts
                    && created < after
                {
                    // Messages are newest-first; if we've gone past after_date, skip
                    // but keep paginating in case ordering isn't guaranteed
                    continue;
                }
                if let Some(before) = before_ts
                    && created > before
                {
                    continue;
                }

                // Username filter
                if let Some(ref u) = username_lower {
                    let author = msg
                        .author_display_name
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase();
                    if !author.contains(u.as_str()) {
                        continue;
                    }
                }

                // Keyword filter
                if let Some(ref k) = keyword_lower {
                    let text = msg.text.as_deref().unwrap_or("").to_lowercase();
                    if !text.contains(k.as_str()) {
                        continue;
                    }
                }

                // Format the matching message
                let author = msg.author_display_name.as_deref().unwrap_or("Unknown");
                let text = msg.text.as_deref().unwrap_or("");
                let date_str = if created > 0 {
                    // Try both milliseconds and seconds
                    let ts_secs = if created > 1_000_000_000_000 {
                        created / 1000
                    } else {
                        created
                    };
                    chrono::DateTime::from_timestamp(ts_secs, 0)
                        .map(|dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
                        .unwrap_or_else(|| "unknown date".to_string())
                } else {
                    "unknown date".to_string()
                };

                all_matches.push(format!("[{date_str}] {author}: {text}"));
            }

            cursor = oldest_id;
            if cursor.is_none() {
                break;
            }
        }

        if all_matches.is_empty() {
            let mut desc = String::from("No messages found");
            let mut filters = Vec::new();
            if let Some(u) = username {
                filters.push(format!("from user \"{u}\""));
            }
            if let Some(k) = keyword {
                filters.push(format!("containing \"{k}\""));
            }
            if let Some(d) = after_date {
                filters.push(format!("after {d}"));
            }
            if let Some(d) = before_date {
                filters.push(format!("before {d}"));
            }
            if !filters.is_empty() {
                desc.push(' ');
                desc.push_str(&filters.join(", "));
            }
            desc.push_str(&format!(" (searched {total_fetched} messages)."));
            return Ok(desc);
        }

        let total_matches = all_matches.len();
        let limit_usize = limit as usize;
        let truncated = total_matches > limit_usize;
        // Show oldest first for chronological reading
        all_matches.reverse();
        if truncated {
            // After reversing, the oldest matches are first; keep the most recent `limit`
            let drop = total_matches - limit_usize;
            all_matches.drain(..drop);
        }

        let mut result = if truncated {
            format!(
                "Found {total_matches} matching message(s) (searched {total_fetched} messages, showing {limit_usize} of {total_matches} matches — refine your search for more specific results):\n\n"
            )
        } else {
            format!(
                "Found {total_matches} matching message(s) (searched {total_fetched} messages):\n\n"
            )
        };
        for m in &all_matches {
            result.push_str(m);
            result.push('\n');
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sleeper::{RosterSettings, UserMetadata};

    fn test_player(first: &str, last: &str, position: &str, team: Option<&str>) -> Player {
        Player {
            first_name: Some(first.to_string()),
            last_name: Some(last.to_string()),
            position: Some(position.to_string()),
            team: team.map(|t| t.to_string()),
            ..Player::default()
        }
    }

    #[test]
    fn test_all_tool_definitions_count() {
        let tools = all_tool_definitions();
        assert_eq!(tools.len(), 9);
        for tool in &tools {
            assert!(tool.get("name").is_some(), "Tool missing 'name'");
            assert!(
                tool.get("description").is_some(),
                "Tool missing 'description'"
            );
            assert!(
                tool.get("input_schema").is_some(),
                "Tool missing 'input_schema'"
            );
        }
    }

    #[test]
    fn test_parse_no_arg_tools() {
        let input = json!({});
        assert!(matches!(
            parse_tool_call("get_league_standings", &input).unwrap(),
            ToolName::GetLeagueStandings
        ));
        assert!(matches!(
            parse_tool_call("get_league_history", &input).unwrap(),
            ToolName::GetLeagueHistory
        ));
    }

    #[test]
    fn test_parse_get_team_roster() {
        let input = json!({"team_name": "Touchdown Tyrants"});
        match parse_tool_call("get_team_roster", &input).unwrap() {
            ToolName::GetTeamRoster { team_name, position } => {
                assert_eq!(team_name, "Touchdown Tyrants");
                assert!(position.is_none());
            }
            _ => panic!("Expected GetTeamRoster"),
        }
    }

    #[test]
    fn test_parse_get_team_roster_with_position() {
        let input = json!({"team_name": "Touchdown Tyrants", "position": "RB"});
        match parse_tool_call("get_team_roster", &input).unwrap() {
            ToolName::GetTeamRoster { team_name, position } => {
                assert_eq!(team_name, "Touchdown Tyrants");
                assert_eq!(position, Some("RB".to_string()));
            }
            _ => panic!("Expected GetTeamRoster"),
        }
    }

    #[test]
    fn test_parse_get_team_roster_missing_field() {
        let input = json!({});
        assert!(parse_tool_call("get_team_roster", &input).is_err());
    }

    #[test]
    fn test_parse_get_player_info() {
        let input = json!({"player_name": "Patrick Mahomes"});
        match parse_tool_call("get_player_info", &input).unwrap() {
            ToolName::GetPlayerInfo { player_name } => {
                assert_eq!(player_name, "Patrick Mahomes");
            }
            _ => panic!("Expected GetPlayerInfo"),
        }
    }

    #[test]
    fn test_parse_search_waiver_wire_with_all_params() {
        let input = json!({"position": "RB", "limit": 5});
        match parse_tool_call("search_waiver_wire", &input).unwrap() {
            ToolName::SearchWaiverWire { position, limit } => {
                assert_eq!(position, Some("RB".to_string()));
                assert_eq!(limit, Some(5));
            }
            _ => panic!("Expected SearchWaiverWire"),
        }
    }

    #[test]
    fn test_parse_search_waiver_wire_no_params() {
        let input = json!({});
        match parse_tool_call("search_waiver_wire", &input).unwrap() {
            ToolName::SearchWaiverWire { position, limit } => {
                assert!(position.is_none());
                assert!(limit.is_none());
            }
            _ => panic!("Expected SearchWaiverWire"),
        }
    }

    #[test]
    fn test_parse_search_league_messages_with_limit() {
        let input = json!({"keyword": "trade", "limit": 5});
        match parse_tool_call("search_league_messages", &input).unwrap() {
            ToolName::SearchLeagueMessages {
                username,
                keyword,
                after_date,
                before_date,
                limit,
            } => {
                assert!(username.is_none());
                assert_eq!(keyword, Some("trade".to_string()));
                assert!(after_date.is_none());
                assert!(before_date.is_none());
                assert_eq!(limit, Some(5));
            }
            _ => panic!("Expected SearchLeagueMessages"),
        }
    }

    #[test]
    fn test_parse_search_league_messages_no_limit() {
        let input = json!({});
        match parse_tool_call("search_league_messages", &input).unwrap() {
            ToolName::SearchLeagueMessages { limit, .. } => {
                assert!(limit.is_none());
            }
            _ => panic!("Expected SearchLeagueMessages"),
        }
    }

    #[test]
    fn test_parse_get_recent_transactions() {
        let input = json!({"tx_type": "trade", "limit": 3});
        match parse_tool_call("get_recent_transactions", &input).unwrap() {
            ToolName::GetRecentTransactions { tx_type, limit } => {
                assert_eq!(tx_type, Some("trade".to_string()));
                assert_eq!(limit, Some(3));
            }
            _ => panic!("Expected GetRecentTransactions"),
        }
    }

    #[test]
    fn test_parse_get_matchup() {
        let input = json!({"team_name": "Nick"});
        match parse_tool_call("get_matchup", &input).unwrap() {
            ToolName::GetMatchup { team_name } => {
                assert_eq!(team_name, "Nick");
            }
            _ => panic!("Expected GetMatchup"),
        }
    }

    #[test]
    fn test_parse_unknown_tool() {
        let input = json!({});
        assert!(parse_tool_call("nonexistent_tool", &input).is_err());
    }

    #[test]
    fn test_parse_get_past_season_results() {
        let input = json!({"seasons_ago": 2});
        match parse_tool_call("get_past_season_results", &input).unwrap() {
            ToolName::GetPastSeasonResults { seasons_ago } => {
                assert_eq!(seasons_ago, 2);
            }
            _ => panic!("Expected GetPastSeasonResults"),
        }
    }

    #[test]
    fn test_parse_get_past_season_results_missing_field() {
        assert!(parse_tool_call("get_past_season_results", &json!({})).is_err());
    }

    #[test]
    fn test_parse_missing_required_fields() {
        assert!(parse_tool_call("get_player_info", &json!({})).is_err());
        assert!(parse_tool_call("get_matchup", &json!({})).is_err());
    }

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("A.J. Brown"), "aj brown");
        assert_eq!(normalize_name("D'Andre Swift"), "dandre swift");
        assert_eq!(normalize_name("Amon-Ra   St. Brown"), "amonra st brown");
    }

    #[test]
    fn test_get_player_info_prefers_normalized_exact_match() {
        let sleeper = SleeperClient::new();
        let players = HashMap::from([
            (
                "1".to_string(),
                test_player("A.J.", "Brown", "WR", Some("PHI")),
            ),
            (
                "2".to_string(),
                test_player("Marquise", "Brown", "WR", Some("KC")),
            ),
        ]);
        let users = Vec::new();
        let rosters = Vec::new();
        let roster_names = HashMap::new();
        let nfl_state = NflState {
            week: 1,
            season: "2026".to_string(),
            season_type: "regular".to_string(),
        };
        let historical_stats = HashMap::new();
        let projections = HashMap::new();
        let champions = Vec::new();
        let all_time_stats = Vec::new();
        let recent_transactions = Vec::new();

        let executor = ToolExecutor {
            sleeper: &sleeper,
            league_id: "league",
            players: &players,
            users: &users,
            rosters: &rosters,
            roster_names: &roster_names,
            nfl_state: &nfl_state,
            historical_stats: &historical_stats,
            projections: &projections,
            champions: &champions,
            all_time_stats: &all_time_stats,
            scoring: "half_ppr",
            recent_transactions: &recent_transactions,
            gql: None,
        };

        let result = executor.get_player_info("AJ Brown").unwrap();
        assert!(result.starts_with("A.J. Brown (WR, PHI)\n"));
    }

    #[test]
    fn test_get_player_info_disambiguates_multiple_matches() {
        let sleeper = SleeperClient::new();
        let players = HashMap::from([
            (
                "1".to_string(),
                test_player("Mike", "Williams", "WR", Some("NYJ")),
            ),
            (
                "2".to_string(),
                test_player("Jameson", "Williams", "WR", Some("DET")),
            ),
            (
                "3".to_string(),
                test_player("Caleb", "Williams", "QB", Some("CHI")),
            ),
        ]);
        let users = Vec::new();
        let rosters = Vec::new();
        let roster_names = HashMap::new();
        let nfl_state = NflState {
            week: 1,
            season: "2026".to_string(),
            season_type: "regular".to_string(),
        };
        let historical_stats = HashMap::new();
        let projections = HashMap::new();
        let champions = Vec::new();
        let all_time_stats = Vec::new();
        let recent_transactions = Vec::new();

        let executor = ToolExecutor {
            sleeper: &sleeper,
            league_id: "league",
            players: &players,
            users: &users,
            rosters: &rosters,
            roster_names: &roster_names,
            nfl_state: &nfl_state,
            historical_stats: &historical_stats,
            projections: &projections,
            champions: &champions,
            all_time_stats: &all_time_stats,
            scoring: "half_ppr",
            recent_transactions: &recent_transactions,
            gql: None,
        };

        let result = executor.get_player_info("Williams").unwrap();
        assert_eq!(
            result,
            "Multiple players match 'Williams': Caleb Williams (QB, CHI), Jameson Williams (WR, DET), Mike Williams (WR, NYJ). Please specify which one."
        );
    }

    #[test]
    fn test_get_team_roster_uses_normalized_matching() {
        let sleeper = SleeperClient::new();
        let players = HashMap::from([(
            "1".to_string(),
            test_player("D'Andre", "Swift", "RB", Some("CHI")),
        )]);
        let users = vec![User {
            user_id: "user-1".to_string(),
            display_name: Some("Nick".to_string()),
            metadata: Some(UserMetadata {
                team_name: Some("D'Andre Dogs".to_string()),
            }),
        }];
        let rosters = vec![Roster {
            roster_id: 1,
            owner_id: Some("user-1".to_string()),
            players: Some(vec!["1".to_string()]),
            starters: Some(vec!["1".to_string()]),
            settings: Some(RosterSettings {
                wins: Some(3),
                losses: Some(1),
                ties: Some(0),
                fpts: None,
                fpts_decimal: None,
                fpts_against: None,
                fpts_against_decimal: None,
            }),
        }];
        let roster_names = HashMap::from([(1, "Nick (D'Andre Dogs)".to_string())]);
        let nfl_state = NflState {
            week: 1,
            season: "2026".to_string(),
            season_type: "regular".to_string(),
        };
        let historical_stats = HashMap::new();
        let projections = HashMap::new();
        let champions = Vec::new();
        let all_time_stats = Vec::new();
        let recent_transactions = Vec::new();

        let executor = ToolExecutor {
            sleeper: &sleeper,
            league_id: "league",
            players: &players,
            users: &users,
            rosters: &rosters,
            roster_names: &roster_names,
            nfl_state: &nfl_state,
            historical_stats: &historical_stats,
            projections: &projections,
            champions: &champions,
            all_time_stats: &all_time_stats,
            scoring: "half_ppr",
            recent_transactions: &recent_transactions,
            gql: None,
        };

        let result = executor.get_team_roster("dandre dogs", None).unwrap();
        assert!(result.contains("Nick (D'Andre Dogs) (3-1)"));
        assert!(result.contains("D'Andre Swift (RB, CHI)"));
    }

    #[test]
    fn test_get_team_roster_position_filter() {
        let sleeper = SleeperClient::new();
        let players = HashMap::from([
            (
                "1".to_string(),
                test_player("D'Andre", "Swift", "RB", Some("CHI")),
            ),
            (
                "2".to_string(),
                test_player("Patrick", "Mahomes", "QB", Some("KC")),
            ),
            (
                "3".to_string(),
                test_player("Justin", "Jefferson", "WR", Some("MIN")),
            ),
        ]);
        let users = vec![User {
            user_id: "user-1".to_string(),
            display_name: Some("Nick".to_string()),
            metadata: Some(UserMetadata {
                team_name: Some("Touchdown Tyrants".to_string()),
            }),
        }];
        let rosters = vec![Roster {
            roster_id: 1,
            owner_id: Some("user-1".to_string()),
            players: Some(vec!["1".to_string(), "2".to_string(), "3".to_string()]),
            starters: Some(vec!["2".to_string(), "1".to_string(), "3".to_string()]),
            settings: Some(RosterSettings {
                wins: Some(3),
                losses: Some(1),
                ties: Some(0),
                fpts: None,
                fpts_decimal: None,
                fpts_against: None,
                fpts_against_decimal: None,
            }),
        }];
        let roster_names = HashMap::from([(1, "Nick (Touchdown Tyrants)".to_string())]);
        let nfl_state = NflState {
            week: 1,
            season: "2026".to_string(),
            season_type: "regular".to_string(),
        };
        let historical_stats = HashMap::new();
        let projections = HashMap::new();
        let champions = Vec::new();
        let all_time_stats = Vec::new();
        let recent_transactions = Vec::new();

        let executor = ToolExecutor {
            sleeper: &sleeper,
            league_id: "league",
            players: &players,
            users: &users,
            rosters: &rosters,
            roster_names: &roster_names,
            nfl_state: &nfl_state,
            historical_stats: &historical_stats,
            projections: &projections,
            champions: &champions,
            all_time_stats: &all_time_stats,
            scoring: "half_ppr",
            recent_transactions: &recent_transactions,
            gql: None,
        };

        let result = executor.get_team_roster("Touchdown", Some("RB")).unwrap();
        assert!(result.contains("Showing RB only:"));
        assert!(result.contains("D'Andre Swift (RB, CHI)"));
        assert!(!result.contains("Mahomes"));
        assert!(!result.contains("Jefferson"));
    }
}

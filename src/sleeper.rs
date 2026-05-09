use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const BASE_URL: &str = "https://api.sleeper.app/v1";
const PLAYER_CACHE_FILE: &str = "players_cache.json";
const CACHE_MAX_AGE_DAYS: i64 = 7;

// ─── Data Models ───

#[derive(Debug, Deserialize)]
pub struct NflState {
    pub week: u32,
    pub season: String,
    pub season_type: String,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub user_id: String,
    pub display_name: Option<String>,
    pub metadata: Option<UserMetadata>,
}

#[derive(Debug, Deserialize)]
pub struct UserMetadata {
    pub team_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Roster {
    pub roster_id: u32,
    pub owner_id: Option<String>,
    pub players: Option<Vec<String>>,
    pub starters: Option<Vec<String>>,
    pub settings: Option<RosterSettings>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct RosterSettings {
    pub wins: Option<u32>,
    pub losses: Option<u32>,
    pub ties: Option<u32>,
    pub fpts: Option<u64>,
    pub fpts_decimal: Option<u64>,
    pub fpts_against: Option<u64>,
    pub fpts_against_decimal: Option<u64>,
}

impl RosterSettings {
    pub fn record(&self) -> String {
        let w = self.wins.unwrap_or(0);
        let l = self.losses.unwrap_or(0);
        let t = self.ties.unwrap_or(0);
        if t > 0 {
            format!("{w}-{l}-{t}")
        } else {
            format!("{w}-{l}")
        }
    }

    pub fn total_points(&self) -> f64 {
        let fpts = self.fpts.unwrap_or(0) as f64;
        let dec = self.fpts_decimal.unwrap_or(0) as f64;
        fpts + dec / 100.0
    }

    pub fn points_against(&self) -> f64 {
        let fpts = self.fpts_against.unwrap_or(0) as f64;
        let dec = self.fpts_against_decimal.unwrap_or(0) as f64;
        fpts + dec / 100.0
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Transaction {
    #[serde(rename = "type")]
    pub tx_type: Option<String>,
    pub transaction_id: Option<String>,
    pub status: Option<String>,
    pub roster_ids: Option<Vec<u32>>,
    pub adds: Option<HashMap<String, u32>>,
    pub drops: Option<HashMap<String, u32>>,
    pub draft_picks: Option<Vec<DraftPick>>,
    pub created: Option<u64>,
    pub status_updated: Option<u64>,
}

impl Transaction {
    pub fn is_completed_trade(&self) -> bool {
        self.tx_type.as_deref() == Some("trade") && self.status.as_deref() == Some("complete")
    }

    pub fn id(&self) -> &str {
        self.transaction_id.as_deref().unwrap_or("unknown")
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct DraftPick {
    pub season: Option<String>,
    pub round: Option<u32>,
    pub roster_id: Option<u32>,
    pub previous_owner_id: Option<u32>,
    pub owner_id: Option<u32>,
}

/// Metadata for a league season.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct League {
    pub league_id: Option<String>,
    pub name: Option<String>,
    pub season: Option<String>,
    pub status: Option<String>,
    pub previous_league_id: Option<String>,
    pub total_rosters: Option<u32>,
    pub roster_positions: Option<Vec<String>>,
    pub scoring_settings: Option<HashMap<String, f64>>,
    pub settings: Option<LeagueSettings>,
}

impl League {
    /// Derive the scoring format ("ppr" / "half_ppr" / "std") from the
    /// league's `scoring_settings.rec` value. Falls back to "half_ppr" if the
    /// API didn't return scoring settings.
    pub fn detect_scoring(&self) -> &'static str {
        let rec = self
            .scoring_settings
            .as_ref()
            .and_then(|s| s.get("rec").copied())
            .unwrap_or(0.5);
        // Allow a small epsilon for float comparisons.
        if rec >= 0.99 {
            "ppr"
        } else if rec >= 0.49 {
            "half_ppr"
        } else if rec >= 0.01 {
            // Some leagues use 0.25 PPR — round to half_ppr for the projections lookup.
            "half_ppr"
        } else {
            "std"
        }
    }

    /// True if any starter slot allows two QBs (i.e. SUPER_FLEX / SF / Q-FLEX).
    pub fn is_superflex(&self) -> bool {
        self.roster_positions
            .as_ref()
            .map(|positions| {
                positions.iter().any(|p| {
                    let upper = p.to_uppercase();
                    upper == "SUPER_FLEX" || upper == "SF" || upper == "Q-FLEX" || upper == "QFLEX"
                })
            })
            .unwrap_or(false)
    }

    /// Build a compact, ordered summary of `roster_positions` like
    /// "1 QB, 2 RB, 3 WR, 1 TE, 1 SUPER_FLEX, 1 FLEX, 1 K, 1 DEF, 14 BN, 2 IR".
    pub fn format_roster_positions(&self) -> String {
        let positions = match self.roster_positions.as_deref() {
            Some(p) if !p.is_empty() => p,
            _ => return "Unknown roster format".to_string(),
        };

        let mut counts: HashMap<String, u32> = HashMap::new();
        for slot in positions {
            *counts.entry(slot.to_uppercase()).or_insert(0) += 1;
        }

        // Stable display order: starters first, then flexes, K/DEF/IDP, then bench/IR/taxi.
        const ORDER: &[&str] = &[
            "QB",
            "RB",
            "WR",
            "TE",
            "SUPER_FLEX",
            "SF",
            "Q-FLEX",
            "QFLEX",
            "REC_FLEX",
            "WRRB_FLEX",
            "FLEX",
            "K",
            "DEF",
            "DL",
            "LB",
            "DB",
            "IDP_FLEX",
            "BN",
            "IR",
            "TAXI",
        ];

        let mut parts: Vec<String> = Vec::new();
        let mut emitted: HashSet<String> = HashSet::new();
        for key in ORDER {
            if let Some(n) = counts.get(*key) {
                parts.push(format!("{n} {key}"));
                emitted.insert((*key).to_string());
            }
        }
        // Append any unrecognised slots in alphabetical order so we don't lose info.
        let mut leftover: Vec<(&String, &u32)> = counts
            .iter()
            .filter(|(k, _)| !emitted.contains(k.as_str()))
            .collect();
        leftover.sort_by(|a, b| a.0.cmp(b.0));
        for (k, n) in leftover {
            parts.push(format!("{n} {k}"));
        }

        parts.join(", ")
    }

    /// Pick out the most useful scoring rules for the LLM (PPR amount, pass TD
    /// value, TE premium, big yardage bonuses). Returns `None` if nothing
    /// non-default is worth highlighting.
    pub fn format_scoring_highlights(&self) -> Option<String> {
        let s = self.scoring_settings.as_ref()?;
        let mut parts: Vec<String> = Vec::new();

        // Reception value
        let rec = s.get("rec").copied().unwrap_or(0.0);
        if rec >= 0.99 {
            parts.push("full PPR".to_string());
        } else if rec >= 0.49 {
            parts.push("half PPR".to_string());
        } else if rec >= 0.01 {
            parts.push(format!("{rec} PPR"));
        } else {
            parts.push("standard (no PPR)".to_string());
        }

        // Passing TD value (4 vs 6 is the big one)
        let pass_td = s.get("pass_td").copied().unwrap_or(4.0);
        if (pass_td - 4.0).abs() > 0.01 {
            parts.push(format!("{pass_td:.0}pt pass TDs"));
        }

        // TE premium
        if let Some(bonus) = s.get("bonus_rec_te").copied()
            && bonus > 0.01
        {
            parts.push(format!("+{bonus} TE premium"));
        }

        // Big-game receiving yardage bonuses
        for (key, label) in [
            ("bonus_rec_yd_100", "100+ rec yds"),
            ("bonus_rec_yd_200", "200+ rec yds"),
            ("bonus_rush_yd_100", "100+ rush yds"),
            ("bonus_rush_yd_200", "200+ rush yds"),
            ("bonus_pass_yd_300", "300+ pass yds"),
            ("bonus_pass_yd_400", "400+ pass yds"),
        ] {
            if let Some(v) = s.get(key).copied()
                && v > 0.01
            {
                parts.push(format!("+{v} {label}"));
            }
        }

        Some(parts.join(", "))
    }

    /// Build a single-line league summary suitable for injecting into LLM
    /// system prompts:
    ///   "10-team superflex dynasty league. Lineup: 1 QB, 2 RB, ... Bench/IR/Taxi
    ///    counted. Scoring: full PPR, 6pt pass TDs, +0.5 TE premium."
    /// `extra_rules` is appended verbatim if provided (free-form notes from
    /// `config.toml` for things the API doesn't expose, e.g. payouts).
    pub fn format_summary(&self, extra_rules: Option<&str>) -> String {
        let team_count = self
            .total_rosters
            .or_else(|| self.settings.as_ref().and_then(|s| s.num_teams))
            .unwrap_or(0);

        let league_kind = self
            .settings
            .as_ref()
            .map(|s| match s.league_type {
                Some(2) => "dynasty",
                Some(1) => "keeper",
                Some(0) => "redraft",
                _ => "fantasy",
            })
            .unwrap_or("fantasy");

        let superflex_label = if self.is_superflex() {
            " superflex"
        } else {
            ""
        };

        let mut summary = if team_count > 0 {
            format!("{team_count}-team{superflex_label} {league_kind} league.")
        } else {
            format!("{league_kind} league.")
        };

        let lineup = self.format_roster_positions();
        if !lineup.is_empty() && lineup != "Unknown roster format" {
            summary.push_str(&format!(" Lineup: {lineup}."));
        }

        if let Some(highlights) = self.format_scoring_highlights() {
            summary.push_str(&format!(" Scoring: {highlights}."));
        }

        if let Some(notes) = extra_rules {
            let trimmed = notes.trim();
            if !trimmed.is_empty() {
                summary.push_str(&format!(" Additional notes: {trimmed}"));
            }
        }

        summary
    }
}

/// League-level settings from the Sleeper API.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct LeagueSettings {
    pub num_teams: Option<u32>,
    pub playoff_teams: Option<u32>,
    pub playoff_week_start: Option<u32>,
    pub playoff_type: Option<u32>,
    pub trade_deadline: Option<u32>,
    pub trade_review_days: Option<u32>,
    pub pick_trading: Option<u32>,
    pub waiver_type: Option<u32>,
    pub waiver_budget: Option<u32>,
    pub taxi_slots: Option<u32>,
    pub taxi_years: Option<u32>,
    pub taxi_allow_vets: Option<u32>,
    pub reserve_slots: Option<u32>,
    pub bench_lock: Option<u32>,
    pub best_ball: Option<u32>,
    pub disable_trades: Option<u32>,
    pub offseason_adds: Option<u32>,
    pub draft_rounds: Option<u32>,
    #[serde(rename = "type")]
    pub league_type: Option<u32>,
    pub position_limit_qb: Option<u32>,
}

/// A single weekly matchup entry from the Sleeper API.
#[derive(Debug, Deserialize)]
pub struct Matchup {
    pub roster_id: u32,
    pub matchup_id: Option<u32>,
    pub points: Option<f64>,
    pub starters: Option<Vec<String>>,
    pub starters_points: Option<Vec<f64>>,
}

/// A single matchup entry in a playoff bracket.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct BracketMatch {
    pub r: u32,
    pub m: u32,
    pub t1: Option<u32>,
    pub t2: Option<u32>,
    pub w: Option<u32>,
    pub l: Option<u32>,
}

/// Champion entry for a single historical season.
#[derive(Debug, Clone)]
pub struct SeasonChampion {
    pub season: String,
    pub display_name: String,
}

/// All-time aggregated stats for a single user across all seasons.
#[derive(Debug, Clone, Default)]
pub struct AllTimeUserStats {
    pub display_name: String,
    pub seasons: u32,
    pub wins: u32,
    pub losses: u32,
    pub points_for: f64,
    pub points_against: f64,
    pub championships: u32,
}

/// Per-player season stats or projections from the Sleeper API.
/// Fields are all optional because not every player has every stat category.
#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub struct PlayerStats {
    pub pts_half_ppr: Option<f64>,
    pub pts_ppr: Option<f64>,
    pub pts_std: Option<f64>,
    pub gp: Option<f64>,
    pub rec: Option<f64>,
    pub rec_yd: Option<f64>,
    pub rec_td: Option<f64>,
    pub rush_att: Option<f64>,
    pub rush_yd: Option<f64>,
    pub rush_td: Option<f64>,
    pub pass_yd: Option<f64>,
    pub pass_td: Option<f64>,
    pub pass_int: Option<f64>,
    pub pass_att: Option<f64>,
    pub fum_lost: Option<f64>,
}

impl PlayerStats {
    /// Build a concise summary string from the stats.
    pub fn summary(&self, scoring: &str) -> String {
        let pts = match scoring {
            "ppr" => self.pts_ppr,
            "std" => self.pts_std,
            _ => self.pts_half_ppr, // default to half PPR
        };
        let mut parts = Vec::new();

        if let Some(p) = pts {
            parts.push(format!("{p:.1} pts"));
        }
        if let Some(gp) = self.gp {
            let gp = gp as u32;
            if gp > 0 {
                parts.push(format!("{gp} gms"));
                if let Some(p) = pts {
                    parts.push(format!("{:.1} ppg", p / gp as f64));
                }
            }
        }

        // Show relevant stat lines based on what's present
        if let Some(pass_yd) = self.pass_yd {
            let td = self.pass_td.unwrap_or(0.0) as u32;
            let int = self.pass_int.unwrap_or(0.0) as u32;
            parts.push(format!("{:.0} pass yds, {} TD, {} INT", pass_yd, td, int));
        }
        if let Some(rush_yd) = self.rush_yd
            && rush_yd > 10.0
        {
            let td = self.rush_td.unwrap_or(0.0) as u32;
            parts.push(format!("{:.0} rush yds, {} TD", rush_yd, td));
        }
        if let Some(rec) = self.rec
            && rec > 1.0
        {
            let yd = self.rec_yd.unwrap_or(0.0);
            let td = self.rec_td.unwrap_or(0.0) as u32;
            parts.push(format!("{:.0} rec, {:.0} rec yds, {} TD", rec, yd, td));
        }

        parts.join(", ")
    }
}

/// Aggregated fantasy stats for a player across multiple seasons.
#[derive(Debug, Clone, Default)]
pub struct PlayerSeasonEntry {
    pub season: String,
    pub stats: PlayerStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Player {
    #[serde(default)]
    pub player_id: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub position: Option<String>,
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub age: Option<u32>,
    #[serde(default)]
    pub years_exp: Option<u32>,
    #[serde(default)]
    pub college: Option<String>,
    #[serde(default)]
    pub injury_status: Option<String>,
    #[serde(default)]
    pub injury_body_part: Option<String>,
    #[serde(default)]
    pub injury_notes: Option<String>,
    #[serde(default)]
    pub injury_start_date: Option<String>,
    #[serde(default)]
    pub depth_chart_order: Option<u32>,
    #[serde(default)]
    pub depth_chart_position: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub number: Option<u32>,
    #[serde(default)]
    pub birth_date: Option<String>,
    #[serde(default)]
    pub news_updated: Option<u64>,
}

impl Player {
    pub fn full_name(&self) -> String {
        let first = self.first_name.as_deref().unwrap_or("");
        let last = self.last_name.as_deref().unwrap_or("");
        format!("{first} {last}").trim().to_string()
    }

    /// Build a rich context string with all available player metadata.
    pub fn context_summary(&self) -> String {
        let mut parts = Vec::new();

        if let Some(age) = self.age {
            parts.push(format!("Age: {age}"));
        }
        if let Some(exp) = self.years_exp {
            parts.push(format!("Experience: {exp} yr(s)"));
        }
        if let Some(ref status) = self.status
            && status != "Active"
        {
            parts.push(format!("Status: {status}"));
        }
        if let Some(ref injury) = self.injury_status {
            let mut inj = format!("Injury: {injury}");
            if let Some(ref part) = self.injury_body_part {
                inj.push_str(&format!(" ({part})"));
            }
            if let Some(ref notes) = self.injury_notes {
                inj.push_str(&format!(" - {notes}"));
            }
            parts.push(inj);
        }
        if let Some(order) = self.depth_chart_order
            && let Some(ref pos) = self.depth_chart_position
        {
            parts.push(format!("Depth chart: #{order} {pos}"));
        }

        parts.join(", ")
    }
}

// ─── Client ───

pub struct SleeperClient {
    client: reqwest::Client,
    players: Option<HashMap<String, Player>>,
}

impl SleeperClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            players: None,
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("HTTP request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {status} from {url}: {body}");
        }
        resp.json::<T>().await.context("Failed to parse JSON")
    }

    async fn get_json_with_retry<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        match self.get_json(url).await {
            Ok(v) => Ok(v),
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                self.get_json(url).await
            }
        }
    }

    pub async fn get_nfl_state(&self) -> Result<NflState> {
        self.get_json_with_retry(&format!("{BASE_URL}/state/nfl"))
            .await
    }

    pub async fn get_users(&self, league_id: &str) -> Result<Vec<User>> {
        self.get_json_with_retry(&format!("{BASE_URL}/league/{league_id}/users"))
            .await
    }

    pub async fn get_rosters(&self, league_id: &str) -> Result<Vec<Roster>> {
        self.get_json_with_retry(&format!("{BASE_URL}/league/{league_id}/rosters"))
            .await
    }

    pub async fn get_league(&self, league_id: &str) -> Result<League> {
        self.get_json_with_retry(&format!("{BASE_URL}/league/{league_id}"))
            .await
    }

    pub async fn get_winners_bracket(&self, league_id: &str) -> Result<Vec<BracketMatch>> {
        self.get_json_with_retry(&format!("{BASE_URL}/league/{league_id}/winners_bracket"))
            .await
    }

    pub async fn get_matchups(&self, league_id: &str, week: u32) -> Result<Vec<Matchup>> {
        self.get_json_with_retry(&format!("{BASE_URL}/league/{league_id}/matchups/{week}"))
            .await
    }

    /// Walk back through the previous_league_id chain and collect:
    /// - A champion per completed season
    /// - All-time win/loss/points aggregated per user_id
    ///
    /// Includes the current season in stats but only marks a champion if status == "complete".
    pub async fn fetch_league_history(
        &self,
        current_league_id: &str,
    ) -> (Vec<SeasonChampion>, Vec<AllTimeUserStats>) {
        let mut champions: Vec<SeasonChampion> = Vec::new();
        // user_id → stats
        let mut stats_map: HashMap<String, AllTimeUserStats> = HashMap::new();

        let mut league_id = current_league_id.to_string();
        let mut visited = std::collections::HashSet::new();

        loop {
            if visited.contains(&league_id) {
                break;
            }
            visited.insert(league_id.clone());

            // Fetch league metadata
            let league = match self.get_league(&league_id).await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("Warning: failed to fetch league {league_id}: {e}");
                    break;
                }
            };

            let season = league.season.clone().unwrap_or_else(|| league_id.clone());
            let is_complete = league.status.as_deref() == Some("complete");

            // Fetch users and rosters for this season
            let users: Vec<User> = match self
                .get_json_with_retry(&format!("{BASE_URL}/league/{league_id}/users"))
                .await
            {
                Ok(u) => u,
                Err(e) => {
                    eprintln!("Warning: failed to fetch users for league {league_id}: {e}");
                    vec![]
                }
            };
            let rosters: Vec<Roster> = match self
                .get_json_with_retry(&format!("{BASE_URL}/league/{league_id}/rosters"))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Warning: failed to fetch rosters for league {league_id}: {e}");
                    vec![]
                }
            };

            // Build user_id → display name for this season
            let user_names: HashMap<&str, &str> = users
                .iter()
                .filter_map(|u| {
                    let name = u.display_name.as_deref()?;
                    Some((u.user_id.as_str(), name))
                })
                .collect();

            // Build roster_id → owner_id for this season
            let roster_owner: HashMap<u32, &str> = rosters
                .iter()
                .filter_map(|r| r.owner_id.as_deref().map(|oid| (r.roster_id, oid)))
                .collect();

            // Accumulate per-user stats from rosters
            for roster in &rosters {
                let owner_id = match roster.owner_id.as_deref() {
                    Some(id) => id,
                    None => continue,
                };
                let display_name = user_names
                    .get(owner_id)
                    .copied()
                    .unwrap_or("Unknown")
                    .to_string();
                let settings = match &roster.settings {
                    Some(s) => s,
                    None => continue,
                };

                let entry =
                    stats_map
                        .entry(owner_id.to_string())
                        .or_insert_with(|| AllTimeUserStats {
                            display_name: display_name.clone(),
                            ..Default::default()
                        });
                // Keep display name fresh (most recent season)
                entry.display_name = display_name;
                entry.seasons += 1;
                entry.wins += settings.wins.unwrap_or(0);
                entry.losses += settings.losses.unwrap_or(0);
                entry.points_for += settings.total_points();
                entry.points_against += settings.points_against();
            }

            // Find champion from winners bracket (only for completed seasons)
            if is_complete && let Ok(bracket) = self.get_winners_bracket(&league_id).await {
                // Championship game = highest round, match #1
                if let Some(max_round) = bracket.iter().map(|b| b.r).max()
                    && let Some(champ_match) = bracket
                        .iter()
                        .find(|b| b.r == max_round && b.m == 1 && b.w.is_some())
                    && let Some(winner_roster_id) = champ_match.w
                {
                    let owner_id = roster_owner.get(&winner_roster_id).copied().unwrap_or("");
                    let name = user_names.get(owner_id).copied().unwrap_or("Unknown");
                    champions.push(SeasonChampion {
                        season: season.clone(),
                        display_name: name.to_string(),
                    });
                    // Increment championship count
                    if let Some(entry) = stats_map.get_mut(owner_id) {
                        entry.championships += 1;
                    }
                }
            }

            // Walk back to previous season
            match league.previous_league_id {
                Some(ref prev) if !prev.is_empty() && prev != "0" => {
                    league_id = prev.clone();
                }
                _ => break,
            }
        }

        // Sort champions most-recent first
        champions.sort_by(|a, b| b.season.cmp(&a.season));

        let mut all_time: Vec<AllTimeUserStats> = stats_map.into_values().collect();
        // Sort by wins descending
        all_time.sort_by(|a, b| b.wins.cmp(&a.wins));

        (champions, all_time)
    }

    /// Fetch season-long stats for a given NFL season (e.g. "2025").
    pub async fn get_season_stats(&self, season: &str) -> Result<HashMap<String, PlayerStats>> {
        self.get_json_with_retry(&format!("{BASE_URL}/stats/nfl/regular/{season}"))
            .await
    }

    /// Fetch season-long projections for a given NFL season.
    pub async fn get_season_projections(
        &self,
        season: &str,
    ) -> Result<HashMap<String, PlayerStats>> {
        self.get_json_with_retry(&format!("{BASE_URL}/projections/nfl/regular/{season}"))
            .await
    }

    /// Fetch historical stats for the last N seasons plus current projections.
    /// Returns (season_stats_by_player_id, current_projections_by_player_id).
    pub async fn fetch_player_stats(
        &self,
        current_season: &str,
        history_years: u32,
    ) -> (
        HashMap<String, Vec<PlayerSeasonEntry>>,
        HashMap<String, PlayerStats>,
    ) {
        let current_year: u32 = current_season.parse().unwrap_or(2025);

        // Fetch historical stats
        let mut all_stats: HashMap<String, Vec<PlayerSeasonEntry>> = HashMap::new();
        for year_offset in 0..history_years {
            let year = current_year - year_offset;
            let season = year.to_string();
            match self.get_season_stats(&season).await {
                Ok(stats) => {
                    for (pid, st) in stats {
                        // Skip players with no meaningful points
                        let pts = st.pts_half_ppr.unwrap_or(0.0);
                        if pts < 1.0 {
                            continue;
                        }
                        all_stats.entry(pid).or_default().push(PlayerSeasonEntry {
                            season: season.clone(),
                            stats: st,
                        });
                    }
                    println!("  Loaded {season} stats.");
                }
                Err(e) => {
                    eprintln!("Warning: could not fetch {season} stats: {e}");
                }
            }
        }

        // Fetch current season projections
        let projections = match self.get_season_projections(current_season).await {
            Ok(p) => {
                println!("  Loaded {current_season} projections.");
                p
            }
            Err(e) => {
                eprintln!("Warning: could not fetch {current_season} projections: {e}");
                HashMap::new()
            }
        };

        (all_stats, projections)
    }

    pub async fn get_transactions(&self, league_id: &str, week: u32) -> Result<Vec<Transaction>> {
        self.get_json_with_retry(&format!(
            "{BASE_URL}/league/{league_id}/transactions/{week}"
        ))
        .await
    }

    /// Fetch transactions across all weeks (0 through max_week).
    /// Dynasty/offseason trades can land on any week number, so we scan them all.
    pub async fn get_all_transactions(
        &self,
        league_id: &str,
        max_week: u32,
    ) -> Result<Vec<Transaction>> {
        let mut all = Vec::new();
        for week in 0..=max_week {
            match self.get_transactions(league_id, week).await {
                Ok(txs) => all.extend(txs),
                Err(e) => {
                    // Week may simply have no endpoint (returns 404); skip it
                    eprintln!("Warning: could not fetch week {week} transactions: {e}");
                }
            }
        }
        Ok(all)
    }

    pub async fn load_players(&mut self) -> Result<&HashMap<String, Player>> {
        if let Some(ref players) = self.players {
            return Ok(players);
        }

        let cache_path = Path::new(PLAYER_CACHE_FILE);
        let should_fetch = if cache_path.exists() {
            let metadata = std::fs::metadata(cache_path)?;
            let modified = metadata.modified()?;
            let age = std::time::SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default();
            age.as_secs() > (CACHE_MAX_AGE_DAYS * 86400) as u64
        } else {
            true
        };

        if should_fetch {
            println!("Fetching NFL players (this may take a moment)...");
            let players: HashMap<String, Player> = match self
                .get_json(&format!("{BASE_URL}/players/nfl"))
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    // If cache exists but is stale, try using it anyway
                    if cache_path.exists() {
                        eprintln!(
                            "Warning: failed to fetch fresh player data ({e}), using stale cache"
                        );
                        let data = std::fs::read_to_string(cache_path)?;
                        serde_json::from_str(&data).context("Corrupt player cache")?
                    } else {
                        return Err(e);
                    }
                }
            };
            // Write cache
            if let Ok(json) = serde_json::to_string(&players) {
                let _ = std::fs::write(cache_path, json);
            }
            self.players = Some(players);
        } else {
            let data = match std::fs::read_to_string(cache_path) {
                Ok(d) => d,
                Err(_) => {
                    // Cache unreadable, delete and refetch
                    let _ = std::fs::remove_file(cache_path);
                    return Box::pin(self.load_players()).await;
                }
            };
            let players: HashMap<String, Player> = match serde_json::from_str(&data) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("Warning: corrupt player cache, re-fetching...");
                    let _ = std::fs::remove_file(cache_path);
                    return Box::pin(self.load_players()).await;
                }
            };
            self.players = Some(players);
        }

        Ok(self.players.as_ref().unwrap())
    }

    #[allow(dead_code)]
    pub fn get_cached_players(&self) -> Option<&HashMap<String, Player>> {
        self.players.as_ref()
    }
}

// ─── Helpers ───

/// Build a map of roster_id → display name (e.g. "Nick (Touchdown Tyrants)")
pub fn build_roster_name_map(users: &[User], rosters: &[Roster]) -> HashMap<u32, String> {
    let user_map: HashMap<&str, &User> = users.iter().map(|u| (u.user_id.as_str(), u)).collect();

    rosters
        .iter()
        .filter_map(|r| {
            let owner_id = r.owner_id.as_deref()?;
            let user = user_map.get(owner_id)?;
            let display = user.display_name.as_deref().unwrap_or("Unknown");
            let name = if let Some(ref meta) = user.metadata {
                if let Some(ref team) = meta.team_name {
                    if !team.is_empty() {
                        format!("{display} ({team})")
                    } else {
                        display.to_string()
                    }
                } else {
                    display.to_string()
                }
            } else {
                display.to_string()
            };
            Some((r.roster_id, name))
        })
        .collect()
}

/// Build a map of roster_id → record string
pub fn build_roster_record_map(rosters: &[Roster]) -> HashMap<u32, String> {
    rosters
        .iter()
        .map(|r| {
            let record = r
                .settings
                .as_ref()
                .map(|s| s.record())
                .unwrap_or_else(|| "0-0".to_string());
            (r.roster_id, record)
        })
        .collect()
}

/// Format a player name from ID. Detects D/ST (short all-caps IDs like "DET").
pub fn format_player_name(player_id: &str, players: &HashMap<String, Player>) -> String {
    // Detect D/ST: short string, all uppercase letters
    if player_id.len() <= 4 && player_id.chars().all(|c| c.is_ascii_uppercase()) {
        return format!("{player_id} D/ST");
    }

    match players.get(player_id) {
        Some(player) => {
            let name = player.full_name();
            let pos = player.position.as_deref().unwrap_or("??");
            let team = player.team.as_deref().unwrap_or("FA");
            if name.is_empty() {
                format!("Unknown ({pos} - {team})")
            } else {
                format!("{name} ({pos} - {team})")
            }
        }
        None => format!("Unknown Player ({player_id})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_deserialization() {
        let json = r#"{
            "type": "trade",
            "transaction_id": "434852362033561600",
            "status": "complete",
            "roster_ids": [2, 1],
            "adds": { "4035": 1, "2257": 2 },
            "drops": { "4035": 2, "2257": 1 },
            "draft_picks": [
                {
                    "season": "2025",
                    "round": 3,
                    "roster_id": 2,
                    "previous_owner_id": 2,
                    "owner_id": 1
                }
            ],
            "created": 1558039391576,
            "status_updated": 1558039402803
        }"#;

        let tx: Transaction = serde_json::from_str(json).unwrap();
        assert!(tx.is_completed_trade());
        assert_eq!(tx.id(), "434852362033561600");
        assert_eq!(tx.roster_ids.as_ref().unwrap().len(), 2);

        let adds = tx.adds.as_ref().unwrap();
        assert_eq!(adds.get("4035"), Some(&1));
        assert_eq!(adds.get("2257"), Some(&2));

        let picks = tx.draft_picks.as_ref().unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].round, Some(3));
        assert_eq!(picks[0].owner_id, Some(1));
    }

    #[test]
    fn test_draft_pick_only_trade() {
        let json = r#"{
            "type": "trade",
            "transaction_id": "123",
            "status": "complete",
            "roster_ids": [1, 2],
            "adds": null,
            "drops": null,
            "draft_picks": [
                {
                    "season": "2025",
                    "round": 1,
                    "roster_id": 1,
                    "previous_owner_id": 1,
                    "owner_id": 2
                }
            ],
            "created": 1558039391576
        }"#;

        let tx: Transaction = serde_json::from_str(json).unwrap();
        assert!(tx.is_completed_trade());
        assert!(tx.adds.is_none());
        assert!(tx.drops.is_none());
    }

    #[test]
    fn test_format_player_name_regular() {
        let mut players = HashMap::new();
        players.insert(
            "4035".to_string(),
            Player {
                player_id: Some("4035".to_string()),
                first_name: Some("Jaylen".to_string()),
                last_name: Some("Waddle".to_string()),
                position: Some("WR".to_string()),
                team: Some("MIA".to_string()),
                ..Default::default()
            },
        );

        assert_eq!(
            format_player_name("4035", &players),
            "Jaylen Waddle (WR - MIA)"
        );
    }

    #[test]
    fn test_format_player_name_dst() {
        let players = HashMap::new();
        assert_eq!(format_player_name("DET", &players), "DET D/ST");
        assert_eq!(format_player_name("PHI", &players), "PHI D/ST");
    }

    #[test]
    fn test_format_player_name_unknown() {
        let players = HashMap::new();
        assert_eq!(
            format_player_name("99999", &players),
            "Unknown Player (99999)"
        );
    }

    #[test]
    fn test_roster_settings_record() {
        let settings = RosterSettings {
            wins: Some(8),
            losses: Some(2),
            ties: None,
            fpts: Some(1234),
            fpts_decimal: Some(56),
            fpts_against: Some(1100),
            fpts_against_decimal: Some(0),
        };
        assert_eq!(settings.record(), "8-2");
        assert!((settings.total_points() - 1234.56).abs() < 0.001);
    }

    #[test]
    fn test_non_trade_filtered() {
        let json = r#"{
            "type": "waiver",
            "transaction_id": "456",
            "status": "complete",
            "roster_ids": [1]
        }"#;
        let tx: Transaction = serde_json::from_str(json).unwrap();
        assert!(!tx.is_completed_trade());
    }

    #[test]
    fn test_pending_trade_filtered() {
        let json = r#"{
            "type": "trade",
            "transaction_id": "789",
            "status": "pending",
            "roster_ids": [1, 2]
        }"#;
        let tx: Transaction = serde_json::from_str(json).unwrap();
        assert!(!tx.is_completed_trade());
    }

    fn build_league(
        roster_positions: Option<Vec<&str>>,
        scoring: Option<Vec<(&str, f64)>>,
        league_type: Option<u32>,
        total_rosters: Option<u32>,
    ) -> League {
        let settings = LeagueSettings {
            num_teams: total_rosters,
            playoff_teams: None,
            playoff_week_start: None,
            playoff_type: None,
            trade_deadline: None,
            trade_review_days: None,
            pick_trading: None,
            waiver_type: None,
            waiver_budget: None,
            taxi_slots: None,
            taxi_years: None,
            taxi_allow_vets: None,
            reserve_slots: None,
            bench_lock: None,
            best_ball: None,
            disable_trades: None,
            offseason_adds: None,
            draft_rounds: None,
            league_type,
            position_limit_qb: None,
        };
        League {
            league_id: Some("123".to_string()),
            name: Some("Test League".to_string()),
            season: Some("2026".to_string()),
            status: Some("in_season".to_string()),
            previous_league_id: None,
            total_rosters,
            roster_positions: roster_positions
                .map(|v| v.into_iter().map(|s| s.to_string()).collect()),
            scoring_settings: scoring.map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect()
            }),
            settings: Some(settings),
        }
    }

    #[test]
    fn test_detect_scoring_from_rec_value() {
        assert_eq!(
            build_league(None, Some(vec![("rec", 1.0)]), None, None).detect_scoring(),
            "ppr"
        );
        assert_eq!(
            build_league(None, Some(vec![("rec", 0.5)]), None, None).detect_scoring(),
            "half_ppr"
        );
        assert_eq!(
            build_league(None, Some(vec![("rec", 0.25)]), None, None).detect_scoring(),
            "half_ppr"
        );
        assert_eq!(
            build_league(None, Some(vec![("rec", 0.0)]), None, None).detect_scoring(),
            "std"
        );
        // Falls back to half_ppr when scoring_settings is missing.
        assert_eq!(build_league(None, None, None, None).detect_scoring(), "half_ppr");
    }

    #[test]
    fn test_is_superflex() {
        let sf = build_league(
            Some(vec!["QB", "RB", "RB", "WR", "WR", "TE", "FLEX", "SUPER_FLEX", "K", "DEF"]),
            None,
            None,
            None,
        );
        assert!(sf.is_superflex());

        let std = build_league(
            Some(vec!["QB", "RB", "RB", "WR", "WR", "TE", "FLEX", "K", "DEF"]),
            None,
            None,
            None,
        );
        assert!(!std.is_superflex());
    }

    #[test]
    fn test_format_roster_positions_orders_starters_first() {
        let league = build_league(
            Some(vec![
                "QB", "RB", "RB", "WR", "WR", "WR", "TE", "FLEX", "SUPER_FLEX", "K", "DEF", "BN",
                "BN", "BN", "BN", "IR", "IR",
            ]),
            None,
            None,
            None,
        );
        let formatted = league.format_roster_positions();
        // Starters appear in canonical order, FLEX after SUPER_FLEX, BN/IR last.
        assert_eq!(
            formatted,
            "1 QB, 2 RB, 3 WR, 1 TE, 1 SUPER_FLEX, 1 FLEX, 1 K, 1 DEF, 4 BN, 2 IR"
        );
    }

    #[test]
    fn test_format_roster_positions_handles_unknown_slots() {
        let league = build_league(Some(vec!["QB", "WEIRD_SLOT", "BN"]), None, None, None);
        let formatted = league.format_roster_positions();
        assert!(formatted.starts_with("1 QB"));
        assert!(formatted.contains("WEIRD_SLOT"));
        assert!(formatted.contains("BN"));
    }

    #[test]
    fn test_format_scoring_highlights_full_ppr_with_te_premium() {
        let league = build_league(
            None,
            Some(vec![
                ("rec", 1.0),
                ("pass_td", 6.0),
                ("bonus_rec_te", 0.5),
                ("bonus_rec_yd_100", 3.0),
            ]),
            None,
            None,
        );
        let highlights = league.format_scoring_highlights().unwrap();
        assert!(highlights.contains("full PPR"));
        assert!(highlights.contains("6pt pass TDs"));
        assert!(highlights.contains("+0.5 TE premium"));
        assert!(highlights.contains("100+ rec yds"));
    }

    #[test]
    fn test_format_scoring_highlights_standard_no_extras() {
        let league = build_league(
            None,
            Some(vec![("rec", 0.0), ("pass_td", 4.0)]),
            None,
            None,
        );
        let highlights = league.format_scoring_highlights().unwrap();
        // Standard scoring with default 4pt pass TDs — only the PPR label is emitted.
        assert_eq!(highlights, "standard (no PPR)");
    }

    #[test]
    fn test_format_summary_dynasty_superflex_full_ppr() {
        let league = build_league(
            Some(vec![
                "QB", "RB", "RB", "WR", "WR", "WR", "TE", "FLEX", "SUPER_FLEX", "K", "DEF", "BN",
                "BN", "BN",
            ]),
            Some(vec![("rec", 1.0), ("pass_td", 6.0), ("bonus_rec_te", 0.5)]),
            Some(2),
            Some(10),
        );
        let summary = league.format_summary(None);
        assert!(summary.starts_with("10-team superflex dynasty league."));
        assert!(summary.contains("Lineup: 1 QB, 2 RB, 3 WR, 1 TE, 1 SUPER_FLEX, 1 FLEX, 1 K, 1 DEF, 3 BN."));
        assert!(summary.contains("Scoring: full PPR, 6pt pass TDs, +0.5 TE premium."));
        assert!(!summary.contains("Additional notes"));
    }

    #[test]
    fn test_format_summary_appends_extra_rules() {
        let league = build_league(
            Some(vec!["QB", "RB", "WR", "FLEX", "K", "DEF", "BN"]),
            Some(vec![("rec", 0.5)]),
            Some(0),
            Some(12),
        );
        let summary = league.format_summary(Some("$100 buy-in, payouts top 4."));
        assert!(summary.contains("12-team redraft league."));
        assert!(!summary.contains("superflex"));
        assert!(summary.contains("half PPR"));
        assert!(summary.contains("Additional notes: $100 buy-in, payouts top 4."));
    }

    #[ignore]
    #[tokio::test]
    async fn test_real_sleeper_api() {
        // Uses a known public league for integration testing
        let client = SleeperClient::new();
        let state = client.get_nfl_state().await.unwrap();
        assert!(!state.season.is_empty());
        println!("NFL State: week={}, season={}", state.week, state.season);
    }

    /// Hits the live Sleeper API for the user's configured league
    /// (`SLEEPER_LEAGUE_ID` from the environment / .env) and verifies that
    /// the league-format helpers produce non-trivial output. Run with:
    ///   cargo test --no-fail-fast -- --ignored --nocapture sleeper::tests::test_real_league_format
    #[ignore]
    #[tokio::test]
    async fn test_real_league_format() {
        // Pull league_id from .env / environment, matching how main.rs sources it.
        let _ = dotenvy::dotenv();
        let league_id = match std::env::var("SLEEPER_LEAGUE_ID") {
            Ok(id) if !id.is_empty() => id,
            _ => {
                eprintln!(
                    "Skipping test_real_league_format: SLEEPER_LEAGUE_ID is not set in the environment or .env"
                );
                return;
            }
        };

        let client = SleeperClient::new();
        let league = client
            .get_league(&league_id)
            .await
            .expect("get_league should succeed for a real league_id");

        // The league should round-trip with at least the basic identity fields populated.
        assert_eq!(
            league.league_id.as_deref(),
            Some(league_id.as_str()),
            "round-tripped league_id should match the request"
        );
        assert!(
            league.name.as_deref().is_some_and(|n| !n.is_empty()),
            "league.name should be populated by the API"
        );

        // Roster positions should be present and non-empty for any real league.
        let positions = league
            .roster_positions
            .as_ref()
            .expect("roster_positions should be present on a real league");
        assert!(
            !positions.is_empty(),
            "roster_positions should not be empty"
        );

        // Scoring settings should also come back populated.
        let scoring_settings = league
            .scoring_settings
            .as_ref()
            .expect("scoring_settings should be present on a real league");
        assert!(
            !scoring_settings.is_empty(),
            "scoring_settings should not be empty"
        );

        // detect_scoring must return one of the three known formats.
        let scoring = league.detect_scoring();
        assert!(
            matches!(scoring, "ppr" | "half_ppr" | "std"),
            "detect_scoring returned unexpected value: {scoring}"
        );

        // The roster format should mention at least one canonical starter slot
        // and have a digit count prefix (e.g. "1 QB").
        let roster_fmt = league.format_roster_positions();
        assert!(!roster_fmt.is_empty(), "roster format should not be empty");
        assert_ne!(
            roster_fmt, "Unknown roster format",
            "roster format should be derived from real data"
        );
        assert!(
            roster_fmt.contains("QB")
                || roster_fmt.contains("RB")
                || roster_fmt.contains("WR")
                || roster_fmt.contains("TE"),
            "roster format should mention at least one offensive starter slot, got: {roster_fmt}"
        );

        // The full summary should include the team count, lineup, and scoring.
        let summary = league.format_summary(None);
        assert!(summary.contains("Lineup:"), "summary missing Lineup: {summary}");
        assert!(summary.contains("Scoring:"), "summary missing Scoring: {summary}");

        // Sanity-check the superflex flag against the raw positions.
        let raw_has_sf = positions.iter().any(|p| {
            let u = p.to_uppercase();
            u == "SUPER_FLEX" || u == "SF" || u == "Q-FLEX" || u == "QFLEX"
        });
        assert_eq!(league.is_superflex(), raw_has_sf);
        if raw_has_sf {
            assert!(
                summary.contains("superflex"),
                "summary should mention superflex when SF slot exists, got: {summary}"
            );
        }

        // Print everything so a developer running with --nocapture can eyeball it.
        println!("League: {}", league.name.as_deref().unwrap_or(""));
        println!("  Detected scoring: {scoring}");
        println!("  Roster format:    {roster_fmt}");
        println!("  Superflex:        {}", league.is_superflex());
        if let Some(highlights) = league.format_scoring_highlights() {
            println!("  Scoring highlights: {highlights}");
        }
        println!("  Full summary:     {summary}");
    }
}

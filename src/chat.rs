use std::collections::HashMap;

use crate::news;
use crate::sleeper::{AllTimeUserStats, Player, Roster, SeasonChampion, Transaction, User};

const BOT_USERNAME: &str = "tradegimp210";

/// Check if a message text mentions the bot's username.
pub fn is_mention(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains(&format!("@{BOT_USERNAME}")) || lower.contains(BOT_USERNAME)
}

/// Strip the bot mention from the message to get the actual question.
pub fn strip_mention(text: &str) -> String {
    let result = text
        .replace(&format!("@{BOT_USERNAME}"), "")
        .replace(BOT_USERNAME, "");
    result.trim().to_string()
}

/// Build a league context string with standings, points, roster info, recent transactions, and history.
pub fn build_league_context(
    users: &[User],
    rosters: &[Roster],
    players: &HashMap<String, Player>,
    recent_transactions: &[Transaction],
    roster_names: &HashMap<u32, String>,
    champions: &[SeasonChampion],
    all_time_stats: &[AllTimeUserStats],
) -> String {
    let user_map: HashMap<&str, &User> = users.iter().map(|u| (u.user_id.as_str(), u)).collect();

    let mut standings: Vec<(String, String, f64, f64, Vec<String>)> = Vec::new();

    for roster in rosters {
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

        // Get top starters
        let starters: Vec<String> = roster
            .starters
            .as_ref()
            .map(|ids| {
                ids.iter()
                    .filter(|id| *id != "0")
                    .take(5)
                    .filter_map(|id| {
                        players.get(id).map(|p| {
                            let pos = p.position.as_deref().unwrap_or("??");
                            format!("{} ({})", p.full_name(), pos)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        standings.push((name, record, points, pts_against, starters));
    }

    // Sort by wins descending, then points as tiebreaker
    standings.sort_by(|a, b| {
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

    let mut ctx = String::from("LEAGUE STANDINGS:\n");
    for (i, (name, record, points, pts_against, starters)) in standings.iter().enumerate() {
        ctx.push_str(&format!(
            "  {}. {name} ({record}, {points:.1} PF, {pts_against:.1} PA)",
            i + 1
        ));
        if !starters.is_empty() {
            ctx.push_str(&format!(" - Key players: {}", starters.join(", ")));
        }
        ctx.push('\n');
    }

    // Recent transactions (last 10)
    if !recent_transactions.is_empty() {
        ctx.push_str("\nRECENT TRANSACTIONS:\n");
        let cutoff_ms = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            now.saturating_sub(7 * 24 * 60 * 60 * 1000)
        };

        let mut shown = 0;
        for tx in recent_transactions.iter() {
            if shown >= 10 {
                break;
            }
            if tx.created.unwrap_or(0) < cutoff_ms {
                continue;
            }
            let tx_type = tx.tx_type.as_deref().unwrap_or("unknown");
            let team = tx
                .roster_ids
                .as_ref()
                .and_then(|ids| ids.first())
                .and_then(|id| roster_names.get(id))
                .map(|s| s.as_str())
                .unwrap_or("Unknown");

            match tx_type {
                "trade" => {
                    let teams: Vec<&str> = tx
                        .roster_ids
                        .as_ref()
                        .map(|ids| {
                            ids.iter()
                                .filter_map(|id| roster_names.get(id).map(|s| s.as_str()))
                                .collect()
                        })
                        .unwrap_or_default();
                    ctx.push_str(&format!(
                        "  Trade: {} completed a trade\n",
                        teams.join(" and ")
                    ));
                }
                "waiver" | "free_agent" => {
                    let adds: Vec<String> = tx
                        .adds
                        .as_ref()
                        .map(|m| {
                            m.keys()
                                .filter_map(|pid| players.get(pid))
                                .map(|p| p.full_name())
                                .collect()
                        })
                        .unwrap_or_default();
                    let drops: Vec<String> = tx
                        .drops
                        .as_ref()
                        .map(|m| {
                            m.keys()
                                .filter_map(|pid| players.get(pid))
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
                    ctx.push_str(&line);
                    ctx.push('\n');
                }
                _ => {}
            }
            shown += 1;
        }
    }

    // Historical champions
    if !champions.is_empty() {
        ctx.push_str("\nLEAGUE CHAMPIONS:\n");
        for champ in champions {
            ctx.push_str(&format!("  {} - {}\n", champ.season, champ.display_name));
        }
    }

    // All-time stats
    if !all_time_stats.is_empty() {
        ctx.push_str("\nALL-TIME STATS:\n");
        for s in all_time_stats {
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
            ctx.push_str(&format!(
                "  {} - {}-{} ({win_pct:.0}%), {:.1} PF, {:.1} PA{champ_str}\n",
                s.display_name, s.wins, s.losses, s.points_for, s.points_against
            ));
        }
    }

    ctx
}

/// Search Google News for context relevant to the user's question.
pub async fn search_for_context(query: &str) -> String {
    if query.trim().is_empty() {
        return String::new();
    }

    // Add "NFL fantasy football" to improve relevance
    let search_query = format!("{query} NFL fantasy football");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    match fetch_google_news_rss(&client, &search_query).await {
        Ok(results) if !results.is_empty() => {
            format!("SEARCH RESULTS:\n{results}")
        }
        _ => String::new(),
    }
}

/// Fetch headlines from Google News RSS.
async fn fetch_google_news_rss(client: &reqwest::Client, query: &str) -> anyhow::Result<String> {
    let url = format!(
        "https://news.google.com/rss/search?q={}&hl=en-US&gl=US&ceid=US:en",
        news::urlencode(query)
    );

    let resp = client.get(&url).send().await?;
    let body = resp.text().await?;
    let headlines = extract_rss_titles(&body);

    if headlines.is_empty() {
        return Ok(String::new());
    }

    let summary = headlines
        .into_iter()
        .take(5)
        .enumerate()
        .map(|(i, h)| format!("  {}. {}", i + 1, h))
        .collect::<Vec<_>>()
        .join("\n");

    Ok(summary)
}

/// Extract titles from RSS XML `<item>` elements.
fn extract_rss_titles(xml: &str) -> Vec<String> {
    let mut titles = Vec::new();
    let mut rest = xml;
    while let Some(item_start) = rest.find("<item>") {
        if titles.len() >= 8 {
            break;
        }
        rest = &rest[item_start + 6..];
        let item_end = rest.find("</item>").unwrap_or(rest.len());
        let item_body = &rest[..item_end];

        if let Some(title_start) = item_body.find("<title>") {
            let after_tag = &item_body[title_start + 7..];
            if let Some(title_end) = after_tag.find("</title>") {
                let title = after_tag[..title_end]
                    .replace("&amp;", "&")
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&#39;", "'");
                let title = title.trim().to_string();
                if !title.is_empty() {
                    titles.push(title);
                }
            }
        }

        rest = &rest[item_end..];
    }
    titles
}

/// Build the full prompt for the LLM to respond to a chat mention.
pub fn build_chat_prompt(
    author_name: &str,
    message: &str,
    league_context: &str,
    search_results: &str,
) -> String {
    let today = chrono::Local::now().format("%B %d, %Y");
    let mut prompt = format!(
        "Today's date: {today}\n\n\
         {author_name} tagged you in the league chat and said:\n\
         \"{message}\"\n\n\
         {league_context}\n"
    );

    if !search_results.is_empty() {
        prompt.push_str(search_results);
        prompt.push('\n');
    }

    prompt.push_str("\nRespond to their message. Use the league data and search results above. If they're asking about a specific player, team, or matchup, give your honest take — don't hold back.");
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_mention() {
        assert!(is_mention("hey @tradegimp210 who should I start?"));
        assert!(is_mention("@tradegimp210"));
        assert!(is_mention("yo tradegimp210 what do you think?"));
        assert!(!is_mention("hey guys what's up"));
    }

    #[test]
    fn test_strip_mention() {
        assert_eq!(
            strip_mention("@tradegimp210 who should I start at flex?"),
            "who should I start at flex?"
        );
        assert_eq!(
            strip_mention("hey @tradegimp210 thoughts on this trade?"),
            "hey  thoughts on this trade?"
        );
    }
}

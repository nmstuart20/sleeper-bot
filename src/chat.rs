use std::collections::HashMap;

use crate::news;
use crate::sleeper::{Player, Roster, User};

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

/// Build a league context string with standings and roster info for the LLM.
pub fn build_league_context(
    users: &[User],
    rosters: &[Roster],
    players: &HashMap<String, Player>,
) -> String {
    let user_map: HashMap<&str, &User> = users.iter().map(|u| (u.user_id.as_str(), u)).collect();

    let mut standings: Vec<(String, String, Vec<String>)> = Vec::new();

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

        let record = roster
            .settings
            .as_ref()
            .map(|s| s.record())
            .unwrap_or_else(|| "0-0".to_string());

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

        standings.push((name, record, starters));
    }

    // Sort by wins descending
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
        b_wins.cmp(&a_wins)
    });

    let mut ctx = String::from("LEAGUE STANDINGS:\n");
    for (name, record, starters) in &standings {
        ctx.push_str(&format!("  {name} ({record})"));
        if !starters.is_empty() {
            ctx.push_str(&format!(" - Key players: {}", starters.join(", ")));
        }
        ctx.push('\n');
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

    prompt.push_str("\nRespond to their message. Be helpful but stay in character as Jon Gruden. If they're asking about a specific player, team, or matchup, give your honest take.");
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

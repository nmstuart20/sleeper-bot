mod anthropic;
mod chat;
mod config;
mod gemini;
mod graphql;
mod llm;
mod news;
mod sleeper;
mod state;
mod trade_analyzer;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::collections::HashMap;

use crate::anthropic::AnthropicClient;
use crate::gemini::GeminiClient;
use crate::graphql::SleeperGraphql;
use crate::llm::TradeAnalyzer;
use crate::sleeper::{Player, SleeperClient};
use crate::state::{ChatState, ReviewState};

#[derive(Clone, ValueEnum)]
enum LlmProvider {
    Anthropic,
    Gemini,
}

#[derive(Parser)]
#[command(name = "sleeper-trade-bot")]
enum Cli {
    /// Check for new trades once and exit (cron-friendly)
    Check {
        #[arg(long, env = "SLEEPER_LEAGUE_ID")]
        league: String,
        /// Post reviews to Sleeper chat. Without this, terminal output only.
        #[arg(long)]
        post: bool,
        /// LLM provider to use for analysis
        #[arg(long, value_enum, default_value = "anthropic", env = "LLM_PROVIDER")]
        provider: LlmProvider,
        /// Only process trades from the last N days
        #[arg(long, default_value = "3")]
        days: u64,
        /// Character persona for trade analysis (e.g. "Donald Trump", "Jon Gruden", "Barack Obama")
        #[arg(long, default_value = "Donald Trump", env = "BOT_CHARACTER")]
        character: String,
    },
    /// Watch for trades and chat @mentions continuously
    Watch {
        #[arg(long, env = "SLEEPER_LEAGUE_ID")]
        league: String,
        /// Trade poll interval in seconds
        #[arg(long, default_value = "20")]
        interval: u64,
        /// Chat poll interval in seconds
        #[arg(long, default_value = "20")]
        chat_interval: u64,
        /// LLM provider to use for analysis
        #[arg(long, value_enum, default_value = "anthropic", env = "LLM_PROVIDER")]
        provider: LlmProvider,
        /// Only process trades from the last N days
        #[arg(long, default_value = "3")]
        days: u64,
        /// Character persona for trade analysis (e.g. "Donald Trump", "Jon Gruden", "Barack Obama")
        #[arg(long, default_value = "Donald Trump", env = "BOT_CHARACTER")]
        character: String,
    },
    /// Debug the GraphQL connection
    Debug {
        /// Verify the token is valid and check expiry
        #[arg(long)]
        check_token: bool,
        /// Send a test message to the league chat
        #[arg(long)]
        send: Option<String>,
        /// Test the chat AI with a question (prints response, does not post)
        #[arg(long)]
        chat: Option<String>,
        #[arg(long, env = "SLEEPER_LEAGUE_ID")]
        league: Option<String>,
        /// LLM provider to use for --chat test
        #[arg(long, value_enum, default_value = "anthropic", env = "LLM_PROVIDER")]
        provider: LlmProvider,
        /// Character persona for trade analysis
        #[arg(long, default_value = "Donald Trump", env = "BOT_CHARACTER")]
        character: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    let config_path = std::path::Path::new("config.toml");
    let config = config::Config::load(config_path)?;
    let league_rules = &config.league.rules;

    match cli {
        Cli::Check {
            league,
            post,
            provider,
            days,
            character,
        } => {
            let analyzer = build_analyzer(&provider, &character, league_rules)?;
            run_check(&league, post, days, analyzer.as_ref()).await
        }
        Cli::Watch {
            league,
            interval,
            chat_interval,
            provider,
            days,
            character,
        } => {
            let analyzer = build_analyzer(&provider, &character, league_rules)?;
            run_watch(&league, interval, chat_interval, days, league_rules, &config.league.scoring, analyzer.as_ref()).await
        }
        Cli::Debug {
            check_token,
            send,
            chat,
            league,
            provider,
            character,
        } => {
            let analyzer = if chat.is_some() {
                Some(build_analyzer(&provider, &character, league_rules)?)
            } else {
                None
            };
            run_debug(check_token, send, chat, league, league_rules, &config.league.scoring, analyzer.as_deref()).await
        }
    }
}

fn build_analyzer(provider: &LlmProvider, character: &str, league_rules: &str) -> Result<Box<dyn TradeAnalyzer>> {
    let trade_prompt = llm::trade_system_prompt(character, league_rules);
    println!("Character persona: {character}");
    match provider {
        LlmProvider::Anthropic => {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
            println!("Using Anthropic (Claude) for trade analysis.");
            Ok(Box::new(AnthropicClient::new(key, trade_prompt)))
        }
        LlmProvider::Gemini => {
            let key = std::env::var("GEMINI_API_KEY")
                .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY not set"))?;
            println!("Using Gemini for trade analysis.");
            Ok(Box::new(GeminiClient::new(key, trade_prompt)))
        }
    }
}

async fn run_check(
    league_id: &str,
    post: bool,
    days: u64,
    analyzer: &dyn TradeAnalyzer,
) -> Result<()> {
    let mut sleeper = SleeperClient::new();
    let mut review_state = ReviewState::load()?;

    let gql = if post {
        match setup_graphql() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("Warning: GraphQL setup failed ({e}). Continuing in terminal-only mode.");
                None
            }
        }
    } else {
        None
    };

    process_trades(
        league_id,
        &mut sleeper,
        analyzer,
        gql.as_ref(),
        &mut review_state,
        days,
    )
    .await
}

async fn run_watch(
    league_id: &str,
    trade_interval: u64,
    chat_interval: u64,
    days: u64,
    league_rules: &str,
    scoring: &str,
    analyzer: &dyn TradeAnalyzer,
) -> Result<()> {
    let gql = match setup_graphql() {
        Ok(g) => g,
        Err(e) => {
            anyhow::bail!("GraphQL setup failed: {e}");
        }
    };

    println!(
        "Watching league {league_id} — trades every {trade_interval}s, chat every {chat_interval}s. Press Ctrl+C to stop."
    );

    let trade_loop = trade_poll_loop(league_id, trade_interval, days, analyzer, &gql);
    let chat_loop = chat_poll_loop(league_id, chat_interval, league_rules, scoring, analyzer, &gql);

    // Run both loops concurrently — if either returns an error, report it
    tokio::select! {
        result = trade_loop => {
            if let Err(e) = result {
                eprintln!("Trade watcher exited with error: {e}");
            }
        }
        result = chat_loop => {
            if let Err(e) = result {
                eprintln!("Chat watcher exited with error: {e}");
            }
        }
    }

    Ok(())
}

async fn trade_poll_loop(
    league_id: &str,
    interval: u64,
    days: u64,
    analyzer: &dyn TradeAnalyzer,
    gql: &SleeperGraphql,
) -> Result<()> {
    let mut sleeper = SleeperClient::new();
    let mut review_state = ReviewState::load()?;

    loop {
        if let Err(e) = process_trades(
            league_id,
            &mut sleeper,
            analyzer,
            Some(gql),
            &mut review_state,
            days,
        )
        .await
        {
            eprintln!("Error during trade poll: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    }
}

async fn chat_poll_loop(
    league_id: &str,
    interval: u64,
    league_rules: &str,
    scoring: &str,
    analyzer: &dyn TradeAnalyzer,
    gql: &SleeperGraphql,
) -> Result<()> {
    let bot_user_id = gql.bot_user_id();

    if let Some(ref id) = bot_user_id {
        println!("Bot user ID: {id}");
    } else {
        eprintln!(
            "Warning: could not extract bot user_id from token. Bot may reply to its own messages."
        );
    }

    let mut chat_state = ChatState::load()?;
    let mut sleeper = SleeperClient::new();

    // Pre-load league data
    println!("Loading league data for chat...");
    let users = sleeper.get_users(league_id).await?;
    let rosters = sleeper.get_rosters(league_id).await?;
    let players: HashMap<String, sleeper::Player> = sleeper.load_players().await?.clone();
    let roster_names = sleeper::build_roster_name_map(&users, &rosters);

    let nfl_state = sleeper.get_nfl_state().await?;
    let max_week = std::cmp::max(nfl_state.week, 1);
    let recent_transactions = sleeper
        .get_all_transactions(league_id, max_week)
        .await
        .unwrap_or_default();
    println!("Loading league history...");
    let (champions, all_time_stats) = sleeper.fetch_league_history(league_id).await;

    println!("Loading player stats and projections...");
    let (historical_stats, projections) = sleeper
        .fetch_player_stats(&nfl_state.season, 3)
        .await;

    let league_context = chat::build_league_context(
        &users,
        &rosters,
        &players,
        &recent_transactions,
        &roster_names,
        &champions,
        &all_time_stats,
        &projections,
        scoring,
    );

    loop {
        match gql.fetch_messages(league_id, None).await {
            Ok(messages) => {
                for msg in &messages {
                    let msg_id = match msg.message_id.as_deref() {
                        Some(id) => id,
                        None => continue,
                    };

                    if chat_state.has_responded(msg_id) {
                        continue;
                    }

                    let text = msg.text.as_deref().unwrap_or("");

                    // Skip system-generated messages and bot messages
                    if msg.author_is_bot.unwrap_or(false) {
                        chat_state.mark_responded(msg_id)?;
                        continue;
                    }

                    // Skip messages from the bot itself
                    if let (Some(bot_id), Some(author_id)) = (&bot_user_id, &msg.author_id)
                        && bot_id == author_id
                    {
                        chat_state.mark_responded(msg_id)?;
                        continue;
                    }

                    if !chat::is_mention(text) {
                        continue;
                    }

                    let author = msg.author_display_name.as_deref().unwrap_or("Someone");
                    let question = chat::strip_mention(text);

                    println!("\nMention from {author}: \"{text}\"");

                    println!("Looking up mentioned players...");
                    let player_context = chat::find_mentioned_players(
                        &question,
                        &players,
                        &historical_stats,
                        &projections,
                        scoring,
                    );

                    println!("Searching for context...");
                    let search_results = chat::search_for_context(&question).await;

                    let prompt = chat::build_chat_prompt(
                        author,
                        &question,
                        &league_context,
                        &search_results,
                        &player_context,
                    );

                    println!("Generating response...");
                    let chat_sys = llm::chat_system_prompt(league_rules);
                    match analyzer.generate(&chat_sys, &prompt).await {
                        Ok(response) => {
                            println!("Response: {response}");
                            match gql.send_message(league_id, &response).await {
                                Ok(()) => println!("Posted reply to league chat!"),
                                Err(e) => eprintln!("Failed to post reply: {e}"),
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to generate response: {e}");
                        }
                    }

                    chat_state.mark_responded(msg_id)?;
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
            Err(e) => {
                eprintln!("Error fetching messages: {e}");
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    }
}

async fn run_debug(
    check_token: bool,
    send: Option<String>,
    chat_question: Option<String>,
    league: Option<String>,
    league_rules: &str,
    scoring: &str,
    analyzer: Option<&dyn TradeAnalyzer>,
) -> Result<()> {
    if check_token {
        println!("Checking SLEEPER_TOKEN...");
        let gql = setup_graphql()?;
        println!("Authenticated: {}", gql.is_authenticated());

        if let (Some(msg), Some(league_id)) = (send, league) {
            println!("Sending test message to league {league_id}...");
            gql.send_message(&league_id, &msg).await?;
            println!("Message sent successfully!");
        }
    } else if let Some(question) = chat_question {
        let league_id = league.ok_or_else(|| anyhow::anyhow!("--league required with --chat"))?;
        let analyzer = analyzer.ok_or_else(|| anyhow::anyhow!("analyzer not initialized"))?;

        println!("Loading league data...");
        let mut sleeper = SleeperClient::new();
        let users = sleeper.get_users(&league_id).await?;
        let rosters = sleeper.get_rosters(&league_id).await?;
        let players: HashMap<String, sleeper::Player> = sleeper.load_players().await?.clone();
        let roster_names = sleeper::build_roster_name_map(&users, &rosters);

        let nfl_state = sleeper.get_nfl_state().await?;
        let max_week = std::cmp::max(nfl_state.week, 1);
        let recent_transactions = sleeper
            .get_all_transactions(&league_id, max_week)
            .await
            .unwrap_or_default();

        println!("Loading league history...");
        let (champions, all_time_stats) = sleeper.fetch_league_history(&league_id).await;

        println!("Loading player stats and projections...");
        let (historical_stats, projections) = sleeper
            .fetch_player_stats(&nfl_state.season, 3)
            .await;

        let league_context = chat::build_league_context(
            &users,
            &rosters,
            &players,
            &recent_transactions,
            &roster_names,
            &champions,
            &all_time_stats,
            &projections,
            scoring,
        );

        println!("\n--- League Context ---\n{league_context}\n---\n");

        println!("Looking up mentioned players...");
        let player_context = chat::find_mentioned_players(
            &question,
            &players,
            &historical_stats,
            &projections,
            scoring,
        );
        if !player_context.is_empty() {
            println!("{player_context}");
        }

        println!("Searching for context on: \"{question}\"");
        let search_results = chat::search_for_context(&question).await;
        if !search_results.is_empty() {
            println!("Search results found.\n");
        }

        let prompt =
            chat::build_chat_prompt("debug_user", &question, &league_context, &search_results, &player_context);

        println!("Generating response...");
        let chat_sys = llm::chat_system_prompt(league_rules);
        let response = analyzer.generate(&chat_sys, &prompt).await?;
        println!("\n--- Chat Response ---\n{response}\n---");
    } else if let Some(msg) = send {
        let league_id = league.ok_or_else(|| anyhow::anyhow!("--league required with --send"))?;
        let gql = setup_graphql()?;
        println!("Sending test message to league {league_id}...");
        gql.send_message(&league_id, &msg).await?;
        println!("Message sent successfully!");
    } else {
        println!(
            "Use --check-token to verify your token, --send <msg> --league <id> to send a test message, or --chat <question> --league <id> to test the chat AI."
        );
    }
    Ok(())
}

fn setup_graphql() -> Result<SleeperGraphql> {
    let token = std::env::var("SLEEPER_TOKEN").map_err(|_| {
        anyhow::anyhow!(
            "SLEEPER_TOKEN is not set.\n\
            To get your token: log into sleeper.app in your browser → \
            DevTools (F12) → Application → Local Storage → copy your token.\n\
            Then add SLEEPER_TOKEN=<your_token> to your .env file."
        )
    })?;

    SleeperGraphql::new(token)
}

async fn process_trades(
    league_id: &str,
    sleeper: &mut SleeperClient,
    analyzer: &dyn TradeAnalyzer,
    gql: Option<&SleeperGraphql>,
    review_state: &mut ReviewState,
    days: u64,
) -> Result<()> {
    let nfl_state = sleeper.get_nfl_state().await?;
    println!(
        "NFL {} {} - Week {}",
        nfl_state.season, nfl_state.season_type, nfl_state.week
    );

    let users = sleeper.get_users(league_id).await?;
    let rosters = sleeper.get_rosters(league_id).await?;
    let roster_names = sleeper::build_roster_name_map(&users, &rosters);
    let roster_records = sleeper::build_roster_record_map(&rosters);

    let players: HashMap<String, Player> = sleeper.load_players().await?.clone();

    // Scan all weeks (0 through 18) to catch offseason/dynasty trades.
    let max_week = std::cmp::max(nfl_state.week, 18);
    println!("Scanning weeks 0-{max_week} for trades from the last {days} day(s)...");
    let transactions = sleeper.get_all_transactions(league_id, max_week).await?;

    // Only consider trades created within the last N days (created is ms timestamp)
    let cutoff_ms = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        now.saturating_sub(days * 24 * 60 * 60 * 1000)
    };

    let trades: Vec<_> = transactions
        .iter()
        .filter(|tx| {
            tx.is_completed_trade()
                && !review_state.is_reviewed(tx.id())
                && tx.created.unwrap_or(0) >= cutoff_ms
        })
        .collect();

    if trades.is_empty() {
        println!("No new trades found.");
        return Ok(());
    }

    println!("Found {} new trade(s)!", trades.len());

    for trade in trades {
        let tx_id = trade.id().to_string();
        println!("\n--- Trade {tx_id} ---");

        let summary =
            match trade_analyzer::parse_trade(trade, &roster_names, &roster_records, &players) {
                Some(s) => s,
                None => {
                    eprintln!("Could not parse trade {tx_id}, skipping.");
                    continue;
                }
            };

        // Fetch recent news for players in the trade
        println!("Fetching recent news for players...");
        let news_by_id = news::fetch_player_news(&summary.player_ids, &players).await;

        // Convert news map from player_id keys to display name keys
        let mut player_news = std::collections::HashMap::new();
        for (pid, news_text) in &news_by_id {
            let name = sleeper::format_player_name(pid, &players);
            player_news.insert(name, news_text.clone());
        }

        let prompt = trade_analyzer::build_prompt(&summary, &player_news);
        println!("\nTrade details:\n{prompt}\n");

        println!("Generating analysis...");
        let analysis = match analyzer.analyze_trade(&prompt).await {
            Ok(a) => a,
            Err(e) => {
                eprintln!("Failed to analyze trade {tx_id}: {e}");
                continue;
            }
        };

        println!("\n{analysis}\n");

        if let Some(graphql) = gql {
            println!("Posting to league chat...");
            match graphql.send_message(league_id, &analysis).await {
                Ok(()) => {
                    println!("Posted to league chat!");
                }
                Err(e) => {
                    eprintln!("Warning: failed to post to chat ({e}). Review printed above.");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        review_state.mark_reviewed(&tx_id)?;
    }

    Ok(())
}

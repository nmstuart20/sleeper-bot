mod agent;
mod chat;
mod config;
mod gemini_agent;
mod graphql;
mod llm;
mod news;
mod sleeper;
mod state;
mod tools;
mod trade_analyzer;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::agent::ChatAgent;
use crate::gemini_agent::GeminiChatAgent;
use crate::graphql::SleeperGraphql;
use crate::sleeper::{Player, SleeperClient};
use crate::state::{ChatState, ReviewState};
use crate::tools::ToolExecutor;

#[derive(Clone, ValueEnum)]
enum LlmProvider {
    Anthropic,
    Gemini,
}

const LEAGUE_DATA_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

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
        #[arg(long, default_value = "10")]
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
    let extra_rules = config.league.rules.as_deref();
    let scoring_override = config.league.scoring.as_deref();

    match cli {
        Cli::Check {
            league,
            post,
            provider,
            days,
            character,
        } => {
            println!("Character persona: {character}");
            run_check(
                &league,
                post,
                days,
                &provider,
                &character,
                extra_rules,
                scoring_override,
            )
            .await
        }
        Cli::Watch {
            league,
            interval,
            chat_interval,
            provider,
            days,
            character,
        } => {
            println!("Character persona: {character}");
            let watch_config = WatchConfig {
                chat_interval,
                trade_interval: interval,
                days,
            };
            run_watch(
                &league,
                watch_config,
                extra_rules,
                scoring_override,
                &config.league.bot_username,
                &provider,
                &character,
            )
            .await
        }
        Cli::Debug {
            check_token,
            send,
            chat,
            league,
            provider,
            character: _,
        } => {
            run_debug(
                check_token,
                send,
                chat,
                league,
                extra_rules,
                scoring_override,
                &provider,
            )
            .await
        }
    }
}

fn build_agent_runner(
    provider: &LlmProvider,
    system_prompt: &str,
    league: Option<&sleeper::League>,
) -> Result<AgentRunner> {
    match provider {
        LlmProvider::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
            Ok(AgentRunner::Anthropic(ChatAgent::new(
                api_key,
                system_prompt.to_string(),
                league,
            )))
        }
        LlmProvider::Gemini => {
            let api_key = std::env::var("GEMINI_API_KEY")
                .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY not set"))?;
            Ok(AgentRunner::Gemini(GeminiChatAgent::new(
                api_key,
                system_prompt.to_string(),
                league,
            )))
        }
    }
}

async fn run_check(
    league_id: &str,
    post: bool,
    days: u64,
    provider: &LlmProvider,
    character: &str,
    extra_rules: Option<&str>,
    scoring_override: Option<&str>,
) -> Result<()> {
    let mut sleeper = SleeperClient::new();
    let mut review_state = ReviewState::load()?;
    println!("Loading league data for trade check...");
    let league_data = load_league_data(&mut sleeper, league_id).await?;

    let scoring = scoring_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| league_data.league.detect_scoring().to_string());
    let league_format = league_data.league.format_summary(extra_rules);
    println!("Detected league format: {league_format}");

    let agent = build_agent_runner(
        provider,
        &llm::trade_system_prompt(character, &league_format),
        Some(&league_data.league),
    )?;

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
        &league_data,
        &agent,
        gql.as_ref(),
        &mut review_state,
        days,
        &scoring,
    )
    .await
}

struct WatchConfig {
    trade_interval: u64,
    chat_interval: u64,
    days: u64,
}

struct LeagueData {
    league: sleeper::League,
    users: Vec<sleeper::User>,
    rosters: Vec<sleeper::Roster>,
    roster_names: HashMap<u32, String>,
    roster_records: HashMap<u32, String>,
    players: HashMap<String, Player>,
    nfl_state: sleeper::NflState,
    historical_stats: HashMap<String, Vec<sleeper::PlayerSeasonEntry>>,
    projections: HashMap<String, sleeper::PlayerStats>,
    champions: Vec<sleeper::SeasonChampion>,
    all_time_stats: Vec<sleeper::AllTimeUserStats>,
    recent_transactions: Vec<sleeper::Transaction>,
}

async fn load_league_data(sleeper: &mut SleeperClient, league_id: &str) -> Result<LeagueData> {
    let league = sleeper.get_league(league_id).await?;
    let users = sleeper.get_users(league_id).await?;
    let rosters = sleeper.get_rosters(league_id).await?;
    let roster_names = sleeper::build_roster_name_map(&users, &rosters);
    let roster_records = sleeper::build_roster_record_map(&rosters);
    let players = sleeper.load_players().await?.clone();

    let nfl_state = sleeper.get_nfl_state().await?;
    let max_week = std::cmp::max(nfl_state.week, 1);
    let recent_transactions = sleeper
        .get_all_transactions(league_id, max_week)
        .await
        .unwrap_or_default();

    let (champions, all_time_stats) = sleeper.fetch_league_history(league_id).await;
    let (historical_stats, projections) = sleeper.fetch_player_stats(&nfl_state.season, 3).await;

    Ok(LeagueData {
        league,
        users,
        rosters,
        roster_names,
        roster_records,
        players,
        nfl_state,
        historical_stats,
        projections,
        champions,
        all_time_stats,
        recent_transactions,
    })
}

async fn run_watch(
    league_id: &str,
    watch_config: WatchConfig,
    extra_rules: Option<&str>,
    scoring_override: Option<&str>,
    bot_username: &str,
    provider: &LlmProvider,
    character: &str,
) -> Result<()> {
    let gql = match setup_graphql() {
        Ok(g) => g,
        Err(e) => {
            anyhow::bail!("GraphQL setup failed: {e}");
        }
    };

    // One upfront API call so we can derive scoring + roster format before
    // building agents. The poll loops will reload the full league data on
    // their own refresh interval.
    let sleeper_probe = SleeperClient::new();
    let initial_league = sleeper_probe.get_league(league_id).await?;
    let scoring = scoring_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| initial_league.detect_scoring().to_string());
    let league_format = initial_league.format_summary(extra_rules);
    println!("Detected league format: {league_format}");

    let trade_agent = build_agent_runner(
        provider,
        &llm::trade_system_prompt(character, &league_format),
        Some(&initial_league),
    )?;

    let trade_interval = watch_config.trade_interval;
    let chat_interval = watch_config.chat_interval;
    let days = watch_config.days;

    println!(
        "Watching league {league_id} — trades every {trade_interval}s, chat every {chat_interval}s. Press Ctrl+C to stop."
    );

    let trade_loop = trade_poll_loop(
        league_id,
        trade_interval,
        days,
        &trade_agent,
        &gql,
        &scoring,
    );
    let chat_loop = chat_poll_loop(
        league_id,
        chat_interval,
        &league_format,
        &scoring,
        bot_username,
        provider,
        &gql,
    );

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
    agent: &AgentRunner,
    gql: &SleeperGraphql,
    scoring: &str,
) -> Result<()> {
    let mut sleeper = SleeperClient::new();
    let mut review_state = ReviewState::load()?;
    println!("Loading league data for trade watch...");
    let mut league_data = load_league_data(&mut sleeper, league_id).await?;
    let mut last_refresh = Instant::now();

    loop {
        if last_refresh.elapsed() >= LEAGUE_DATA_REFRESH_INTERVAL {
            println!("Refreshing league data for trade watch...");
            league_data = load_league_data(&mut sleeper, league_id).await?;
            last_refresh = Instant::now();
        }

        if let Err(e) = process_trades(
            league_id,
            &mut sleeper,
            &league_data,
            agent,
            Some(gql),
            &mut review_state,
            days,
            scoring,
        )
        .await
        {
            eprintln!("Error during trade poll: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    }
}

/// Wraps either agent type so the chat loop doesn't branch on provider.
enum AgentRunner {
    Anthropic(ChatAgent),
    Gemini(GeminiChatAgent),
}

impl AgentRunner {
    async fn run(
        &self,
        user_message: &str,
        executor: &ToolExecutor<'_>,
        max_iterations: u32,
    ) -> Result<String> {
        match self {
            AgentRunner::Anthropic(agent) => {
                agent.run(user_message, executor, max_iterations).await
            }
            AgentRunner::Gemini(agent) => agent.run(user_message, executor, max_iterations).await,
        }
    }
}

async fn chat_poll_loop(
    league_id: &str,
    interval: u64,
    league_format: &str,
    scoring: &str,
    bot_username: &str,
    provider: &LlmProvider,
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

    println!("Loading league data for chat...");
    let mut league_data = load_league_data(&mut sleeper, league_id).await?;
    let mut lightweight_ctx = chat::build_lightweight_context(
        &league_data.league,
        &league_data.users,
        &league_data.rosters,
        &league_data.nfl_state,
    );
    let mut last_refresh = Instant::now();

    // Set up agent for the chosen provider — pass the league so the
    // compare_start_options tool advertises this league's actual lineup slots.
    let agent = match provider {
        LlmProvider::Anthropic => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
            let sys = llm::chat_system_prompt(league_format);
            AgentRunner::Anthropic(ChatAgent::new(api_key, sys, Some(&league_data.league)))
        }
        LlmProvider::Gemini => {
            let api_key = std::env::var("GEMINI_API_KEY")
                .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY not set"))?;
            let sys = llm::chat_system_prompt(league_format);
            AgentRunner::Gemini(GeminiChatAgent::new(api_key, sys, Some(&league_data.league)))
        }
    };

    loop {
        if last_refresh.elapsed() >= LEAGUE_DATA_REFRESH_INTERVAL {
            println!("Refreshing league data for chat...");
            league_data = load_league_data(&mut sleeper, league_id).await?;
            lightweight_ctx = chat::build_lightweight_context(
                &league_data.league,
                &league_data.users,
                &league_data.rosters,
                &league_data.nfl_state,
            );
            last_refresh = Instant::now();
        }

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

                    if !chat::is_mention(text, bot_username) {
                        continue;
                    }

                    let author = msg.author_display_name.as_deref().unwrap_or("Someone");
                    let question = chat::strip_mention(text, bot_username);

                    println!("\nMention from {author}: \"{text}\"");

                    let executor = ToolExecutor {
                        sleeper: &sleeper,
                        league_id,
                        league: &league_data.league,
                        players: &league_data.players,
                        users: &league_data.users,
                        rosters: &league_data.rosters,
                        roster_names: &league_data.roster_names,
                        nfl_state: &league_data.nfl_state,
                        historical_stats: &league_data.historical_stats,
                        projections: &league_data.projections,
                        champions: &league_data.champions,
                        all_time_stats: &league_data.all_time_stats,
                        scoring,
                        recent_transactions: &league_data.recent_transactions,
                        gql: Some(gql),
                    };

                    // Prepend recent conversation history for follow-up context
                    let author_id = msg.author_id.as_deref().unwrap_or("");
                    let history = chat_state.get_exchanges(author_id);
                    let history_str = if history.is_empty() {
                        String::new()
                    } else {
                        let mut h = String::from("Recent conversation with this user:\n");
                        for (q, a) in &history {
                            h.push_str(&format!("User: {q}\nYou: {a}\n\n"));
                        }
                        h
                    };

                    let user_msg = format!(
                        "League context: {lightweight_ctx}\n\n\
                         {history_str}\
                         {author} tagged you in the league chat and said:\n\"{question}\""
                    );

                    println!("Running agent...");
                    let response = agent.run(&user_msg, &executor, 10).await;

                    let resp_author_id = msg.author_id.as_deref().unwrap_or("");
                    match response {
                        Ok(reply) => {
                            println!("Response: {reply}");
                            // Record exchange for conversation continuity
                            chat_state.add_exchange(
                                resp_author_id,
                                question.clone(),
                                reply.clone(),
                            );
                            match gql.send_message(league_id, &reply).await {
                                Ok(()) => println!("Posted reply to league chat!"),
                                Err(e) => eprintln!("Failed to post reply: {e}"),
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to generate response: {e}");
                            // Post a fallback message so the user knows something went wrong
                            let _ = gql
                                .send_message(
                                    league_id,
                                    "I'm having trouble thinking right now. Try again in a bit.",
                                )
                                .await;
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
    extra_rules: Option<&str>,
    scoring_override: Option<&str>,
    provider: &LlmProvider,
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

        println!("Loading league data...");
        let mut sleeper = SleeperClient::new();
        let league_data = load_league_data(&mut sleeper, &league_id).await?;

        let scoring = scoring_override
            .map(|s| s.to_string())
            .unwrap_or_else(|| league_data.league.detect_scoring().to_string());
        let league_format = league_data.league.format_summary(extra_rules);
        println!("Detected league format: {league_format}");

        let gql_client = setup_graphql().ok();

        let agent = match provider {
            LlmProvider::Anthropic => {
                let api_key = std::env::var("ANTHROPIC_API_KEY")
                    .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
                let sys = llm::chat_system_prompt(&league_format);
                AgentRunner::Anthropic(ChatAgent::new(api_key, sys, Some(&league_data.league)))
            }
            LlmProvider::Gemini => {
                let api_key = std::env::var("GEMINI_API_KEY")
                    .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY not set"))?;
                let sys = llm::chat_system_prompt(&league_format);
                AgentRunner::Gemini(GeminiChatAgent::new(api_key, sys, Some(&league_data.league)))
            }
        };

        let executor = ToolExecutor {
            sleeper: &sleeper,
            league_id: &league_id,
            league: &league_data.league,
            players: &league_data.players,
            users: &league_data.users,
            rosters: &league_data.rosters,
            roster_names: &league_data.roster_names,
            nfl_state: &league_data.nfl_state,
            historical_stats: &league_data.historical_stats,
            projections: &league_data.projections,
            champions: &league_data.champions,
            all_time_stats: &league_data.all_time_stats,
            scoring: &scoring,
            recent_transactions: &league_data.recent_transactions,
            gql: gql_client.as_ref(),
        };

        let lightweight_ctx = chat::build_lightweight_context(
            &league_data.league,
            &league_data.users,
            &league_data.rosters,
            &league_data.nfl_state,
        );
        let user_msg = format!(
            "League context: {lightweight_ctx}\n\n\
             debug_user tagged you in the league chat and said:\n\"{question}\""
        );

        println!("Running agent (tool calls will be printed to stderr)...");
        let response = agent.run(&user_msg, &executor, 10).await?;

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
    league_data: &LeagueData,
    agent: &AgentRunner,
    gql: Option<&SleeperGraphql>,
    review_state: &mut ReviewState,
    days: u64,
    scoring: &str,
) -> Result<()> {
    let nfl_state = &league_data.nfl_state;
    println!(
        "NFL {} {} - Week {}",
        nfl_state.season, nfl_state.season_type, nfl_state.week
    );

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

    let recent_transactions = transactions.clone();

    for trade in trades {
        let tx_id = trade.id().to_string();
        println!("\n--- Trade {tx_id} ---");

        let summary = match trade_analyzer::parse_trade(
            trade,
            &league_data.roster_names,
            &league_data.roster_records,
            &league_data.players,
        ) {
            Some(s) => s,
            None => {
                eprintln!("Could not parse trade {tx_id}, skipping.");
                continue;
            }
        };

        // Build a simple trade prompt (no pre-fetched news — agent will search itself)
        let prompt = trade_analyzer::build_prompt(&summary, &HashMap::new());
        println!("\nTrade details:\n{prompt}\n");

        let executor = ToolExecutor {
            sleeper,
            league_id,
            league: &league_data.league,
            players: &league_data.players,
            users: &league_data.users,
            rosters: &league_data.rosters,
            roster_names: &league_data.roster_names,
            nfl_state,
            historical_stats: &league_data.historical_stats,
            projections: &league_data.projections,
            champions: &league_data.champions,
            all_time_stats: &league_data.all_time_stats,
            scoring,
            recent_transactions: &recent_transactions,
            gql,
        };

        println!("Running agent for trade analysis...");
        let analysis = match agent.run(&prompt, &executor, 10).await {
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

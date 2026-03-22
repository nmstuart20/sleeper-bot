mod anthropic;
mod gemini;
mod graphql;
mod llm;
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
use crate::state::ReviewState;

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
        #[arg(long, value_enum, default_value = "gemini", env = "LLM_PROVIDER")]
        provider: LlmProvider,
        /// Only process trades from the last N days
        #[arg(long, default_value = "2")]
        days: u64,
    },
    /// Watch for trades continuously with polling
    Watch {
        #[arg(long, env = "SLEEPER_LEAGUE_ID")]
        league: String,
        /// Poll interval in seconds
        #[arg(long, default_value = "300")]
        interval: u64,
        /// LLM provider to use for analysis
        #[arg(long, value_enum, default_value = "gemini", env = "LLM_PROVIDER")]
        provider: LlmProvider,
        /// Only process trades from the last N days
        #[arg(long, default_value = "2")]
        days: u64,
    },
    /// Debug the GraphQL connection
    Debug {
        /// Test login and print the response
        #[arg(long)]
        login: bool,
        /// Send a test message to the league chat
        #[arg(long)]
        send: Option<String>,
        #[arg(long, env = "SLEEPER_LEAGUE_ID")]
        league: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    match cli {
        Cli::Check {
            league,
            post,
            provider,
            days,
        } => {
            let analyzer = build_analyzer(&provider)?;
            run_check(&league, post, days, analyzer.as_ref()).await
        }
        Cli::Watch {
            league,
            interval,
            provider,
            days,
        } => {
            let analyzer = build_analyzer(&provider)?;
            run_watch(&league, interval, days, analyzer.as_ref()).await
        }
        Cli::Debug {
            login,
            send,
            league,
        } => run_debug(login, send, league).await,
    }
}

fn build_analyzer(provider: &LlmProvider) -> Result<Box<dyn TradeAnalyzer>> {
    match provider {
        LlmProvider::Anthropic => {
            let key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY not set"))?;
            println!("Using Anthropic (Claude) for trade analysis.");
            Ok(Box::new(AnthropicClient::new(key)))
        }
        LlmProvider::Gemini => {
            let key = std::env::var("GEMINI_API_KEY")
                .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY not set"))?;
            println!("Using Gemini for trade analysis.");
            Ok(Box::new(GeminiClient::new(key)))
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

    let mut gql = if post {
        match setup_graphql().await {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("Warning: GraphQL login failed ({e}). Continuing in terminal-only mode.");
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
        &mut gql,
        &mut review_state,
        days,
    )
    .await
}

async fn run_watch(
    league_id: &str,
    interval: u64,
    days: u64,
    analyzer: &dyn TradeAnalyzer,
) -> Result<()> {
    let mut sleeper = SleeperClient::new();
    let mut review_state = ReviewState::load()?;

    let mut gql = match setup_graphql().await {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("Warning: GraphQL login failed ({e}). Continuing in terminal-only mode.");
            None
        }
    };

    println!(
        "Watching league {league_id} for trades (polling every {interval}s). Press Ctrl+C to stop."
    );

    loop {
        if let Err(e) = process_trades(
            league_id,
            &mut sleeper,
            analyzer,
            &mut gql,
            &mut review_state,
            days,
        )
        .await
        {
            eprintln!("Error during poll cycle: {e}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    }
}

async fn run_debug(login: bool, send: Option<String>, league: Option<String>) -> Result<()> {
    if login {
        println!("Testing GraphQL login...");
        let mut gql = setup_graphql().await?;
        println!(
            "Login successful! Authenticated: {}",
            gql.is_authenticated()
        );

        if let (Some(msg), Some(league_id)) = (send, league) {
            println!("Sending test message to league {league_id}...");
            gql.send_message(&league_id, &msg).await?;
            println!("Message sent successfully!");
        }
    } else if let Some(msg) = send {
        let league_id = league.ok_or_else(|| anyhow::anyhow!("--league required with --send"))?;
        let mut gql = setup_graphql().await?;
        println!("Sending test message to league {league_id}...");
        gql.send_message(&league_id, &msg).await?;
        println!("Message sent successfully!");
    } else {
        println!(
            "Use --login to test authentication, --send <msg> --league <id> to send a test message."
        );
    }
    Ok(())
}

async fn setup_graphql() -> Result<SleeperGraphql> {
    let username = std::env::var("SLEEPER_USERNAME")
        .map_err(|_| anyhow::anyhow!("SLEEPER_USERNAME not set"))?;
    let password = std::env::var("SLEEPER_PASSWORD")
        .map_err(|_| anyhow::anyhow!("SLEEPER_PASSWORD not set"))?;

    let mut gql = SleeperGraphql::new(username, password);
    gql.login().await?;
    Ok(gql)
}

async fn process_trades(
    league_id: &str,
    sleeper: &mut SleeperClient,
    analyzer: &dyn TradeAnalyzer,
    gql: &mut Option<SleeperGraphql>,
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

        let prompt = trade_analyzer::build_prompt(&summary);
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

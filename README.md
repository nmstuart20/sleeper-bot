# Sleeper Trade Review Bot

A Rust CLI that monitors a Sleeper fantasy football league for completed trades, generates LLM-powered analysis, and optionally posts reviews to the league chat.

## Setup

**Create a `.env` file** in the project root:

```
SLEEPER_LEAGUE_ID=123456789
ANTHROPIC_API_KEY=sk-ant-...
GEMINI_API_KEY=...
SLEEPER_USERNAME=your_email@example.com
SLEEPER_PASSWORD=your_password
```

- `SLEEPER_LEAGUE_ID` — found in your league URL on sleeper.com
- `ANTHROPIC_API_KEY` — required if using the `anthropic` provider
- `GEMINI_API_KEY` — required if using the `gemini` provider
- `SLEEPER_USERNAME` / `SLEEPER_PASSWORD` — only needed for posting to league chat (`--post`)

## Usage

### Check for trades (one-shot)

```sh
cargo run --release -- check --league <LEAGUE_ID>
```

Fetches the current week's transactions, analyzes any new trades, and prints results to the terminal.

Add `--post` to also send the review to the Sleeper league chat:

```sh
cargo run --release -- check --league <LEAGUE_ID> --post
```

### Watch for trades (continuous)

```sh
cargo run --release -- watch --league <LEAGUE_ID>
```

Polls every 5 minutes (default). Change with `--interval <seconds>`:

```sh
cargo run --release -- watch --league <LEAGUE_ID> --interval 120
```

### Choose LLM provider

Default is `gemini`. Switch with `--provider`:

```sh
cargo run --release -- check --league <LEAGUE_ID> --provider anthropic
cargo run --release -- check --league <LEAGUE_ID> --provider gemini
```

Or set `LLM_PROVIDER` in your `.env`.

### Debug GraphQL connection

```sh
cargo run --release -- debug --login
cargo run --release -- debug --login --send "Hello from the bot!" --league <LEAGUE_ID>
```

### Cron example

Run every 5 minutes, posting to chat:

```
*/5 * * * * cd /path/to/sleeper_bot && cargo run --release -- check --league <LEAGUE_ID> --post
```

## How it works

1. Fetches the current NFL week from the Sleeper API
2. Pulls all transactions for that week and filters for completed trades
3. Resolves player names, team records, and draft pick details
4. Sends a structured prompt to the configured LLM for opinionated analysis
5. Optionally posts the review to the Sleeper league chat via GraphQL
6. Tracks reviewed trade IDs in `.reviewed_trades.json` to avoid double-posting

## Notes

- The Sleeper GraphQL API (used for chat posting) is undocumented and may change. If posting fails, the bot falls back to terminal output.
- The NFL player database (~40MB) is cached to `players_cache.json` and refreshed weekly.

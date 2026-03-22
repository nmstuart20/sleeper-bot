# Sleeper Trade Review Bot

A Rust CLI that monitors a Sleeper fantasy football league for completed trades, generates LLM-powered analysis with real-time news context, and optionally posts reviews to the league chat.

## Setup

**Create a `.env` file** in the project root:

```
SLEEPER_LEAGUE_ID=123456789
GEMINI_API_KEY=...
ANTHROPIC_API_KEY=sk-ant-...
SLEEPER_TOKEN=eyJhbGciOiJIUzI1NiIs...
```

### Environment variables

| Variable | Required | Description |
|---|---|---|
| `SLEEPER_LEAGUE_ID` | Yes | Found in your league URL on sleeper.app |
| `GEMINI_API_KEY` | If using Gemini | API key for Google Gemini (default provider) |
| `ANTHROPIC_API_KEY` | If using Anthropic | API key for Claude |
| `SLEEPER_TOKEN` | For `--post` | Auth token for posting to league chat |

### Getting your Sleeper token

The bot uses a token from your browser session to post messages. Tokens last **1 year**.

1. Log into [sleeper.app](https://sleeper.app) in your browser
2. Open DevTools (F12) → **Application** tab → **Local Storage** → `https://sleeper.app`
3. Find and copy your auth token (a long `eyJ...` string)
4. Add it to your `.env` as `SLEEPER_TOKEN=<your_token>`

The bot checks the token expiry on every run and will:
- Show remaining days on startup
- Warn you when it's within 30 days of expiring
- Error with refresh instructions if it's already expired

## Usage

### Check for trades (one-shot)

```sh
cargo run --release -- check --league <LEAGUE_ID>
```

Scans all weeks for recent trades, fetches player news, analyzes with the LLM, and prints results.

Add `--post` to also send the review to the Sleeper league chat:

```sh
cargo run --release -- check --league <LEAGUE_ID> --post
```

Adjust the lookback window (default 2 days):

```sh
cargo run --release -- check --league <LEAGUE_ID> --days 7
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

### Debug / test connection

Verify your token is valid and check expiry:

```sh
cargo run --release -- debug --check-token
```

Send a test message to your league chat:

```sh
cargo run --release -- debug --send "Hello from the bot!" --league <LEAGUE_ID>
```

### Cron example

Run every 5 minutes, posting to chat:

```
*/5 * * * * cd /path/to/sleeper_bot && cargo run --release -- check --league <LEAGUE_ID> --post
```

## How it works

1. Fetches the current NFL state from the Sleeper API
2. Scans transactions across all weeks (0-18) to catch offseason/dynasty trades
3. Resolves player names, team records, and draft pick details
4. Enriches each player with metadata from Sleeper (age, injury status, depth chart position, years of experience)
5. Fetches recent news headlines for each player via Google News RSS
6. Sends a structured prompt with trade details, player context, news, and today's date to the configured LLM
7. Optionally posts the review to the Sleeper league chat via GraphQL
8. Tracks reviewed trade IDs in `.reviewed_trades.json` to avoid double-posting

## Notes

- The Sleeper GraphQL API (used for chat posting) is undocumented and may change. If posting fails, the bot falls back to terminal output.
- The NFL player database (~40MB) is cached to `players_cache.json` and refreshed weekly.
- News fetching uses Google News RSS and adds a small delay between requests to be respectful.
- The bot scans all weeks (not just the current one) so it catches dynasty and offseason trades.

# Sleeper Trade Review Bot

A Rust CLI that monitors a Sleeper fantasy football league for trades and chat @mentions, generates LLM-powered analysis in a configurable character persona, and posts to the league chat.

## Setup

Environmental variables needed (can make a `.env` file in the project root for sensitive tokens and keys):

```
SLEEPER_LEAGUE_ID=123456789
GEMINI_API_KEY=...
ANTHROPIC_API_KEY=sk-ant-...
SLEEPER_TOKEN=...
```

| Variable | Required | Description |
|---|---|---|
| `SLEEPER_LEAGUE_ID` | Yes | Found in your league URL on sleeper.app |
| `GEMINI_API_KEY` | If using Gemini | API key for Google Gemini |
| `ANTHROPIC_API_KEY` | If using Anthropic | API key for Claude (default provider) |
| `SLEEPER_TOKEN` | For posting/watch | Auth token from your browser session (lasts 1 year) |
| `LLM_PROVIDER` | No | `anthropic` (default) or `gemini` |
| `BOT_CHARACTER` | No | Character persona (default: `Donald Trump`) |

### Setting Up Sleeper for Chat

Sleeper's API is read-only so to post in the chat we need a user account.

If you don't want to post from your user account:

1. Create a new account with username (example: tradebot123)
2. Add account as a co-owner of a team in your league.

#### Getting the Sleeper Token

1. Log into [sleeper.app](https://sleeper.app) in your browser with the bot account
2. DevTools (F12) → **Application** → **Local Storage** → `https://sleeper.app`
3. Copy the auth token into `.env` as `SLEEPER_TOKEN`

### Config

Update the `config.toml` file with the league rules, scoring format and the bot_username

## Commands

### `check` — One-shot trade scan

Scans all weeks for recent trades, analyzes with the LLM, and prints results.

```sh
cargo run --release -- check --league <ID>
```

| Flag | Default | Description |
|---|---|---|
| `--league <ID>` | `$SLEEPER_LEAGUE_ID` | League ID |
| `--post` | off | Post reviews to Sleeper league chat |
| `--provider <P>` | `anthropic` | `anthropic` or `gemini` |
| `--days <N>` | `3` | Only process trades from the last N days |
| `--character <C>` | `Donald Trump` | Character persona for the analysis |

### `watch` — Continuous polling

Polls for new trades **and** responds to @mentions in the league chat. Requires `SLEEPER_TOKEN`.

```sh
cargo run --release -- watch --league <ID>
```

| Flag | Default | Description |
|---|---|---|
| `--league <ID>` | `$SLEEPER_LEAGUE_ID` | League ID |
| `--interval <S>` | `20` | Trade poll interval in seconds |
| `--chat-interval <S>` | `20` | Chat poll interval in seconds |
| `--provider <P>` | `anthropic` | `anthropic` or `gemini` |
| `--days <N>` | `3` | Trade lookback window in days |
| `--character <C>` | `Donald Trump` | Character persona |

### `debug` — Test & troubleshoot

```sh
# Verify token and check expiry
cargo run --release -- debug --check-token

# Send a test message to league chat
cargo run --release -- debug --send "Hello!" --league <ID>

# Test the chat AI locally (prints response, does not post)
cargo run --release -- debug --chat "Who won the league last year?" --league <ID>
```

| Flag | Default | Description |
|---|---|---|
| `--check-token` | — | Verify token validity and expiry |
| `--send <MSG>` | — | Send a test message (requires `--league`) |
| `--chat <Q>` | — | Test chat AI response (requires `--league`) |
| `--league <ID>` | `$SLEEPER_LEAGUE_ID` | League ID |
| `--provider <P>` | `anthropic` | LLM provider for `--chat` |
| `--character <C>` | `Donald Trump` | Character persona for `--chat` |

## How it works

1. Fetches NFL state and scans transactions across weeks 0–18
2. Resolves player names, team records, draft picks, and player metadata (age, injury, depth chart)
3. Fetches recent news headlines via Google
4. Sends structured prompt to the configured LLM with trade details, player context, and news
5. In `watch` mode, also monitors league chat for @mentions and responds with league-aware answers
6. Posts reviews/responses to Sleeper chat via GraphQL
7. Tracks reviewed trades in `.reviewed_trades.json` and responded messages in `.chat_state.json` to avoid duplicates

## Cron example

```
*/5 * * * * cd /path/to/sleeper_bot && cargo run --release -- check --league <ID> --post
```

## Notes

- The Sleeper GraphQL API is undocumented and may change. If posting fails, the bot falls back to terminal output.
- The NFL player database (~40MB) is cached to `players_cache.json` and refreshed weekly.
- League configuration lives in `config.toml` (rules, scoring).

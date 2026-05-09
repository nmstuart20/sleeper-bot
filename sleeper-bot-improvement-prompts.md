# Sleeper Bot — Claude Code Implementation Prompts

Run these prompts sequentially from your `sleeper-bot` project root. Each prompt is self-contained. Later prompts may reference files created by earlier ones.

---

## 8. Add Roster-Aware Waiver Recommendations

```
In src/tools.rs, enhance the `search_waiver_wire` tool to optionally recommend players based on a specific team's roster needs.

1. Add an optional `for_team` parameter to the tool definition:
   "for_team": {
       "type": "string",
       "description": "Optional: team/owner name to get personalized waiver recommendations based on their roster's weakest positions. When provided, the tool analyzes the team's roster depth at each position and prioritizes waiver picks that fill gaps."
   }

2. Update the `SearchWaiverWire` variant to include `for_team: Option<String>`.

3. In the `search_waiver_wire` implementation, when `for_team` is provided:
   a. Find the team's roster using the same fuzzy matching as get_team_roster
   b. Count starters at each position (QB, RB, WR, TE)
   c. Identify positions with fewer starters than typical lineup requirements (e.g. only 1 RB when the league starts 2)
   d. Also identify positions where the bench depth is thin (0-1 bench players at that position)
   e. Prepend a "Roster needs:" summary like "Weak at RB (1 starter, 1 bench). WR depth is thin (0 bench)."
   f. Then show the waiver results, but sort/group by the team's needs first — show players at weak positions first, then other positions

4. When `for_team` is not provided, behavior is unchanged from the current implementation.

5. Update parse_tool_call and add tests.
```

---

## 9. Add Weekly Recap Command

```
Add a new CLI subcommand `recap` that generates and optionally posts a weekly recap to the league chat.

1. In src/main.rs, add a new Cli variant:
   Recap {
       #[arg(long, env = "SLEEPER_LEAGUE_ID")]
       league: String,
       #[arg(long)]
       post: bool,
       #[arg(long, value_enum, default_value = "anthropic", env = "LLM_PROVIDER")]
       provider: LlmProvider,
       #[arg(long, default_value = "Donald Trump", env = "BOT_CHARACTER")]
       character: String,
       /// Which week to recap (defaults to current week - 1)
       #[arg(long)]
       week: Option<u32>,
   }

2. In src/llm.rs, add a `recap_system_prompt(character: &str, league_rules: &str) -> String` function:
   - The persona should be the character
   - The prompt should instruct the LLM to: use get_matchup and get_league_standings to get results, then produce a recap covering: biggest blowout, closest game, highest scorer, lowest scorer, best bench (most points left on bench), biggest upset, updated standings/playoff picture
   - Keep it under 1500 characters for Sleeper chat
   - Use the character's voice and personality
   - Tell it to call tools for EVERY team's matchup to get the full picture

3. Implement `run_recap` that:
   - Loads all league data (same pattern as run_debug)
   - Creates a ToolExecutor
   - Builds the agent with the recap system prompt
   - Sends a user message like "Generate the Week {week} recap for the league. Look up every matchup and the standings to give a complete summary."
   - Prints the result and optionally posts to chat

4. Wire up the Recap variant in the main match block.
```

---

## 10. Add Weekly Preview Command

```
Add a new CLI subcommand `preview` that generates and optionally posts a weekly matchup preview.

1. In src/main.rs, add a new Cli variant:
   Preview {
       #[arg(long, env = "SLEEPER_LEAGUE_ID")]
       league: String,
       #[arg(long)]
       post: bool,
       #[arg(long, value_enum, default_value = "anthropic", env = "LLM_PROVIDER")]
       provider: LlmProvider,
       #[arg(long, default_value = "Donald Trump", env = "BOT_CHARACTER")]
       character: String,
       /// Which week to preview (defaults to current week)
       #[arg(long)]
       week: Option<u32>,
   }

2. In src/llm.rs, add a `preview_system_prompt(character: &str, league_rules: &str) -> String`:
   - Character persona
   - Instruct: use get_matchup for each team to see all matchups, use get_team_roster on interesting teams, use web_search for relevant injury/news updates
   - Produce: matchup-by-matchup predictions with projected winners, highlight the "game of the week", mention injury concerns, identify must-start and risky plays
   - Under 1500 characters, character voice, no markdown

3. Implement `run_preview` following the same pattern as run_recap.

4. Wire up the Preview variant in main.

5. Add a note to the README.md under Commands for both `recap` and `preview` with example cron entries:
   # Tuesday preview
   0 10 * * 2 cd /path/to/sleeper_bot && cargo run --release -- preview --league <ID> --post
   # Monday recap
   0 10 * * 1 cd /path/to/sleeper_bot && cargo run --release -- recap --league <ID> --post
```

---

## 11. Remove Stale Lightweight Context Standings Duplication

```
In src/main.rs, the chat_poll_loop (around line 443) builds a `lightweight_ctx` that includes a compact standings line, and then the LLM also has access to the `GetLeagueStandings` tool. This means standings data is sent twice — once inline (potentially stale since it's built at startup) and once via tool call (current).

Fix this:

1. In src/chat.rs, modify `build_lightweight_context` to NOT include the standings line. Keep only the league metadata: league name, type (dynasty/redraft), team count, scoring format, NFL season and week. Rename it to `build_league_metadata` to reflect what it actually is now.

2. Update all call sites in main.rs (chat_poll_loop and run_debug) to use the new function name.

3. In the system prompt rules (src/llm.rs, chat_system_prompt), add: "- The league metadata in the user message gives you basic league info. For current standings, records, and matchups, ALWAYS use the appropriate tool — the metadata does not include standings."

This saves ~200-400 tokens per request on the inline standings and ensures the LLM always gets fresh data from the tool.
```

---

## 12. Add Conversation History Summarization

```
In src/main.rs chat_poll_loop (around line 450-458), the conversation history is replayed verbatim:

    for (q, a) in &history {
        h.push_str(&format!("User: {q}\nYou: {a}\n\n"));
    }

Since responses can be up to 1000 characters and there are up to 3 exchanges (MAX_RECENT_EXCHANGES in state.rs), this can add ~3000+ tokens to every request — and it's re-sent on every iteration of the agent loop.

Optimize this:

1. In src/state.rs, add a method `get_summary(&self, user_id: &str) -> Option<String>` that returns a compressed summary of recent exchanges. Format: "Previous context: User asked about [first question topic]. You told them [key point]. Then they asked about [second question]. You said [key point]." Keep it under 200 characters total by truncating questions to 50 chars and answers to 80 chars.

2. In main.rs chat_poll_loop, replace the verbatim history replay with the summary. Only include it if the current question appears to be a follow-up — check if it contains pronouns without clear antecedents ("he", "his", "that player", "them", "what about", "and") or is very short (< 20 chars, suggesting it's a continuation like "what about his backup?").

3. Keep the full `add_exchange` storage unchanged — you might want the full history for debugging or future features.

4. Add tests in state.rs for the summary method.
```

---

## 13. Add Trade Block Feature

```
Add a "trade block" feature where league members can declare players they're shopping, and others can query it.

1. In src/state.rs, add a new struct `TradeBlock`:
   - Store as HashMap<String, Vec<TradeBlockEntry>> where key is user_id
   - TradeBlockEntry: { player_name: String, added_at: u64 (unix ms), note: Option<String> }
   - Persist to `.trade_block.json`
   - Methods: add_player, remove_player, get_all, get_for_user, clear_expired (remove entries older than 30 days)

2. In src/tools.rs, add two new tools:

   a. "manage_trade_block":
      - description: "Add or remove a player from a team's trade block. Use when someone says 'put X on the trade block' or 'take X off the block'."
      - input: action (enum: "add", "remove"), team_name (string), player_name (string), note (optional string)
      - Execution: resolve team_name to user_id via fuzzy match, then add/remove from TradeBlock

   b. "get_trade_block":
      - description: "View the current trade block — all players that league members have declared available for trade. Use when someone asks 'who's on the trade block?' or 'is anyone trading a RB?'"
      - input: position (optional string filter), team_name (optional string filter)
      - Execution: return formatted list of all trade block entries, optionally filtered

3. Add `trade_block: &'a mut TradeBlock` to the ToolExecutor struct (note: mut because manage_trade_block modifies it).

4. Wire it up in main.rs — load TradeBlock alongside ChatState, pass it to ToolExecutor.

5. Update all_tool_definitions count test.
```

---

## 14. Add Token Cost Logging to a File

```
Currently agent.rs logs token usage to stderr on completion (line 149-155). Add persistent cost tracking so you can monitor API spend over time.

1. Create src/usage.rs with a struct `UsageLog`:
   - Append-only log to `.usage_log.jsonl` (one JSON object per line)
   - Each entry: { timestamp: ISO8601, command: String (check/watch/recap/etc), model: String, input_tokens: u64, output_tokens: u64, tool_calls: u32, iterations: u32, estimated_cost_usd: f64 }
   - Method: `log_usage(entry: UsageEntry) -> Result<()>` that appends to the file
   - Method: `summary(days: u32) -> Result<UsageSummary>` that reads the file and returns totals for the last N days
   - Cost estimation: use approximate per-token prices (Haiku input: $0.80/M, output: $4/M; Sonnet input: $3/M, output: $15/M — check current pricing and adjust)

2. In agent.rs, after the agent loop completes, call `UsageLog::log_usage()` with the accumulated stats.

3. Add a new Debug flag `--usage` that prints a usage summary:
   cargo run -- debug --usage
   Output: "Last 7 days: 142 requests, 1.2M input tokens, 340K output tokens, ~$5.82 estimated cost"

4. Add `mod usage;` to main.rs.
```

---

## 15. Add Error Fallback Response Improvement

```
In src/main.rs chat_poll_loop (around line 485-493), when the agent fails, the bot posts a generic "I'm having trouble thinking right now. Try again in a bit." This is good — but enhance it:

1. When the error is a rate limit (contains "429" or "rate" in the error message), post: "I'm getting rate-limited right now. Give me a minute and try again."

2. When the error is an auth/API key issue (contains "401" or "authentication"), log the full error to stderr but post: "I'm having an authentication issue. The league admin needs to check my API key."

3. When the error is a timeout or network issue, post: "I'm having trouble connecting right now. Try again in a bit."

4. For all other errors, keep the current generic message.

5. Also add the same error categorization to the trade analysis path (around line 720-723 in process_trades) — currently it just prints to stderr and skips the trade, but if `--post` is enabled, it should post a brief note like "I couldn't analyze this trade right now — I'll try again next poll."
```

---

## 16. Add Computed `GetPowerRankings` Tool (No LLM Reasoning Needed)

```
The query "who has the highest projected ppg total right now for 2026 with their best starting lineup" causes the agent to call GetTeamRoster and GetPlayerInfo for every team and every player, generating 50+ tool calls and hitting rate limits. This is a pure computation problem — the LLM shouldn't be doing the math.

Add a new tool `get_power_rankings` that computes optimal projected lineups entirely in Rust and returns a finished leaderboard. The LLM just calls one tool and formats the result.

1. Add a new enum variant in src/tools.rs:
   GetPowerRankings { sort_by: Option<String> }

2. Add the tool definition:
   - name: "get_power_rankings"
   - description: "Compute power rankings for all teams by calculating each team's optimal starting lineup from their current roster using projected fantasy points. This does all the math — use it instead of manually looking up each team's roster and players. Supports sorting by: 'projected' (default, best possible lineup projected points), 'record' (current W-L record), or 'scored' (actual points scored this season)."
   - input_schema: sort_by (optional string, enum: ["projected", "record", "scored"])

3. The implementation needs access to the League struct (for roster_positions). Add `league: &'a League` to the ToolExecutor struct. Update all ToolExecutor construction sites in main.rs (chat_poll_loop, run_debug, process_trades) to pass the league reference.

4. Implement the computation in a method `get_power_rankings(&self, sort_by: Option<&str>) -> Result<String>`:

   a. Get the league's starter slot list from self.league.roster_positions. Filter to just the starter slots (everything that's NOT "BN", "IR", "TAXI"). This gives you the lineup template, e.g. ["QB", "RB", "RB", "WR", "WR", "WR", "TE", "FLEX", "SUPER_FLEX", "K", "DEF"].

   b. Build a helper function `compute_optimal_lineup(roster: &Roster, slots: &[String], players: &HashMap<String, Player>, projections: &HashMap<String, PlayerStats>, scoring: &str) -> (f64, Vec<(String, String, f64)>)` that returns (total_projected_pts, vec_of (player_name, position, projected_pts)):
      - For each player on the roster, look up their projected points from self.projections using the scoring format
      - Sort players by projected points descending
      - Greedily fill slots: for each slot in order, find the highest-projected unassigned player eligible for that slot:
        - "QB" slot: only QB
        - "RB" slot: only RB
        - "WR" slot: only WR
        - "TE" slot: only TE
        - "K" slot: only K
        - "DEF" slot: only DEF
        - "FLEX" slot: RB, WR, or TE
        - "SUPER_FLEX" slot: QB, RB, WR, or TE
        - "REC_FLEX" slot: WR or TE
        - "WRRB_FLEX" slot: WR or RB
      - If a slot can't be filled, skip it (empty roster spot)
      - Return total points and the lineup

   c. For each roster, compute the optimal lineup. Collect into a vec of (team_name, record, actual_points_scored, projected_optimal_pts, top_3_players).

   d. Sort by the requested sort_by field (default: projected).

   e. Format as a compact leaderboard:
      "Power Rankings (by projected optimal lineup):
       1. Nick (Touchdown Tyrants) — 8-2, 1234.5 PF — Optimal lineup: 187.3 proj pts
          Top: Patrick Mahomes (QB, 22.1), Ja'Marr Chase (WR, 19.8), Saquon Barkley (RB, 17.4)
       2. Mike (Dumpster Fire) — 3-7, 998.2 PF — Optimal lineup: 162.1 proj pts
          Top: Josh Allen (QB, 24.3), Davante Adams (WR, 14.2), Travis Kelce (TE, 12.8)
       ..."

   f. Keep the output compact — only show top 3 players per team to stay under ~1500 tokens total.

5. Update all_tool_definitions, parse_tool_call, the execute match arm, and the tool count test.
```

---

## 17. Add Computed `GetLeagueSummary` Tool (All-in-One Dashboard)

```
Add another computed tool that answers broad "how's the league doing" or "give me the full picture" questions with a single tool call. This prevents the agent from making 5+ separate tool calls for standings + matchups + transactions + waiver wire.

1. Add a new enum variant:
   GetLeagueSummary

2. Tool definition:
   - name: "get_league_summary"
   - description: "Get a comprehensive league dashboard in one call: current standings, this week's matchups with scores (if available), the 5 most recent transactions, and top 5 waiver wire players. Use this for broad questions like 'what's going on in the league?' or 'give me an update' instead of calling multiple tools separately."
   - input_schema: no required params

3. Implementation — compose results from existing methods:
   a. Call self.get_league_standings() for standings
   b. Fetch all matchups for the current week and format them compactly: "Matchups: Nick (102.3) vs Mike (87.1), Sarah (95.4) vs Tom (91.2), ..."
   c. Call self.get_recent_transactions(None, 5) for recent moves
   d. Call self.search_waiver_wire(None, 5) for top available players
   e. Concatenate with section headers, keeping total output under 2000 tokens

4. Update all definitions, parse, execute, and test count.
```

---

## 18. Add Agent Iteration Cap and Cost Guard

```
In src/agent.rs, the agent loop runs up to max_iterations (currently 10, set in main.rs). For expensive queries that trigger many tool calls, this can mean 10 API round-trips before hitting the cap — enough to trigger rate limits on the Anthropic API.

Add a smarter cost guard:

1. Add a `max_tool_calls: u32` field to ChatAgent (default: 15). This is separate from max_iterations — a single iteration can have multiple tool calls (parallel tool use). Track total_tool_calls (you already do this on line 133) and check it in the loop:

   if total_tool_calls >= self.max_tool_calls {
       // Force a final response without more tools
       // Add a user message telling the LLM to answer with what it has
       messages.push(AgentMessage {
           role: "user".to_string(),
           content: AgentContent::Text(
               "You've used all available tool calls. Please give your best answer based on the information gathered so far. Do not attempt any more tool calls.".to_string()
           ),
       });
       // Make one final API call WITHOUT tools in the request body
       // so the LLM is forced to respond with text only
       let final_response = self.call_api_no_tools(&messages).await?;
       // accumulate tokens, return text
   }

2. Add a `call_api_no_tools` method that's identical to `call_api` but omits the `tools` field from the request body. This forces the LLM to generate a text response.

3. Lower the default max_iterations from 10 to 6 in the three call sites in main.rs (lines 467, 595, 718). Between the iteration cap and the tool call cap, the agent should never make more than ~15 API calls total.

4. In the existing stderr logging, add a note when the cost guard fires: "Agent hit tool call limit (15), forcing final response with available data."

5. Add a `--max-tool-calls <N>` CLI flag to the Watch and Debug commands so you can tune it without recompiling. Pass it through to ChatAgent.
```

---

## 19. Add `GetAllMatchups` Tool (Batch Fetch)

```
Right now, if the LLM wants to see all matchups for the week (for recaps, previews, or power ranking context), it has to call `get_matchup` once per team — that's 5 tool calls for a 10-team league (each call returns both sides). But the Sleeper API already returns ALL matchups in one endpoint call (GET /league/{id}/matchups/{week}).

Add a bulk tool that returns all matchups at once:

1. Add a new enum variant:
   GetAllMatchups { week: Option<u32> }

2. Tool definition:
   - name: "get_all_matchups"
   - description: "Get all matchups for a given week with scores (if available). Returns every matchup in the league at once — much more efficient than calling get_matchup for each team individually. Defaults to the current week if no week is specified."
   - input_schema: week (optional integer)

3. Implementation:
   a. Use self.sleeper.get_matchups(self.league_id, week) — this is the same API call that get_matchup already makes, but instead of filtering to one team, format ALL matchups.
   b. Group matchups by matchup_id to pair opponents.
   c. For each pair, format: "Team A (pts) vs Team B (pts)" with scores if available.
   d. Sort by matchup_id for consistent ordering.
   e. Keep output compact — just the team names and scores, not individual player breakdowns (the LLM can call get_matchup for a specific team if it wants the detail).

4. Update all definitions, parse, execute, and test count.

5. In the system prompts (src/llm.rs), update the recap and preview prompts (if they exist from prompt #9/#10) to reference this tool: "Use get_all_matchups to see every matchup at once instead of looking up teams one by one."
```

---

## 20. Add `CalculateTradeValue` Tool (Computed, No LLM Math)

```
When the LLM evaluates trades, it currently has to call get_player_info on each player individually and then do its own value comparison. For multi-player trades, this can be 6-8 tool calls. Add a computed tool that does the value comparison in Rust.

1. Add a new enum variant:
   CalculateTradeValue { team_a_players: Vec<String>, team_b_players: Vec<String> }

2. Tool definition:
   - name: "calculate_trade_value"
   - description: "Calculate and compare the total fantasy value of two sides of a trade. Input the player names for each side, and this tool will look up projected points, age, injury status, and positional value for every player, then return a side-by-side comparison with a total value for each side. Use this when evaluating trades instead of calling get_player_info on each player individually."
   - input_schema:
     - team_a_players: array of strings (required)
     - team_b_players: array of strings (required)

3. Implementation:
   a. For each player name in both lists, fuzzy-match against self.players (reuse the matching logic from get_player_info)
   b. For each matched player, gather: projected points (from self.projections), age, injury status, position, NFL team, and whether they're currently rostered
   c. Compute a simple composite value per player: projected_points + age_bonus (under 26: +10%, 26-29: +0%, 30+: -10% per year over 29) — this is a rough dynasty value heuristic, not perfect, but better than nothing
   d. Sum each side's total value
   e. Format output:
      "Trade Analysis:
       Side A (total: 182.3 value):
         Ja'Marr Chase (WR, CIN) — 18.2 proj, age 26, Healthy, value: 18.2
         2025 Round 1 Pick — (cannot value draft picks with this tool)
       Side B (total: 156.7 value):
         Derrick Henry (RB, BAL) — 14.1 proj, age 31, Healthy, value: 11.3 (age adj: -20%)
         Travis Kelce (TE, KC) — 11.8 proj, age 36, Healthy, value: 7.1 (age adj: -50%)
       Difference: Side A ahead by 25.6 points"
   f. For draft picks in the player name list (detected by matching patterns like "2025 Round 1" or "1st round pick"), note that draft picks can't be valued by this tool — the LLM will need to apply its own judgment there.

4. Update all definitions, parse, execute, and test count. The parse_tool_call for this one needs to handle the array-of-strings input:
   let team_a = input.get("team_a_players")
       .and_then(|v| v.as_array())
       .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
       .unwrap_or_default();

5. This tool should be referenced in the trade_system_prompt in src/llm.rs: "Use calculate_trade_value to get a quantitative side-by-side comparison of the trade, then layer your own dynasty analysis on top."
```

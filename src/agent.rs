use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{self, ToolExecutor};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-sonnet-4-20250514";

// ─── Request types ───

#[derive(Serialize)]
struct AgentRequest {
    model: &'static str,
    max_tokens: u32,
    system: String,
    messages: Vec<AgentMessage>,
    tools: Vec<Value>,
}

#[derive(Serialize, Clone, Debug)]
struct AgentMessage {
    role: String,
    content: AgentContent,
}

/// Content can be a simple string or an array of content blocks.
#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
enum AgentContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

// ─── Response types ───

#[derive(Deserialize, Debug)]
struct AgentResponse {
    stop_reason: Option<String>,
    content: Vec<ContentBlock>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    /// Server-side tool invocation (e.g. web_search) — executed by Anthropic, not by us.
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    /// Server-side tool result — returned inline by Anthropic, no action needed from us.
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        content: Value,
    },
}

// ─── ChatAgent ───

pub struct ChatAgent {
    api_key: String,
    client: reqwest::Client,
    system_prompt: String,
    tools: Vec<Value>,
}

impl ChatAgent {
    pub fn new(api_key: String, system_prompt: String) -> Self {
        let mut tool_defs = tools::all_tool_definitions();
        // Add Anthropic's built-in web search server tool
        tool_defs.push(serde_json::json!({
            "type": "web_search_20250305",
            "name": "web_search"
        }));
        Self {
            api_key,
            client: reqwest::Client::new(),
            system_prompt,
            tools: tool_defs,
        }
    }

    /// Run the agentic conversation loop.
    ///
    /// Sends the user message to the LLM with tool definitions, then iterates:
    /// - If `stop_reason == "end_turn"`, return the text response.
    /// - If `stop_reason == "tool_use"`, execute each tool call and feed results back.
    /// - Otherwise, return what we have or error.
    pub async fn run(
        &self,
        user_message: &str,
        executor: &ToolExecutor<'_>,
        max_iterations: u32,
    ) -> Result<String> {
        let mut messages = vec![AgentMessage {
            role: "user".to_string(),
            content: AgentContent::Text(user_message.to_string()),
        }];

        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut total_tool_calls: u32 = 0;

        for iteration in 0..max_iterations {
            let response = self.call_api(&messages).await?;

            // Accumulate token usage
            if let Some(ref usage) = response.usage {
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
            }

            let stop_reason = response.stop_reason.as_deref().unwrap_or("unknown");

            match stop_reason {
                "end_turn" => {
                    let text = extract_text(&response.content);
                    eprintln!(
                        "Agent completed in {} iteration(s), {} tool call(s), {} input tokens, {} output tokens",
                        iteration + 1,
                        total_tool_calls,
                        total_input_tokens,
                        total_output_tokens
                    );
                    return Ok(text);
                }
                "tool_use" => {
                    // Push the assistant's response (with tool_use blocks) as an assistant message
                    messages.push(AgentMessage {
                        role: "assistant".to_string(),
                        content: AgentContent::Blocks(response.content.clone()),
                    });

                    // Execute each tool call and collect results
                    let mut tool_results: Vec<ContentBlock> = Vec::new();

                    for block in &response.content {
                        // Server tools (web_search) are executed by Anthropic — skip them
                        if let ContentBlock::ServerToolUse { name, input, .. } = block {
                            let truncated_input = {
                                let s = input.to_string();
                                if s.len() > 200 {
                                    format!("{}...", &s[..200])
                                } else {
                                    s
                                }
                            };
                            eprintln!(
                                "  [iter {iteration}] Server tool: {name}({truncated_input}) (handled by Anthropic)"
                            );
                            continue;
                        }
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            total_tool_calls += 1;
                            let truncated_input = {
                                let s = input.to_string();
                                if s.len() > 100 {
                                    format!("{}...", &s[..100])
                                } else {
                                    s
                                }
                            };
                            eprintln!(
                                "  [iter {iteration}] Tool call #{total_tool_calls}: {name}({truncated_input})"
                            );

                            let result = match tools::parse_tool_call(name, input) {
                                Ok(tool_name) => match executor.execute(&tool_name).await {
                                    Ok(output) => ContentBlock::ToolResult {
                                        tool_use_id: id.clone(),
                                        content: output,
                                        is_error: None,
                                    },
                                    Err(e) => ContentBlock::ToolResult {
                                        tool_use_id: id.clone(),
                                        content: format!("Error executing tool: {e}"),
                                        is_error: Some(true),
                                    },
                                },
                                Err(e) => ContentBlock::ToolResult {
                                    tool_use_id: id.clone(),
                                    content: format!("Error parsing tool call: {e}"),
                                    is_error: Some(true),
                                },
                            };

                            tool_results.push(result);
                        }
                    }

                    // Push tool results as a user message
                    messages.push(AgentMessage {
                        role: "user".to_string(),
                        content: AgentContent::Blocks(tool_results),
                    });
                }
                "max_tokens" => {
                    eprintln!("  [iter {iteration}] Stopped: max_tokens reached");
                    let text = extract_text(&response.content);
                    if !text.is_empty() {
                        return Ok(text);
                    }
                    anyhow::bail!("Agent stopped due to max_tokens with no text output");
                }
                other => {
                    eprintln!("  [iter {iteration}] Unexpected stop_reason: {other}");
                    let text = extract_text(&response.content);
                    if !text.is_empty() {
                        return Ok(text);
                    }
                    anyhow::bail!("Agent stopped with unexpected reason: {other}");
                }
            }
        }

        anyhow::bail!("Agent exceeded maximum iterations ({max_iterations})")
    }

    /// Make a single API call to Anthropic with retry logic.
    async fn call_api(&self, messages: &[AgentMessage]) -> Result<AgentResponse> {
        let body = AgentRequest {
            model: MODEL,
            max_tokens: 4096,
            system: self.system_prompt.clone(),
            messages: messages.to_vec(),
            tools: self.tools.clone(),
        };

        let backoffs = [1, 2, 4];
        let mut last_err = None;

        for (attempt, &delay) in std::iter::once(&0).chain(backoffs.iter()).enumerate() {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }

            let result = self
                .client
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await;

            match result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let response: AgentResponse = resp
                            .json()
                            .await
                            .context("Failed to parse Anthropic agent response")?;
                        return Ok(response);
                    }

                    let code = status.as_u16();
                    let body_text = resp.text().await.unwrap_or_default();

                    if code == 429 || code >= 500 {
                        eprintln!("  Anthropic API {code}, retrying...");
                        last_err = Some(anyhow::anyhow!("HTTP {code}: {body_text}"));
                        continue;
                    }

                    anyhow::bail!("Anthropic API error {code}: {body_text}");
                }
                Err(e) => {
                    last_err = Some(e.into());
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Anthropic API failed after retries")))
    }
}

/// Extract all text from content blocks, joining them together.
fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_content_block_text_deserialization() {
        let json = r#"{"type": "text", "text": "Hello world"}"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected Text block"),
        }
    }

    #[test]
    fn test_content_block_tool_use_deserialization() {
        let json = r#"{
            "type": "tool_use",
            "id": "toolu_123",
            "name": "get_league_standings",
            "input": {}
        }"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "get_league_standings");
                assert!(input.is_object());
            }
            _ => panic!("Expected ToolUse block"),
        }
    }

    #[test]
    fn test_content_block_tool_use_with_input() {
        let json = r#"{
            "type": "tool_use",
            "id": "toolu_456",
            "name": "get_team_roster",
            "input": {"team_name": "Touchdown Tyrants"}
        }"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_456");
                assert_eq!(name, "get_team_roster");
                assert_eq!(input["team_name"], "Touchdown Tyrants");
            }
            _ => panic!("Expected ToolUse block"),
        }
    }

    #[test]
    fn test_agent_response_end_turn() {
        let json = r#"{
            "stop_reason": "end_turn",
            "content": [
                {"type": "text", "text": "The standings show Nick is in first place."}
            ]
        }"#;
        let resp: AgentResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        let text = extract_text(&resp.content);
        assert_eq!(text, "The standings show Nick is in first place.");
    }

    #[test]
    fn test_agent_response_tool_use() {
        let json = r#"{
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "Let me look that up."},
                {"type": "tool_use", "id": "toolu_abc", "name": "get_league_standings", "input": {}}
            ]
        }"#;
        let resp: AgentResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.content.len(), 2);

        // Verify we can identify the tool call
        let tool_calls: Vec<_> = resp
            .content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .collect();
        assert_eq!(tool_calls.len(), 1);
    }

    #[test]
    fn test_tool_result_serialization() {
        let result = ContentBlock::ToolResult {
            tool_use_id: "toolu_abc".to_string(),
            content: "1. Nick — 8-2".to_string(),
            is_error: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["tool_use_id"], "toolu_abc");
        assert_eq!(json["content"], "1. Nick — 8-2");
        assert!(json.get("is_error").is_none());
    }

    #[test]
    fn test_tool_result_error_serialization() {
        let result = ContentBlock::ToolResult {
            tool_use_id: "toolu_err".to_string(),
            content: "No team found".to_string(),
            is_error: Some(true),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "tool_result");
        assert_eq!(json["is_error"], true);
    }

    #[test]
    fn test_extract_text_multiple_blocks() {
        let blocks = vec![
            ContentBlock::Text {
                text: "First part. ".to_string(),
            },
            ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "test".to_string(),
                input: serde_json::json!({}),
            },
            ContentBlock::Text {
                text: "Second part.".to_string(),
            },
        ];
        assert_eq!(extract_text(&blocks), "First part. Second part.");
    }

    #[test]
    fn test_agent_message_text_serialization() {
        let msg = AgentMessage {
            role: "user".to_string(),
            content: AgentContent::Text("Hello".to_string()),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn test_agent_message_blocks_serialization() {
        let msg = AgentMessage {
            role: "user".to_string(),
            content: AgentContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "toolu_1".to_string(),
                content: "result data".to_string(),
                is_error: None,
            }]),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        let blocks = json["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
    }
}

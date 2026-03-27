use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::news;
use crate::tools::{self, ToolExecutor};

const MODEL: &str = "gemini-2.5-flash";

// ─── Request types ───

#[derive(Serialize)]
struct AgentRequest {
    system_instruction: SystemInstruction,
    contents: Vec<GeminiMessage>,
    tools: Vec<Value>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct SystemInstruction {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct GenerationConfig {
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct GeminiMessage {
    role: String,
    parts: Vec<Part>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<FunctionCall>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<FunctionResponse>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FunctionCall {
    name: String,
    #[serde(default)]
    args: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct FunctionResponse {
    name: String,
    response: Value,
}

// ─── Response types ───

#[derive(Deserialize, Debug)]
struct GeminiResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<GeminiError>,
    #[serde(rename = "usageMetadata", default)]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize, Debug)]
struct Candidate {
    content: Option<CandidateContent>,
    #[serde(rename = "finishReason", default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct CandidateContent {
    parts: Option<Vec<Part>>,
}

#[derive(Deserialize, Debug)]
struct GeminiError {
    message: String,
}

#[derive(Deserialize, Debug, Default)]
struct UsageMetadata {
    #[serde(rename = "promptTokenCount", default)]
    prompt_token_count: u64,
    #[serde(rename = "candidatesTokenCount", default)]
    candidates_token_count: u64,
}

// ─── GeminiChatAgent ───

pub struct GeminiChatAgent {
    api_key: String,
    client: reqwest::Client,
    system_prompt: String,
    tools: Vec<Value>,
}

impl GeminiChatAgent {
    pub fn new(api_key: String, system_prompt: String) -> Self {
        let mut function_declarations = tools::all_gemini_tool_definitions();
        // Add web_search as a client-side function (Gemini doesn't allow mixing
        // built-in google_search with function calling in the same request).
        function_declarations.push(serde_json::json!({
            "name": "web_search",
            "description": "Search the web for current NFL news, injury updates, trade rumors, breaking stories, or any other real-time information. Use this when you need up-to-date information beyond what the league data tools provide.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    }
                },
                "required": ["query"]
            }
        }));
        let tools = vec![
            serde_json::json!({
                "function_declarations": function_declarations
            }),
        ];
        Self {
            api_key,
            client: reqwest::Client::new(),
            system_prompt,
            tools,
        }
    }

    /// Run the agentic conversation loop, mirroring ChatAgent::run().
    pub async fn run(
        &self,
        user_message: &str,
        executor: &ToolExecutor<'_>,
        max_iterations: u32,
    ) -> Result<String> {
        let mut messages = vec![GeminiMessage {
            role: "user".to_string(),
            parts: vec![Part {
                text: Some(user_message.to_string()),
                function_call: None,
                function_response: None,
            }],
        }];

        let mut total_input_tokens: u64 = 0;
        let mut total_output_tokens: u64 = 0;
        let mut total_tool_calls: u32 = 0;

        for iteration in 0..max_iterations {
            let response = self.call_api(&messages).await?;

            // Accumulate token usage
            if let Some(ref usage) = response.usage_metadata {
                total_input_tokens += usage.prompt_token_count;
                total_output_tokens += usage.candidates_token_count;
            }

            let candidate = response
                .candidates
                .as_ref()
                .and_then(|c| c.first())
                .ok_or_else(|| anyhow::anyhow!("Gemini returned no candidates"))?;

            let finish_reason = candidate
                .finish_reason
                .as_deref()
                .unwrap_or("UNKNOWN");

            let parts = candidate
                .content
                .as_ref()
                .and_then(|c| c.parts.as_ref())
                .cloned()
                .unwrap_or_default();

            // Check if any parts contain function calls
            let function_calls: Vec<&FunctionCall> = parts
                .iter()
                .filter_map(|p| p.function_call.as_ref())
                .collect();

            if function_calls.is_empty() || finish_reason == "STOP" && function_calls.is_empty() {
                // No tool calls — extract text and return
                let text = extract_text(&parts);
                eprintln!(
                    "Gemini agent completed in {} iteration(s), {} tool call(s), {} input tokens, {} output tokens",
                    iteration + 1, total_tool_calls, total_input_tokens, total_output_tokens
                );
                return Ok(text);
            }

            // Push the model's response (with function calls) into conversation
            messages.push(GeminiMessage {
                role: "model".to_string(),
                parts: parts.clone(),
            });

            // Execute each function call and collect responses
            let mut response_parts: Vec<Part> = Vec::new();

            for fc in &function_calls {
                total_tool_calls += 1;
                let truncated_args = {
                    let s = fc.args.to_string();
                    if s.len() > 100 {
                        format!("{}...", &s[..100])
                    } else {
                        s
                    }
                };
                eprintln!(
                    "  [iter {iteration}] Tool call #{total_tool_calls}: {}({truncated_args})",
                    fc.name
                );

                let result = if fc.name == "web_search" {
                    // Handle web_search client-side via Google News RSS
                    let query = fc.args.get("query")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let output = news::web_search(query).await;
                    serde_json::json!({ "content": output })
                } else {
                    match tools::parse_tool_call(&fc.name, &fc.args) {
                        Ok(tool_name) => match executor.execute(&tool_name).await {
                            Ok(output) => serde_json::json!({ "content": output }),
                            Err(e) => serde_json::json!({ "error": format!("Error executing tool: {e}") }),
                        },
                        Err(e) => serde_json::json!({ "error": format!("Error parsing tool call: {e}") }),
                    }
                };

                response_parts.push(Part {
                    text: None,
                    function_call: None,
                    function_response: Some(FunctionResponse {
                        name: fc.name.clone(),
                        response: result,
                    }),
                });
            }

            // Push function responses back as a user message
            messages.push(GeminiMessage {
                role: "user".to_string(),
                parts: response_parts,
            });
        }

        anyhow::bail!("Gemini agent exceeded maximum iterations ({max_iterations})")
    }

    /// Make a single API call to Gemini with retry logic.
    async fn call_api(&self, messages: &[GeminiMessage]) -> Result<GeminiResponse> {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent?key={}",
            self.api_key
        );

        let body = AgentRequest {
            system_instruction: SystemInstruction {
                parts: vec![Part {
                    text: Some(self.system_prompt.clone()),
                    function_call: None,
                    function_response: None,
                }],
            },
            contents: messages.to_vec(),
            tools: self.tools.clone(),
            generation_config: GenerationConfig {
                max_output_tokens: 4096,
            },
        };

        let backoffs = [1, 2, 4];
        let mut last_err = None;

        for (attempt, &delay) in std::iter::once(&0).chain(backoffs.iter()).enumerate() {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
            }

            let result = self
                .client
                .post(&url)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await;

            match result {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        let response: GeminiResponse = resp
                            .json()
                            .await
                            .context("Failed to parse Gemini agent response")?;

                        if let Some(ref err) = response.error {
                            anyhow::bail!("Gemini API error: {}", err.message);
                        }

                        return Ok(response);
                    }

                    let code = status.as_u16();
                    let body_text = resp.text().await.unwrap_or_default();

                    if code == 429 || code >= 500 {
                        eprintln!("  Gemini API {code}, retrying...");
                        last_err = Some(anyhow::anyhow!("HTTP {code}: {body_text}"));
                        continue;
                    }

                    anyhow::bail!("Gemini API error {code}: {body_text}");
                }
                Err(e) => {
                    last_err = Some(e.into());
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Gemini API failed after retries")))
    }
}

/// Extract all text from response parts.
fn extract_text(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_part_text_serialization() {
        let part = Part {
            text: Some("Hello".to_string()),
            function_call: None,
            function_response: None,
        };
        let json = serde_json::to_value(&part).unwrap();
        assert_eq!(json["text"], "Hello");
        assert!(json.get("functionCall").is_none());
        assert!(json.get("functionResponse").is_none());
    }

    #[test]
    fn test_part_function_call_serialization() {
        let part = Part {
            text: None,
            function_call: Some(FunctionCall {
                name: "get_league_standings".to_string(),
                args: serde_json::json!({}),
            }),
            function_response: None,
        };
        let json = serde_json::to_value(&part).unwrap();
        assert!(json.get("text").is_none());
        assert_eq!(json["functionCall"]["name"], "get_league_standings");
    }

    #[test]
    fn test_part_function_response_serialization() {
        let part = Part {
            text: None,
            function_call: None,
            function_response: Some(FunctionResponse {
                name: "get_league_standings".to_string(),
                response: serde_json::json!({"content": "1. Nick — 8-2"}),
            }),
        };
        let json = serde_json::to_value(&part).unwrap();
        assert!(json.get("text").is_none());
        assert_eq!(json["functionResponse"]["name"], "get_league_standings");
        assert_eq!(json["functionResponse"]["response"]["content"], "1. Nick — 8-2");
    }

    #[test]
    fn test_extract_text_from_parts() {
        let parts = vec![
            Part {
                text: Some("First. ".to_string()),
                function_call: None,
                function_response: None,
            },
            Part {
                text: None,
                function_call: Some(FunctionCall {
                    name: "test".to_string(),
                    args: serde_json::json!({}),
                }),
                function_response: None,
            },
            Part {
                text: Some("Second.".to_string()),
                function_call: None,
                function_response: None,
            },
        ];
        assert_eq!(extract_text(&parts), "First. Second.");
    }

    #[test]
    fn test_gemini_message_serialization() {
        let msg = GeminiMessage {
            role: "user".to_string(),
            parts: vec![Part {
                text: Some("Hello".to_string()),
                function_call: None,
                function_response: None,
            }],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["parts"][0]["text"], "Hello");
    }

    #[test]
    fn test_function_call_deserialization() {
        let json = r#"{"name": "get_team_roster", "args": {"team_name": "Nick"}}"#;
        let fc: FunctionCall = serde_json::from_str(json).unwrap();
        assert_eq!(fc.name, "get_team_roster");
        assert_eq!(fc.args["team_name"], "Nick");
    }

    #[test]
    fn test_gemini_response_deserialization() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [{"text": "The standings show Nick is first."}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 50
            }
        }"#;
        let resp: GeminiResponse = serde_json::from_str(json).unwrap();
        let candidate = resp.candidates.unwrap();
        assert_eq!(candidate[0].finish_reason.as_deref(), Some("STOP"));
        let parts = candidate[0].content.as_ref().unwrap().parts.as_ref().unwrap();
        assert_eq!(extract_text(parts), "The standings show Nick is first.");
        assert_eq!(resp.usage_metadata.unwrap().prompt_token_count, 100);
    }

    #[test]
    fn test_gemini_response_with_function_call() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Let me look that up."},
                        {"functionCall": {"name": "get_league_standings", "args": {}}}
                    ]
                },
                "finishReason": "STOP"
            }]
        }"#;
        let resp: GeminiResponse = serde_json::from_str(json).unwrap();
        let candidates = resp.candidates.unwrap();
        let parts = candidates[0]
            .content
            .as_ref()
            .unwrap()
            .parts
            .as_ref()
            .unwrap();
        let fc_count = parts.iter().filter(|p| p.function_call.is_some()).count();
        assert_eq!(fc_count, 1);
    }
}

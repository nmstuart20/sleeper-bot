use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::llm::TradeAnalyzer;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-sonnet-4-20250514";

pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: String,
    trade_system_prompt: String,
}

/// Kept for reference; the owned variant is used at runtime.
#[derive(Serialize)]
#[allow(dead_code)]
struct Request {
    model: &'static str,
    max_tokens: u32,
    system: &'static str,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct Response {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

impl AnthropicClient {
    pub fn new(api_key: String, trade_system_prompt: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            trade_system_prompt,
        }
    }
}

/// Request with owned system prompt string (for generate method).
#[derive(Serialize)]
struct RequestOwned {
    model: &'static str,
    max_tokens: u32,
    system: String,
    messages: Vec<Message>,
}

impl AnthropicClient {
    async fn call_anthropic(&self, system: &str, user: &str) -> Result<String> {
        let body = RequestOwned {
            model: MODEL,
            max_tokens: 3000,
            system: system.to_string(),
            messages: vec![Message {
                role: "user",
                content: user.to_string(),
            }],
        };

        let mut last_err = None;
        let backoffs = [1, 2, 4];

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
                        let response: Response = resp
                            .json()
                            .await
                            .context("Failed to parse Anthropic response")?;
                        let text = response
                            .content
                            .into_iter()
                            .filter_map(|b| b.text)
                            .collect::<Vec<_>>()
                            .join("");
                        return Ok(text);
                    }

                    let code = status.as_u16();
                    let body_text = resp.text().await.unwrap_or_default();

                    if code == 429 || code >= 500 {
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

impl TradeAnalyzer for AnthropicClient {
    fn analyze_trade(
        &self,
        user_prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        let prompt = user_prompt.to_string();
        Box::pin(async move {
            self.call_anthropic(&self.trade_system_prompt, &prompt)
                .await
        })
    }

    fn generate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        let system = system_prompt.to_string();
        let user = user_prompt.to_string();
        Box::pin(async move { self.call_anthropic(&system, &user).await })
    }
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::llm::{SYSTEM_PROMPT, TradeAnalyzer};

const MODEL: &str = "gemini-2.5-flash";

pub struct GeminiClient {
    client: reqwest::Client,
    api_key: String,
}

#[derive(Serialize)]
struct Request {
    system_instruction: SystemInstruction,
    contents: Vec<Content>,
    #[serde(rename = "generationConfig")]
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct SystemInstruction {
    parts: Vec<Part>,
}

#[derive(Serialize)]
struct Content {
    role: &'static str,
    parts: Vec<Part>,
}

#[derive(Serialize, Deserialize)]
struct Part {
    text: String,
}

#[derive(Serialize)]
struct GenerationConfig {
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
}

#[derive(Deserialize)]
struct Response {
    candidates: Option<Vec<Candidate>>,
    error: Option<GeminiError>,
}

#[derive(Deserialize)]
struct Candidate {
    content: Option<CandidateContent>,
}

#[derive(Deserialize)]
struct CandidateContent {
    parts: Option<Vec<Part>>,
}

#[derive(Deserialize)]
struct GeminiError {
    message: String,
}

impl GeminiClient {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
        }
    }

    fn api_url(&self) -> String {
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent?key={}",
            self.api_key
        )
    }
}

impl TradeAnalyzer for GeminiClient {
    fn analyze_trade(
        &self,
        user_prompt: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        let prompt = user_prompt.to_string();
        Box::pin(async move {
            let body = Request {
                system_instruction: SystemInstruction {
                    parts: vec![Part {
                        text: SYSTEM_PROMPT.to_string(),
                    }],
                },
                contents: vec![Content {
                    role: "user",
                    parts: vec![Part { text: prompt }],
                }],
                generation_config: GenerationConfig {
                    max_output_tokens: 3000,
                },
            };

            let mut last_err = None;
            let backoffs = [1, 2, 4];

            for (attempt, &delay) in std::iter::once(&0).chain(backoffs.iter()).enumerate() {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                }

                let result = self
                    .client
                    .post(self.api_url())
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
                                .context("Failed to parse Gemini response")?;

                            if let Some(err) = response.error {
                                anyhow::bail!("Gemini API error: {}", err.message);
                            }

                            let text = response
                                .candidates
                                .unwrap_or_default()
                                .into_iter()
                                .filter_map(|c| c.content)
                                .flat_map(|c| c.parts.unwrap_or_default())
                                .map(|p| p.text)
                                .collect::<Vec<_>>()
                                .join("");

                            if text.is_empty() {
                                anyhow::bail!("Gemini returned empty response");
                            }

                            return Ok(text);
                        }

                        let code = status.as_u16();
                        let body_text = resp.text().await.unwrap_or_default();

                        if code == 429 || code >= 500 {
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
        })
    }
}

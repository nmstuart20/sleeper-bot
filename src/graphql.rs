use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const GRAPHQL_URL: &str = "https://sleeper.com/graphql";

/// Warn when the token expires within this many days.
const TOKEN_EXPIRY_WARNING_DAYS: i64 = 30;

const CREATE_MESSAGE_MUTATION: &str = r#"mutation create_message($text: String!, $parent_type: String!, $parent_id: Snowflake!) {
  create_message(text: $text, parent_type: $parent_type, parent_id: $parent_id) {
    message_id
    author_display_name
    created
  }
}"#;

const GET_MESSAGES_QUERY: &str = r#"query get_messages($parent_type: String!, $parent_id: Snowflake!, $message_id: Snowflake) {
  get_messages(parent_type: $parent_type, parent_id: $parent_id, message_id: $message_id) {
    message_id
    author_id
    author_display_name
    text
    created
  }
}"#;

#[derive(Serialize)]
struct GraphqlRequest {
    #[serde(rename = "operationName")]
    operation_name: &'static str,
    query: &'static str,
    variables: serde_json::Value,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct SendMessageResponse {
    data: Option<serde_json::Value>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

/// A chat message fetched from a league channel.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ChatMessage {
    pub message_id: Option<String>,
    pub author_id: Option<String>,
    pub author_display_name: Option<String>,
    pub text: Option<String>,
    pub created: Option<i64>,
}

#[derive(Deserialize)]
struct GetMessagesResponse {
    data: Option<GetMessagesData>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GetMessagesData {
    get_messages: Option<Vec<ChatMessage>>,
}

/// JWT claims we care about for expiry checking.
#[derive(Deserialize)]
struct JwtClaims {
    exp: Option<i64>,
    user_id: Option<String>,
}

pub struct SleeperGraphql {
    client: reqwest::Client,
    token: String,
}

impl SleeperGraphql {
    /// Create a client with a token from SLEEPER_TOKEN env var.
    /// Checks expiry and warns if the token is close to expiring or already expired.
    pub fn new(token: String) -> Result<Self> {
        // Validate and check expiry
        match decode_jwt_expiry(&token) {
            Ok(Some(exp)) => {
                let now = chrono::Utc::now().timestamp();
                let days_remaining = (exp - now) / 86400;

                if days_remaining < 0 {
                    anyhow::bail!(
                        "SLEEPER_TOKEN has expired! \
                        Log into sleeper.app in your browser, grab a fresh token from \
                        DevTools → Application → Local Storage, and update SLEEPER_TOKEN in your .env file."
                    );
                } else if days_remaining <= TOKEN_EXPIRY_WARNING_DAYS {
                    let expiry_date = chrono::DateTime::from_timestamp(exp, 0).unwrap_or_default();
                    eprintln!(
                        "⚠️  WARNING: SLEEPER_TOKEN expires in {days_remaining} day(s) (on {}).",
                        expiry_date.format("%B %d, %Y")
                    );
                    eprintln!(
                        "   Grab a fresh token from sleeper.app → DevTools → Application → Local Storage."
                    );
                } else {
                    let expiry_date = chrono::DateTime::from_timestamp(exp, 0).unwrap_or_default();
                    println!(
                        "Token valid for {days_remaining} day(s) (expires {}).",
                        expiry_date.format("%B %d, %Y")
                    );
                }
            }
            Ok(None) => {
                eprintln!(
                    "Warning: could not read token expiry — unable to check if it's still valid."
                );
            }
            Err(e) => {
                eprintln!("Warning: could not decode token ({e}) — unable to check expiry.");
            }
        }

        Ok(Self {
            client: reqwest::Client::new(),
            token,
        })
    }

    pub async fn send_message(&self, league_id: &str, message: &str) -> Result<()> {
        let req = GraphqlRequest {
            operation_name: "create_message",
            query: CREATE_MESSAGE_MUTATION,
            variables: serde_json::json!({
                "text": message,
                "parent_type": "league",
                "parent_id": league_id,
            }),
        };

        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Authorization", &self.token)
            .json(&req)
            .send()
            .await
            .context("Failed to send message to Sleeper")?;

        let status = resp.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "Sleeper returned 401 Unauthorized. Your token may have expired.\n\
                Grab a fresh token from sleeper.app → DevTools → Application → Local Storage \
                and update SLEEPER_TOKEN in your .env file."
            );
        }

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to send message (HTTP {status}): {body}");
        }

        let msg_resp: SendMessageResponse = resp
            .json()
            .await
            .context("Failed to parse create_message response")?;

        if let Some(errors) = msg_resp.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            anyhow::bail!("GraphQL error sending message: {}", msgs.join(", "));
        }

        Ok(())
    }

    /// Fetch recent messages from a league chat.
    /// If `after_message_id` is provided, fetches messages after that cursor.
    pub async fn fetch_messages(
        &self,
        league_id: &str,
        after_message_id: Option<&str>,
    ) -> Result<Vec<ChatMessage>> {
        let mut variables = serde_json::json!({
            "parent_type": "league",
            "parent_id": league_id,
        });

        if let Some(msg_id) = after_message_id {
            variables["message_id"] = serde_json::Value::String(msg_id.to_string());
        }

        let req = serde_json::json!({
            "operationName": "get_messages",
            "query": GET_MESSAGES_QUERY,
            "variables": variables,
        });

        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Authorization", &self.token)
            .json(&req)
            .send()
            .await
            .context("Failed to fetch messages from Sleeper")?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!("Sleeper returned 401 Unauthorized fetching messages.");
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Failed to fetch messages (HTTP {status}): {body}");
        }

        let msg_resp: GetMessagesResponse = resp
            .json()
            .await
            .context("Failed to parse get_messages response")?;

        if let Some(errors) = msg_resp.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            anyhow::bail!("GraphQL error fetching messages: {}", msgs.join(", "));
        }

        Ok(msg_resp
            .data
            .and_then(|d| d.get_messages)
            .unwrap_or_default())
    }

    /// Extract the bot's user_id from the JWT token.
    pub fn bot_user_id(&self) -> Option<String> {
        decode_jwt_user_id(&self.token).ok().flatten()
    }

    pub fn is_authenticated(&self) -> bool {
        !self.token.is_empty()
    }
}

/// Decode JWT claims without verifying the signature.
fn decode_jwt_claims(token: &str) -> Result<JwtClaims> {
    use base64::prelude::*;

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        anyhow::bail!("Invalid JWT format (expected 3 parts, got {})", parts.len());
    }

    let payload_bytes = BASE64_URL_SAFE_NO_PAD
        .decode(parts[1])
        .context("Failed to base64-decode JWT payload")?;

    serde_json::from_slice(&payload_bytes).context("Failed to parse JWT claims")
}

/// Decode the expiry timestamp from a JWT without verifying the signature.
fn decode_jwt_expiry(token: &str) -> Result<Option<i64>> {
    Ok(decode_jwt_claims(token)?.exp)
}

/// Decode the user_id from a JWT without verifying the signature.
fn decode_jwt_user_id(token: &str) -> Result<Option<String>> {
    Ok(decode_jwt_claims(token)?.user_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_jwt_expiry() {
        // Craft a minimal JWT: header.payload.signature
        // payload = {"exp": 1805745375}
        use base64::prelude::*;
        let header = BASE64_URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = BASE64_URL_SAFE_NO_PAD.encode(r#"{"exp":1805745375,"user_id":"123"}"#);
        let token = format!("{header}.{payload}.fakesignature");

        let exp = decode_jwt_expiry(&token).unwrap();
        assert_eq!(exp, Some(1805745375));
    }

    #[test]
    fn test_decode_jwt_no_exp() {
        use base64::prelude::*;
        let header = BASE64_URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256"}"#);
        let payload = BASE64_URL_SAFE_NO_PAD.encode(r#"{"user_id":"123"}"#);
        let token = format!("{header}.{payload}.fakesig");

        let exp = decode_jwt_expiry(&token).unwrap();
        assert_eq!(exp, None);
    }

    #[test]
    fn test_decode_jwt_invalid() {
        assert!(decode_jwt_expiry("not-a-jwt").is_err());
    }
}

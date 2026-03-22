use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const GRAPHQL_URL: &str = "https://sleeper.com/graphql";
const TOKEN_FILE: &str = ".sleeper_token";
const TOKEN_MAX_AGE_HOURS: i64 = 24;

const LOGIN_QUERY: &str = r#"query login_query($email_or_phone_or_username: String!, $password: String) {
  login(email_or_phone_or_username: $email_or_phone_or_username, password: $password) {
    token
    avatar
    cookies
    created
    display_name
    real_name
    email
    notifications
    phone
    user_id
    verification
    data_updated
  }
}"#;

const CREATE_MESSAGE_MUTATION: &str = r#"mutation create_message($text: String!, $parent_type: String!, $parent_id: Snowflake!) {
  create_message(text: $text, parent_type: $parent_type, parent_id: $parent_id) {
    message_id
    author_display_name
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
struct LoginResponse {
    data: Option<LoginData>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct LoginData {
    login: Option<LoginResult>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct LoginResult {
    token: Option<String>,
    user_id: Option<String>,
    display_name: Option<String>,
    avatar: Option<String>,
    email: Option<String>,
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

#[derive(Serialize, Deserialize)]
struct CachedToken {
    token: String,
    created_at: i64,
}

pub struct SleeperGraphql {
    client: reqwest::Client,
    token: Option<String>,
    username: String,
    password: String,
}

impl SleeperGraphql {
    pub fn new(username: String, password: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: None,
            username,
            password,
        }
    }

    /// Create a client with a pre-existing token, skipping login entirely.
    /// Use this when you have a token from browser DevTools.
    pub fn with_token(token: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: Some(token),
            username: String::new(),
            password: String::new(),
        }
    }

    fn load_cached_token() -> Option<String> {
        let path = Path::new(TOKEN_FILE);
        if !path.exists() {
            return None;
        }
        let data = std::fs::read_to_string(path).ok()?;
        let cached: CachedToken = serde_json::from_str(&data).ok()?;
        let now = chrono::Utc::now().timestamp();
        if now - cached.created_at > TOKEN_MAX_AGE_HOURS * 3600 {
            return None;
        }
        Some(cached.token)
    }

    fn save_token(token: &str) {
        let cached = CachedToken {
            token: token.to_string(),
            created_at: chrono::Utc::now().timestamp(),
        };
        if let Ok(json) = serde_json::to_string(&cached) {
            let _ = std::fs::write(TOKEN_FILE, json);
        }
    }

    pub async fn login(&mut self) -> Result<()> {
        // Try cached token first
        if let Some(token) = Self::load_cached_token() {
            self.token = Some(token);
            return Ok(());
        }

        let req = GraphqlRequest {
            operation_name: "login_query",
            query: LOGIN_QUERY,
            variables: serde_json::json!({
                "email_or_phone_or_username": self.username,
                "password": self.password,
            }),
        };

        let resp = self
            .client
            .post(GRAPHQL_URL)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&req)
            .send()
            .await
            .context("Failed to connect to Sleeper GraphQL API")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Sleeper login failed (HTTP {status}): {body}\n\
                Check SLEEPER_USERNAME and SLEEPER_PASSWORD."
            );
        }

        let login_resp: LoginResponse = resp
            .json()
            .await
            .context("Failed to parse login response")?;

        if let Some(errors) = login_resp.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            anyhow::bail!(
                "Sleeper login error: {}\n\
                Check SLEEPER_USERNAME and SLEEPER_PASSWORD.",
                msgs.join(", ")
            );
        }

        let token = login_resp
            .data
            .and_then(|d| d.login)
            .and_then(|l| l.token)
            .context(
                "No token in login response. \
                Check SLEEPER_USERNAME and SLEEPER_PASSWORD.",
            )?;

        Self::save_token(&token);
        self.token = Some(token);
        Ok(())
    }

    pub async fn send_message(&mut self, league_id: &str, message: &str) -> Result<()> {
        let token = self
            .token
            .as_ref()
            .context("Not logged in — call login() first")?
            .clone();

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
            .header("Authorization", token.to_string())
            .json(&req)
            .send()
            .await
            .context("Failed to send message to Sleeper")?;

        let status = resp.status();

        // If 401, try re-login once
        if status == reqwest::StatusCode::UNAUTHORIZED {
            let _ = std::fs::remove_file(TOKEN_FILE);
            self.token = None;
            self.login().await?;
            return Box::pin(self.send_message(league_id, message)).await;
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

    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[ignore]
    #[tokio::test]
    async fn test_graphql_login() {
        dotenvy::dotenv().ok();
        let username = std::env::var("SLEEPER_USERNAME").expect("SLEEPER_USERNAME required");
        let password = std::env::var("SLEEPER_PASSWORD").expect("SLEEPER_PASSWORD required");

        let mut gql = SleeperGraphql::new(username, password);
        gql.login().await.unwrap();
        assert!(gql.is_authenticated());
    }
}

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::RwLock;

/// Global Scrappey client, initialized if SCRAPPEY_API_KEY is set
pub static SCRAPPEY: OnceLock<ScrappeyClient> = OnceLock::new();

/// Global Scrappey session ID (persists cookies across requests)
static SC_SESSION_ID: OnceLock<RwLock<Option<String>>> = OnceLock::new();

/// Global Scrappey User-Agent (must match when doing wreq calls with CF cookies)
static SC_USER_AGENT: OnceLock<RwLock<Option<String>>> = OnceLock::new();

/// Global Scrappey cookies from login (injected via cookiejar dans chaque requête)
static SC_COOKIES: OnceLock<RwLock<Vec<ScrappeyCookie>>> = OnceLock::new();

fn get_session_lock() -> &'static RwLock<Option<String>> {
    SC_SESSION_ID.get_or_init(|| RwLock::new(None))
}

fn get_user_agent_lock() -> &'static RwLock<Option<String>> {
    SC_USER_AGENT.get_or_init(|| RwLock::new(None))
}

fn get_cookies_lock() -> &'static RwLock<Vec<ScrappeyCookie>> {
    SC_COOKIES.get_or_init(|| RwLock::new(Vec::new()))
}

/// Client Scrappey pour bypasser Cloudflare en fallback
pub struct ScrappeyClient {
    api_key: String,
    client: wreq::Client,
}

const SCRAPPEY_ENDPOINT: &str = "https://publisher.scrappey.com/api/v1";

// --- Request structs ---

#[derive(Debug, Serialize)]
struct ScrappeySessionRequest<'a> {
    cmd: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
}

// --- Response structs ---

#[derive(Debug, Deserialize)]
pub struct ScrappeyResponse {
    pub solution: Option<ScrappeyRawSolution>,
    pub data: Option<String>,
    pub session: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct ScrappeyRawSolution {
    #[serde(default)]
    pub verified: bool,
    #[serde(rename = "currentUrl", default)]
    pub current_url: String,
    #[serde(rename = "statusCode", default)]
    pub status_code: u16,
    #[serde(rename = "userAgent", default)]
    pub user_agent: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub cookies: Vec<ScrappeyRawCookie>,
    #[serde(rename = "cookieString", default)]
    pub cookie_string: String,
    #[serde(rename = "innerText", default)]
    pub inner_text: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ScrappeyRawCookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub path: String,
}

/// Unified solution type matching the old FlareSolverr interface
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ScrappeySolution {
    pub url: String,
    pub status: u16,
    pub cookies: Vec<ScrappeyCookie>,
    pub user_agent: String,
    pub response: String,
}

#[derive(Debug, Clone)]
pub struct ScrappeyCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
}

impl From<ScrappeyRawSolution> for ScrappeySolution {
    fn from(raw: ScrappeyRawSolution) -> Self {
        Self {
            url: raw.current_url,
            status: raw.status_code,
            cookies: raw
                .cookies
                .into_iter()
                .map(|c| ScrappeyCookie {
                    name: c.name,
                    value: c.value,
                    domain: c.domain,
                    path: if c.path.is_empty() {
                        "/".to_string()
                    } else {
                        c.path
                    },
                })
                .collect(),
            user_agent: raw.user_agent,
            response: raw.response,
        }
    }
}

impl ScrappeyClient {
    pub fn new(api_key: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let client = wreq::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        Ok(Self {
            api_key: api_key.to_string(),
            client,
        })
    }

    /// Initialize the global Scrappey client
    pub fn init_global(api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
        let client = Self::new(api_key)?;
        SCRAPPEY
            .set(client)
            .map_err(|_| "Scrappey already initialized")?;
        Ok(())
    }

    pub fn is_available() -> bool {
        SCRAPPEY.get().is_some()
    }

    fn endpoint(&self) -> String {
        format!("{}?key={}", SCRAPPEY_ENDPOINT, self.api_key)
    }

    /// Create a persistent Scrappey session (reuses browser + cookies)
    pub async fn create_session(&self) -> Result<String, Box<dyn std::error::Error>> {
        let body = ScrappeySessionRequest {
            cmd: "sessions.create",
            session: None,
        };

        debug!("Scrappey: creating persistent session...");
        let response = self
            .client
            .post(&self.endpoint())
            .json(&body)
            .send()
            .await?;
        let sc_resp: ScrappeyResponse = response.json().await?;

        if sc_resp.data.as_deref() != Some("success") {
            let err_msg = sc_resp
                .error
                .unwrap_or_else(|| "Unknown error creating session".to_string());
            return Err(format!("Scrappey session creation failed: {}", err_msg).into());
        }

        let session_id = sc_resp
            .session
            .ok_or("Scrappey did not return a session ID")?;
        info!("Scrappey session created: {}", session_id);

        // Store globally
        let mut lock = get_session_lock().write().await;
        *lock = Some(session_id.clone());

        Ok(session_id)
    }

    pub async fn set_user_agent(ua: String) {
        let mut lock = get_user_agent_lock().write().await;
        *lock = Some(ua);
    }

    pub async fn get_user_agent() -> Option<String> {
        let lock = get_user_agent_lock().read().await;
        lock.clone()
    }

    /// Stocker les cookies authentifiés (post-login) pour les réutiliser
    pub async fn set_cookies(cookies: Vec<ScrappeyCookie>) {
        let mut lock = get_cookies_lock().write().await;
        *lock = cookies;
    }

    /// Récupérer les cookies stockés
    pub async fn get_cookies() -> Vec<ScrappeyCookie> {
        let lock = get_cookies_lock().read().await;
        lock.clone()
    }

    /// Construire le cookiejar JSON à partir des cookies stockés
    pub async fn build_cookiejar() -> Option<serde_json::Value> {
        let cookies = Self::get_cookies().await;
        if cookies.is_empty() {
            return None;
        }
        let jar: Vec<serde_json::Value> = cookies
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "value": c.value,
                    "domain": c.domain,
                    "path": c.path
                })
            })
            .collect();
        Some(serde_json::Value::Array(jar))
    }

    /// Fetch a page via Scrappey using the persistent session
    pub async fn fetch_page(url: &str) -> Result<String, Box<dyn std::error::Error>> {
        Self::fetch_page_with_solution(url)
            .await
            .map(|s| s.response)
    }

    /// Fetch a page via Scrappey, returning the full solution (with cookies + user-agent)
    /// Injecte automatiquement les cookies authentifiés via cookiejar
    pub async fn fetch_page_with_solution(
        url: &str,
    ) -> Result<ScrappeySolution, Box<dyn std::error::Error>> {
        let sc = SCRAPPEY
            .get()
            .ok_or("Scrappey not configured (set SCRAPPEY_API_KEY)")?;

        // Construire la requête avec cookiejar et User-Agent
        let mut request = serde_json::json!({
            "cmd": "request.get",
            "url": url
        });

        // Ajouter le cookiejar si des cookies authentifiés existent
        if let Some(jar) = Self::build_cookiejar().await {
            request["cookiejar"] = jar;
            debug!("Scrappey: injecting {} auth cookies via cookiejar",
                   request["cookiejar"].as_array().map(|a| a.len()).unwrap_or(0));
        }

        // Matcher le User-Agent du login CF
        if let Some(ua) = Self::get_user_agent().await {
            request["customHeaders"] = serde_json::json!({
                "user-agent": ua
            });
        }

        sc.raw_request(&request).await
    }

    /// Send a raw JSON request to Scrappey API (for browserActions, etc.)
    pub async fn raw_request(
        &self,
        body: &serde_json::Value,
    ) -> Result<ScrappeySolution, Box<dyn std::error::Error>> {
        info!(
            "Scrappey: raw request (cmd: {:?})",
            body.get("cmd").and_then(|v| v.as_str())
        );

        let response = self
            .client
            .post(&self.endpoint())
            .json(body)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(format!("Scrappey raw request returned HTTP {}", response.status()).into());
        }

        let sc_response: ScrappeyResponse = response.json().await?;
        Self::extract_solution(sc_response)
    }

    fn extract_solution(
        sc_response: ScrappeyResponse,
    ) -> Result<ScrappeySolution, Box<dyn std::error::Error>> {
        if sc_response.data.as_deref() != Some("success") {
            let msg = sc_response
                .error
                .unwrap_or_else(|| "Unknown error".to_string());
            return Err(format!("Scrappey error: {}", msg).into());
        }

        let raw_solution = sc_response
            .solution
            .ok_or("Scrappey returned no solution")?;

        if !raw_solution.verified {
            warn!(
                "Scrappey solution not verified (URL: {})",
                raw_solution.current_url
            );
        }

        debug!(
            "Scrappey solution: status={}, url={}, response_len={}, cookies={}",
            raw_solution.status_code,
            raw_solution.current_url,
            raw_solution.response.len(),
            raw_solution.cookies.len()
        );

        Ok(ScrappeySolution::from(raw_solution))
    }
}

/// Centralized function to fetch a YGG page.
/// Tries wreq first, falls back to Scrappey (with persistent session) if CF blocks.
pub async fn fetch_ygg_page(
    client: &wreq::Client,
    url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    // Try wreq first
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                return response
                    .text()
                    .await
                    .map_err(|e| format!("Failed to read response body: {}", e).into());
            }

            // CF block — fallback to Scrappey with session
            if status.as_u16() == 307
                || status.as_u16() == 302
                || status.as_u16() == 403
                || status.as_u16() == 503
            {
                warn!(
                    "wreq blocked by CF (HTTP {}) for {} — falling back to Scrappey",
                    status, url
                );
                if ScrappeyClient::is_available() {
                    let html = ScrappeyClient::fetch_page(url).await?;
                    debug!(
                        "Scrappey fallback returned {} bytes for {}",
                        html.len(),
                        url
                    );
                    return Ok(html);
                } else {
                    return Err(format!(
                        "CF blocked (HTTP {}) and Scrappey not configured",
                        status
                    )
                    .into());
                }
            }

            Err(format!("HTTP error {} for {}", status, url).into())
        }
        Err(e) => {
            warn!(
                "wreq request failed for {}: {} — trying Scrappey",
                url, e
            );
            if ScrappeyClient::is_available() {
                let html = ScrappeyClient::fetch_page(url).await?;
                debug!(
                    "Scrappey fallback returned {} bytes for {}",
                    html.len(),
                    url
                );
                return Ok(html);
            } else {
                Err(format!("Request failed and Scrappey not configured: {}", e).into())
            }
        }
    }
}


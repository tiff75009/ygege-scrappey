use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tokio::sync::RwLock;

/// Global Scrappey client, initialized if SCRAPPEY_API_KEY is set
pub static SCRAPPEY: OnceLock<ScrappeyClient> = OnceLock::new();

/// Persistent profile ID for Scrappey (survives across sessions indefinitely)
pub const PROFILE_ID: &str = "ygg-dl-v2";

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
    #[serde(rename = "javascriptReturn", default)]
    pub javascript_return: Vec<serde_json::Value>,
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
    pub javascript_return: Vec<serde_json::Value>,
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
            javascript_return: raw.javascript_return,
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

    /// Récupérer la session ID stockée
    pub async fn get_session_id() -> Option<String> {
        let lock = get_session_lock().read().await;
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

    /// Fetch a page via Scrappey, returning the full solution (with cookies + user-agent).
    /// Automatically recreates session if the browser was closed (session expired).
    pub async fn fetch_page_with_solution(
        url: &str,
    ) -> Result<ScrappeySolution, Box<dyn std::error::Error>> {
        match Self::fetch_page_with_solution_inner(url).await {
            Ok(solution) => Ok(solution),
            Err(e) if Self::is_session_expired_error(e.as_ref()) => {
                warn!("Scrappey session expired ({}), recreating session...", e);
                Self::recreate_session().await?;
                // Retry with the new session
                Self::fetch_page_with_solution_inner(url).await
            }
            Err(e) => Err(e),
        }
    }

    async fn fetch_page_with_solution_inner(
        url: &str,
    ) -> Result<ScrappeySolution, Box<dyn std::error::Error>> {
        let sc = SCRAPPEY
            .get()
            .ok_or("Scrappey not configured (set SCRAPPEY_API_KEY)")?;

        let request = Self::build_request("request.get", url, None).await;

        sc.raw_request(&request).await
    }

    /// Build a Scrappey request with profileId, session, cookiejar, and user-agent
    pub async fn build_request(cmd: &str, url: &str, extra: Option<&serde_json::Value>) -> serde_json::Value {
        let mut request = serde_json::json!({
            "cmd": cmd,
            "url": url,
            "profileId": PROFILE_ID,
            "proxyCountry": "FR"
        });

        // Reuse login session (same browser = no new CF challenge)
        if let Some(session_id) = Self::get_session_id().await {
            request["session"] = serde_json::Value::String(session_id);
        }

        // Inject auth cookies
        if let Some(jar) = Self::build_cookiejar().await {
            debug!("Scrappey: injecting {} auth cookies via cookiejar",
                   jar.as_array().map(|a| a.len()).unwrap_or(0));
            request["cookiejar"] = jar;
        }

        // Match user-agent from login
        if let Some(ua) = Self::get_user_agent().await {
            request["customHeaders"] = serde_json::json!({
                "user-agent": ua
            });
        }

        // Merge extra fields if provided
        if let Some(extra) = extra {
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    request[k] = v.clone();
                }
            }
        }

        request
    }

    /// Check if an error indicates the Scrappey session/browser has expired
    fn is_session_expired_error(e: &dyn std::error::Error) -> bool {
        let msg = e.to_string();
        msg.contains("browser has been closed")
            || msg.contains("Target page")
            || msg.contains("session")
            || msg.contains("context or browser")
    }

    /// Recreate a new Scrappey session, preserving cookies and user-agent
    async fn recreate_session() -> Result<(), Box<dyn std::error::Error>> {
        let sc = SCRAPPEY
            .get()
            .ok_or("Scrappey not configured")?;

        info!("Scrappey: recreating session (previous browser was closed)...");
        let _session_id = sc.create_session().await?;
        info!("Scrappey: new session created, cookies will be re-injected via cookiejar");
        Ok(())
    }

    /// Execute a raw Scrappey request with automatic session retry on expiration
    pub async fn raw_request_with_retry(
        body: &serde_json::Value,
    ) -> Result<ScrappeySolution, Box<dyn std::error::Error>> {
        let sc = SCRAPPEY
            .get()
            .ok_or("Scrappey not configured")?;

        match sc.raw_request(body).await {
            Ok(solution) => Ok(solution),
            Err(e) if Self::is_session_expired_error(e.as_ref()) => {
                warn!("Scrappey session expired during raw request, recreating...");
                Self::recreate_session().await?;

                // Rebuild the request with new session ID
                let mut new_body = body.clone();
                if let Some(session_id) = Self::get_session_id().await {
                    new_body["session"] = serde_json::Value::String(session_id);
                }
                sc.raw_request(&new_body).await
            }
            Err(e) => Err(e),
        }
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

/// Centralized function to fetch a YGG page via Scrappey (with persistent session).
/// wreq is no longer used for direct YGG access (CF blocks everything).
pub async fn fetch_ygg_page(
    _client: &wreq::Client,
    url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if !ScrappeyClient::is_available() {
        return Err("Scrappey not configured (set SCRAPPEY_API_KEY)".into());
    }

    debug!("Fetching YGG page via Scrappey: {}", url);
    let html = ScrappeyClient::fetch_page(url).await?;
    debug!("Scrappey returned {} bytes for {}", html.len(), url);
    Ok(html)
}


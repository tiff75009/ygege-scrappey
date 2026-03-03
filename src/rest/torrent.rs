use crate::DOMAIN;
use crate::config::Config;
use crate::rest::client_extractor::MaybeCustomClient;
use crate::scrappey::{PROFILE_ID, SCRAPPEY, ScrappeyClient};
use actix_web::{HttpRequest, HttpResponse, get, web};

#[get("/torrent/{id:[0-9]+}")]
pub async fn download_torrent(
    data: MaybeCustomClient,
    config: web::Data<Config>,
    req_data: HttpRequest,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let id = req_data.match_info().get("id").unwrap();
    let id = id.parse::<usize>()?;

    let domain = {
        let lock = DOMAIN.lock()?;
        lock.clone()
    };

    if !ScrappeyClient::is_available() {
        return Err("Scrappey not configured (set SCRAPPEY_API_KEY)".into());
    }

    let wait_ms: u64 = if config.turbo_enabled.unwrap_or(false) { 5000 } else { 35000 };

    info!("Download #{}: starting (profileId={}, wait={}ms)...", id, PROFILE_ID, wait_ms);

    let torrent_bytes = scrappey_download(id, &domain, wait_ms, &config.username, &config.password).await?;

    info!("Download #{}: success ({} bytes)", id, torrent_bytes.len());
    build_torrent_response(id, torrent_bytes, data.cookies_header)
}

/// Single Scrappey call: navigate to login page with profile →
/// if login form present, fill via JS + submit → then token POST → wait → download
/// All in ONE request using browserActions + execute_js.
async fn scrappey_download(
    id: usize,
    domain: &str,
    wait_ms: u64,
    username: &str,
    password: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let sc = SCRAPPEY.get().ok_or("Scrappey not configured")?;

    let login_url = format!("https://{}{}", domain, crate::LOGIN_PAGE);

    // JS: Check if login form exists (visible or hidden), fill it and submit
    // Uses direct DOM manipulation to bypass visibility issues
    let login_js = format!(
        r#"(function() {{
            const idInput = document.querySelector('input[name="id"]');
            const passInput = document.querySelector('input[name="pass"]');
            if (idInput && passInput) {{
                // Set values directly via JS (bypasses visibility check)
                const nativeInputValueSetter = Object.getOwnPropertyDescriptor(
                    window.HTMLInputElement.prototype, 'value'
                ).set;
                nativeInputValueSetter.call(idInput, '{username}');
                idInput.dispatchEvent(new Event('input', {{ bubbles: true }}));
                nativeInputValueSetter.call(passInput, '{password}');
                passInput.dispatchEvent(new Event('input', {{ bubbles: true }}));
                // Submit the form
                const form = idInput.closest('form');
                if (form) {{
                    form.submit();
                    return 'login_submitted';
                }}
                return 'no_form';
            }}
            return 'no_login_form';
        }})()"#,
        username = username.replace('\'', "\\'").replace('\\', "\\\\"),
        password = password.replace('\'', "\\'").replace('\\', "\\\\"),
    );

    // JS: POST token via fetch() — store in localStorage (survives page navigation)
    let token_js = format!(
        r#"fetch('https://{domain}/engine/start_download_timer', {{
            method: 'POST',
            headers: {{
                'Content-Type': 'application/x-www-form-urlencoded',
                'X-Requested-With': 'XMLHttpRequest'
            }},
            body: 'torrent_id={id}',
            credentials: 'include'
        }})
        .then(r => r.text())
        .then(text => {{
            try {{
                const json = JSON.parse(text);
                if (json.token) {{
                    localStorage.setItem('__dl_token', json.token);
                    return json.token;
                }}
            }} catch(e) {{}}
            return 'ERROR:' + text.substring(0, 200);
        }})
        .catch(e => 'FETCH_ERROR:' + String(e))"#,
        domain = domain,
        id = id
    );

    // JS: Download torrent as base64 — read token from localStorage
    // Uses Blob + FileReader instead of Uint8Array (Firefox Xrays compatibility)
    let download_js = format!(
        r#"(async function() {{
            const token = localStorage.getItem('__dl_token');
            if (!token) return 'ERROR:no_token_in_storage';
            const resp = await fetch('https://{domain}/engine/download_torrent?id={id}&token=' + token, {{
                credentials: 'include'
            }});
            if (!resp.ok) return 'ERROR:http_' + resp.status;
            const blob = await resp.blob();
            if (blob.size === 0) return 'ERROR:empty_blob';
            return new Promise((resolve, reject) => {{
                const reader = new FileReader();
                reader.onload = () => {{
                    const dataUrl = reader.result;
                    const base64 = dataUrl.split(',')[1] || '';
                    resolve(base64);
                }};
                reader.onerror = () => reject('READER_ERROR');
                reader.readAsDataURL(blob);
            }});
        }})()"#,
        domain = domain,
        id = id
    );

    let request = serde_json::json!({
        "cmd": "request.get",
        "url": login_url,
        "profileId": PROFILE_ID,
        "proxyCountry": "FR",
        "browserActions": [
            // Step 1: If login form present → fill and submit via JS
            {
                "type": "execute_js",
                "code": login_js,
                "when": "after_captcha"
            },
            // Step 2: If login was submitted, wait for page navigation
            {
                "type": "if",
                "condition": "document.querySelector('input[name=\"id\"]') !== null",
                "when": "after_captcha",
                "then": [
                    { "type": "wait", "wait": 6000 },
                    { "type": "wait_for_load_state", "waitForLoadState": "networkidle" }
                ]
            },
            // Step 3: POST token via fetch() — stored in localStorage
            {
                "type": "execute_js",
                "code": token_js,
                "when": "after_captcha"
            },
            // Step 4: If token OK → wait timer → download torrent as base64
            {
                "type": "if",
                "condition": "!!localStorage.getItem('__dl_token')",
                "when": "after_captcha",
                "then": [
                    { "type": "wait", "wait": wait_ms },
                    { "type": "execute_js", "code": download_js }
                ]
            }
        ]
    });

    info!("Download #{}: sending Scrappey request...", id);
    let solution = sc.raw_request(&request).await
        .map_err(|e| format!("Download #{}: Scrappey request failed: {}", id, e))?;

    info!(
        "Download #{}: Scrappey response: url={}, js_return_count={}, response_len={}",
        id,
        &solution.url[..solution.url.len().min(80)],
        solution.javascript_return.len(),
        solution.response.len()
    );

    // Debug: log all JS return values
    for (i, val) in solution.javascript_return.iter().enumerate() {
        let preview = match val.as_str() {
            Some(s) => s[..s.len().min(100)].to_string(),
            None => format!("{:?}", val),
        };
        debug!("Download #{}: javascriptReturn[{}] = {:?}", id, i, preview);
    }

    // javascriptReturn[0] = login result ("login_submitted" | "no_login_form" | "no_form")
    // javascriptReturn[1] = token result (token string | "ERROR:..." | "FETCH_ERROR:...")
    // javascriptReturn[2] = base64 torrent data (if token succeeded)

    let login_result = solution.javascript_return
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    info!("Download #{}: login check = {}", id, login_result);

    // Find the token result (index depends on whether login was submitted)
    let token_result = solution.javascript_return
        .iter()
        .skip(1) // skip login result
        .find(|v| {
            v.as_str().map(|s| {
                !s.is_empty() && s != "login_submitted" && s != "no_login_form" && s != "no_form"
            }).unwrap_or(false)
        })
        .and_then(|v| v.as_str());

    let token_str = match token_result {
        Some(s) if s.starts_with("ERROR:") => {
            return Err(format!("Download #{}: token POST returned error: {}", id, s).into());
        }
        Some(s) if s.starts_with("FETCH_ERROR:") => {
            return Err(format!("Download #{}: token fetch failed: {}", id, s).into());
        }
        Some(s) if !s.is_empty() => s,
        _ => {
            let preview = &solution.response[..solution.response.len().min(300)];
            return Err(format!(
                "Download #{}: no token in response (url={}, preview={:?})",
                id, solution.url, preview
            ).into());
        }
    };

    info!("Download #{}: token = {}...", id, &token_str[..token_str.len().min(12)]);

    // Find the base64 torrent data (last JS return value, should be large)
    let torrent_b64 = solution.javascript_return
        .last()
        .and_then(|v| v.as_str())
        .filter(|s| !s.starts_with("ERROR:") && !s.starts_with("FETCH_ERROR:") && s.len() > 50);

    let torrent_b64 = match torrent_b64 {
        Some(b64) => b64,
        None => {
            return Err(format!("Download #{}: no torrent data in response (js_returns={})", id, solution.javascript_return.len()).into());
        }
    };

    // Decode base64 → binary
    use base64::{Engine as _, engine::general_purpose};
    let torrent_bytes = general_purpose::STANDARD.decode(torrent_b64)
        .map_err(|e| format!("Download #{}: base64 decode failed: {}", id, e))?;

    if torrent_bytes.is_empty() {
        return Err(format!("Download #{}: decoded torrent is empty", id).into());
    }

    if torrent_bytes.first() != Some(&b'd') {
        let preview = String::from_utf8_lossy(&torrent_bytes[..torrent_bytes.len().min(200)]);
        error!("Download #{}: not a valid torrent file: {:?}", id, &preview);
        return Err("Downloaded content is not a valid torrent file".into());
    }

    Ok(torrent_bytes)
}

/// Build HTTP response with torrent binary data
fn build_torrent_response(
    id: usize,
    torrent_bytes: Vec<u8>,
    cookies_header: Option<String>,
) -> Result<HttpResponse, Box<dyn std::error::Error>> {
    let mut response_builder = HttpResponse::Ok();
    response_builder
        .content_type("application/x-bittorrent")
        .append_header((
            "Content-Disposition",
            format!("attachment; filename=\"{}.torrent\"", id),
        ));

    if let Some(cookies) = cookies_header {
        response_builder.insert_header(("X-Session-Cookies", cookies));
    }

    Ok(response_builder.body(torrent_bytes))
}

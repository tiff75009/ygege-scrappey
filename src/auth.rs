use crate::domain::get_leaked_ip;
use crate::scrappey::ScrappeyClient;
use crate::resolver::AsyncDNSResolverAdapter;
use crate::{DOMAIN, LOGIN_PAGE, LOGIN_PROCESS_PAGE};
use std::fs::File;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use wreq::header::HeaderMap;
use wreq::{Client, Url};
use wreq_util::{Emulation, EmulationOS, EmulationOption};

pub static KEY: OnceLock<String> = OnceLock::new();

pub async fn login(
    username: &str,
    password: &str,
    use_sessions: bool,
) -> Result<Client, Box<dyn std::error::Error>> {
    login_with_scrappey(username, password, use_sessions, None).await
}

pub async fn login_with_scrappey(
    username: &str,
    password: &str,
    use_sessions: bool,
    scrappey_api_key: Option<&str>,
) -> Result<Client, Box<dyn std::error::Error>> {
    debug!("Logging in with username: {}", username);

    let emu = EmulationOption::builder()
        .emulation(Emulation::Chrome132) // no H3 check on CF before 133
        .emulation_os(EmulationOS::Windows)
        .build();

    let domain_lock = DOMAIN.lock()?;
    let cloned_guard = domain_lock.clone();
    let domain = cloned_guard.as_str();
    drop(domain_lock);

    let leaked_ip = get_leaked_ip().await?;

    let client = Client::builder()
        .emulation(emu)
        .gzip(true)
        .deflate(true)
        .brotli(true)
        .zstd(true)
        .cookie_store(true)
        .dns_resolver(Arc::new(AsyncDNSResolverAdapter::new()?))
        .cert_verification(false)
        .verify_hostname(false)
        .timeout(Duration::from_secs(3))
        .connect_timeout(Duration::from_secs(3))
        .resolve(
            &domain,
            SocketAddr::new(IpAddr::from_str(leaked_ip.as_str())?, 443),
        )
        .build()?;

    let mut headers = HeaderMap::new();
    add_bypass_headers(&mut headers);

    let start = std::time::Instant::now();

    if use_sessions {
        // check if the session file exists
        let session_file = format!("sessions/{}.cookies", username);
        if std::path::Path::new(&session_file.clone()).exists() {
            debug!("Session file found: {}", session_file);
            // load the session from the file
            let cookies = std::fs::read_to_string(&session_file)?;
            let cookies = cookies.split(";").collect::<Vec<&str>>();
            let cookies_len = cookies.len();
            for cookie in cookies {
                let cookie = cookie.trim();
                if cookie.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = cookie.split('=').collect();
                if parts.len() != 2 {
                    continue;
                }
                let name = parts[0].trim();
                let value = parts[1].trim();
                let cookie = wreq::cookie::CookieBuilder::new(name, value)
                    .domain(domain)
                    .path("/")
                    .http_only(true)
                    .secure(true)
                    .build();
                let url = Url::parse(format!("https://{domain}/").as_str())?;
                client.set_cookie(&url, cookie);
            }
            debug!("Restored {} cookies from session file", cookies_len);
        }

        // check if the session is still valid
        let session_check = client
            .get(format!("https://{domain}/"))
            .headers(headers.clone())
            .send()
            .await;
        match session_check {
            Ok(response) if response.status().is_success() => {
                let stop = std::time::Instant::now();
                debug!(
                    "Successfully resumed session in {:?}",
                    stop.duration_since(start)
                );
                return Ok(client);
            }
            Ok(response) => {
                debug!(
                    "Session is not valid, deleting session file (code {})",
                    response.status()
                );
                let _ = std::fs::remove_file(&session_file);
                debug!("Session file deleted");
            }
            Err(e) => {
                debug!(
                    "Session check failed ({}), deleting session file and proceeding to login",
                    e
                );
                let _ = std::fs::remove_file(&session_file);
            }
        }
    }

    client.clear_cookies();

    // inject account_created=true cookie (cookie magique)
    let cookie = wreq::cookie::CookieBuilder::new("account_created", "true")
        .domain(domain)
        .path("/")
        .http_only(true)
        .secure(true)
        .build();

    let url = Url::parse(format!("https://{domain}/").as_str())?;
    client.set_cookie(&url, cookie);

    // --- Étape 1 : Essayer de GET la page de login via wreq ---
    let login_page_url = format!("https://{domain}{LOGIN_PAGE}");
    let response = client
        .get(&login_page_url)
        .headers(headers.clone())
        .send()
        .await;

    // Déterminer si on a besoin de Scrappey
    let needs_scrappey = match &response {
        Ok(resp) => {
            if !resp.status().is_success() {
                warn!(
                    "Login page returned HTTP {} — possible Cloudflare block",
                    resp.status()
                );
                true
            } else {
                false
            }
        }
        Err(e) => {
            warn!("Login page request failed: {} — will try Scrappey", e);
            true
        }
    };

    // Vérifier le cookie ygg_ si la réponse est OK
    let has_ygg_cookie = if let Ok(ref resp) = response {
        if resp.status().is_success() {
            resp.cookies().any(|c| c.name() == "ygg_")
        } else {
            false
        }
    } else {
        false
    };

    // --- Étape 2 : Scrappey fallback si nécessaire ---
    // NOTE: Les cookies CF (cf_clearance) sont liés au fingerprint TLS du navigateur
    // qui les a obtenus. On ne peut PAS les transférer de Scrappey vers wreq.
    // Donc si wreq est bloqué, Scrappey doit faire le login COMPLET (GET + POST).
    if needs_scrappey || !has_ygg_cookie {
        if let Some(api_key) = scrappey_api_key {
            warn!(
                "Cloudflare challenge detected (needs_scrappey={}, has_ygg_cookie={}), \
                 Scrappey will handle the full login...",
                needs_scrappey, has_ygg_cookie
            );

            let sc_client = ScrappeyClient::new(api_key)
                .map_err(|e| format!("Failed to create Scrappey client: {}", e))?;

            // Créer une session Scrappey persistante (les cookies survivent entre requêtes)
            let session_id = sc_client
                .create_session()
                .await
                .map_err(|e| format!("Failed to create Scrappey session: {}", e))?;

            // Étape unique : GET login + browserActions (type + enter) en after_captcha
            // - Scrappey navigue vers la page de login
            // - Résout automatiquement le challenge CF / Turnstile
            // - APRÈS résolution, remplit le formulaire avec type (simulation humaine)
            // - Soumet avec Enter (déclenche les event handlers natifs)
            // - Attend que la page se stabilise (networkidle)
            info!("Scrappey: login complet via browserActions (after_captcha)...");

            let login_request = serde_json::json!({
                "cmd": "request.get",
                "url": login_page_url,
                "session": session_id,
                "profileId": crate::scrappey::PROFILE_ID,
                "proxyCountry": "FR",
                "browserActions": [
                    {
                        "type": "type",
                        "cssSelector": "input[name='id']",
                        "text": username,
                        "when": "after_captcha"
                    },
                    {
                        "type": "type",
                        "cssSelector": "input[name='pass']",
                        "text": password,
                        "when": "after_captcha"
                    },
                    {
                        "type": "keyboard",
                        "value": "enter",
                        "when": "after_captcha"
                    },
                    {
                        "type": "wait",
                        "wait": 5000,
                        "when": "after_captcha"
                    },
                    {
                        "type": "wait_for_load_state",
                        "waitForLoadState": "networkidle",
                        "when": "after_captcha"
                    }
                ]
            });

            let login_solution = sc_client.raw_request(&login_request)
                .await
                .map_err(|e| format!("Scrappey browser login failed: {}", e))?;

            info!(
                "Scrappey login: final URL={}, cookies={}, response_len={}",
                &login_solution.url[..login_solution.url.len().min(80)],
                login_solution.cookies.len(),
                login_solution.response.len()
            );
            for cookie in &login_solution.cookies {
                debug!("  Cookie: {}={}", cookie.name, &cookie.value[..cookie.value.len().min(40)]);
            }

            // Vérifier si le login a réussi : l'URL finale doit être la homepage, pas /auth/login
            if login_solution.url.contains("/auth/login") {
                return Err(
                    "Login Scrappey échoué : toujours sur /auth/login après soumission. \
                     Vérifiez les identifiants YGG."
                        .into(),
                );
            }

            // Les cookies du browserActions contiennent l'état authentifié
            // Pas besoin de faire un GET supplémentaire (qui créerait un nouveau contexte)
            let final_cookies = &login_solution.cookies;
            ScrappeyClient::set_user_agent(login_solution.user_agent.clone()).await;
            // Stocker les cookies pour les injecter dans les futures requêtes Scrappey
            ScrappeyClient::set_cookies(final_cookies.clone()).await;

            let base_url = Url::parse(&format!("https://{domain}/"))?;
            for cookie in final_cookies {
                debug!(
                    "Injecting session cookie: {}={} (domain: {})",
                    cookie.name, cookie.value, cookie.domain
                );
                let c = wreq::cookie::CookieBuilder::new(
                    cookie.name.as_str(),
                    cookie.value.as_str(),
                )
                .domain(domain)
                .path(&cookie.path)
                .http_only(true)
                .secure(true)
                .build();
                client.set_cookie(&base_url, c);
            }

            let stop = std::time::Instant::now();
            info!(
                "Logged in successfully via Scrappey in {:?}",
                stop.duration_since(start)
            );

            if use_sessions {
                save_session(username, &client).await?;
            }

            return Ok(client);
        } else {
            // Pas de Scrappey configuré
            if needs_scrappey {
                return Err(
                    "Cloudflare blocked the login page and SCRAPPEY_API_KEY is not set. \
                     Set SCRAPPEY_API_KEY to enable automatic bypass."
                        .into(),
                );
            } else {
                return Err("No ygg_ cookie found and SCRAPPEY_API_KEY is not set".into());
            }
        }
    } else {
        debug!("Login page fetched successfully with ygg_ cookie via wreq (no Scrappey needed)");
    }

    // --- Étape 3 (wreq only) : POST credentials ---
    let payload = [("id", username), ("pass", password)];

    let response = client
        .post(format!("https://{domain}{LOGIN_PROCESS_PAGE}"))
        .headers(headers.clone())
        .form(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        if response.status() == 401 {
            error!("Invalid username or password");
            return Err("Invalid username or password".into());
        }
        return Err(format!("Failed to login: {}", response.status()).into());
    }

    let _headers = response.headers();

    // get site root page for final cookies
    let response = client
        .get(format!("https://{domain}/"))
        .headers(headers.clone())
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(format!("Failed to fetch site root page: {}", response.status()).into());
    }

    let stop = std::time::Instant::now();
    debug!("Logged in successfully via wreq in {:?}", stop.duration_since(start));

    let _headers = response.cookies();

    if use_sessions {
        save_session(username, &client).await?;
    }

    Ok(client)
}

async fn save_session(username: &str, client: &Client) -> Result<(), Box<dyn std::error::Error>> {
    // save the session in a file
    let mut file = File::create(format!("sessions/{}.cookies", username))?;
    let cookies_header = client
        .get_cookies(&Url::parse(
            format!("https://{}/", DOMAIN.lock()?.as_str()).as_str(),
        )?)
        .unwrap();
    let cookies_header_value = cookies_header.to_str()?;
    debug!("Cookies: {}", cookies_header_value);
    file.write_all(cookies_header_value.as_bytes())?;
    file.flush()?;

    Ok(())
}

pub fn add_bypass_headers(headers: &mut HeaderMap) {
    let own_ip_lock = crate::domain::OWN_IP.get();
    if let Some(own_ip) = own_ip_lock {
        headers.insert("CF-Connecting-IP", own_ip.parse().unwrap());
        headers.insert("X-Forwarded-For", own_ip.parse().unwrap());
    }
}

use crate::parser::Torrent;
use crate::search::{Order, Sort};
use serde::Deserialize;
use std::sync::OnceLock;

const YGGAPI_BASE: &str = "https://yggapi.eu";

static YGGAPI_CLIENT: OnceLock<wreq::Client> = OnceLock::new();

fn get_client() -> &'static wreq::Client {
    YGGAPI_CLIENT.get_or_init(|| {
        wreq::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("Failed to create yggapi.eu client")
    })
}

#[derive(Debug, Deserialize)]
struct YggApiTorrent {
    id: usize,
    title: String,
    seeders: usize,
    leechers: usize,
    downloads: Option<usize>,
    size: u64,
    category_id: usize,
    uploaded_at: String,
    link: String,
}

impl YggApiTorrent {
    fn into_torrent(self) -> Torrent {
        let age_stamp = chrono::DateTime::parse_from_rfc3339(&self.uploaded_at)
            .map(|dt| dt.timestamp() as usize)
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(&self.uploaded_at, "%Y-%m-%dT%H:%M:%S")
                    .map(|dt| dt.and_utc().timestamp() as usize)
            })
            .unwrap_or(0);

        let info_url = if let Some(pos) = self.link.find("/torrent/") {
            format!("/torrent/info/{}", &self.link[pos + 9..])
        } else {
            format!("/torrent/info/{}", self.id)
        };

        Torrent {
            category_id: self.category_id,
            name: self.title,
            id: self.id,
            comments_count: 0,
            age_stamp,
            size: self.size,
            completed: self.downloads.unwrap_or(0),
            seed: self.seeders,
            leech: self.leechers,
            info_url,
            link: self.link,
        }
    }
}

fn sort_to_api(sort: &Sort) -> Option<&'static str> {
    match sort {
        Sort::Seed => Some("seeders"),
        Sort::PublishDate => Some("uploaded_at"),
        Sort::Completed => Some("downloads"),
        _ => None,
    }
}

/// Search torrents via yggapi.eu (no Cloudflare, instant)
pub async fn search(
    name: &str,
    page: Option<usize>,
    categories: &[usize],
    sort: Option<Sort>,
    order: Option<Order>,
    ban_words: Option<&[String]>,
) -> Result<Vec<Torrent>, Box<dyn std::error::Error>> {
    let client = get_client();

    let mut url = format!("{}/torrents?per_page=100", YGGAPI_BASE);

    if !name.is_empty() {
        url.push_str(&format!("&q={}", urlencoding::encode(name)));
    }

    if let Some(page) = page {
        if page > 0 {
            url.push_str(&format!("&page={}", page));
        }
    }

    for cat_id in categories {
        url.push_str(&format!("&category_id={}", cat_id));
    }

    if let Some(ref sort_val) = sort {
        if let Some(api_sort) = sort_to_api(sort_val) {
            url.push_str(&format!("&order_by={}", api_sort));
        }
    }

    debug!("yggapi.eu search: {}", url);

    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Err(format!("yggapi.eu returned HTTP {}", response.status()).into());
    }

    let api_torrents: Vec<YggApiTorrent> = response.json().await?;
    let mut torrents: Vec<Torrent> = api_torrents.into_iter().map(|t| t.into_torrent()).collect();

    // Apply ban words filter
    if let Some(ban_words) = ban_words {
        torrents.retain(|t| {
            !ban_words
                .iter()
                .any(|word| t.name.to_lowercase().contains(&word.to_lowercase()))
        });
    }

    // Sort locally if API doesn't support the requested sort, or if ascending order requested
    let needs_local_sort = sort.as_ref().map(|s| sort_to_api(s).is_none()).unwrap_or(false);
    if needs_local_sort || matches!(order, Some(Order::Ascending)) {
        Torrent::sort(&mut torrents, sort, order);
    }

    debug!("yggapi.eu returned {} torrents", torrents.len());
    Ok(torrents)
}

/// Search by TMDB ID via yggapi.eu (native support)
pub async fn search_by_tmdb(
    tmdb_id: usize,
    media_type: Option<&str>,
    categories: &[usize],
    sort: Option<Sort>,
    order: Option<Order>,
) -> Result<Vec<Torrent>, Box<dyn std::error::Error>> {
    let client = get_client();

    let mut url = format!("{}/torrents?per_page=100&tmdb_id={}", YGGAPI_BASE, tmdb_id);

    if let Some(mt) = media_type {
        url.push_str(&format!("&type={}", mt));
    }

    for cat_id in categories {
        url.push_str(&format!("&category_id={}", cat_id));
    }

    if let Some(ref sort_val) = sort {
        if let Some(api_sort) = sort_to_api(sort_val) {
            url.push_str(&format!("&order_by={}", api_sort));
        }
    }

    debug!("yggapi.eu TMDB search: {}", url);

    let response = client.get(&url).send().await?;
    if !response.status().is_success() {
        return Err(format!("yggapi.eu TMDB search returned HTTP {}", response.status()).into());
    }

    let api_torrents: Vec<YggApiTorrent> = response.json().await?;
    let mut torrents: Vec<Torrent> = api_torrents.into_iter().map(|t| t.into_torrent()).collect();

    let needs_local_sort = sort.as_ref().map(|s| sort_to_api(s).is_none()).unwrap_or(false);
    if needs_local_sort || matches!(order, Some(Order::Ascending)) {
        Torrent::sort(&mut torrents, sort, order);
    }

    debug!("yggapi.eu TMDB search returned {} torrents", torrents.len());
    Ok(torrents)
}

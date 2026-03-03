pub(crate) use crate::categories::CATEGORIES_CACHE;
use crate::parser::Torrent;
use crate::rate_limiter::RateLimiter;
use std::str::FromStr;
use std::sync::OnceLock;
use urlencoding::decode;

static RATE_LIMITER: OnceLock<RateLimiter> = OnceLock::new();

pub(crate) fn get_rate_limiter() -> &'static RateLimiter {
    RATE_LIMITER.get_or_init(|| RateLimiter::default())
}

/// Search via yggapi.eu (no Cloudflare, instant).
/// The wreq client parameter is no longer needed for search.
pub async fn search(
    name: &str,
    offset: Option<usize>,
    category: Option<usize>,
    sub_category: Option<usize>,
    sort: Option<Sort>,
    order: Option<Order>,
    ban_words: Option<Vec<String>>,
    quote_search: bool,
) -> Result<Vec<Torrent>, Box<dyn std::error::Error>> {
    let name = match quote_search {
        true if !name.is_empty() => {
            let decoded = decode(name).unwrap_or(std::borrow::Cow::Borrowed(name));
            decoded
                .split(|c| c == '+' || c == ' ')
                .filter(|w| !w.is_empty())
                .map(|w| format!("\"{}\"", w))
                .collect::<Vec<_>>()
                .join(" ")
        }
        _ => name.to_owned(),
    };
    debug!(
        "Searching for torrents (name: {:?}, offset: {:?}, category: {:?}, sub_category: {:?}, sort: {:?}, order: {:?})",
        name, offset, category, sub_category, sort, order
    );

    let _guard = get_rate_limiter().acquire().await;

    // Build categories list for yggapi.eu
    let mut categories = Vec::new();
    if let Some(cat) = category {
        categories.push(cat);
    }
    if let Some(sub_cat) = sub_category {
        categories.push(sub_cat);
    }

    let start = std::time::Instant::now();
    let torrents = crate::yggapi::search(
        name.as_str(),
        offset,
        &categories,
        sort,
        order,
        ban_words.as_deref(),
    )
    .await?;

    let stop = std::time::Instant::now();
    debug!(
        "Found {} torrents in {:?}",
        torrents.len(),
        stop.duration_since(start)
    );
    Ok(torrents)
}

#[derive(Debug, Clone, Copy)]
pub enum Sort {
    Name,
    Seed,
    Comments,
    PublishDate,
    Completed,
    Leech,
}

#[derive(Debug, Clone, Copy)]
pub enum Order {
    Ascending,
    Descending,
}
impl Sort {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        match self {
            Sort::Name => "name",
            Sort::Seed => "seed",
            Sort::Comments => "comments",
            Sort::PublishDate => "publish_date",
            Sort::Completed => "completed",
            Sort::Leech => "leech",
        }
    }
}

impl Order {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        match self {
            Order::Ascending => "asc",
            Order::Descending => "desc",
        }
    }
}

impl FromStr for Sort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "name" => Ok(Sort::Name),
            "seed" => Ok(Sort::Seed),
            "comments" => Ok(Sort::Comments),
            "publish_date" => Ok(Sort::PublishDate),
            "completed" => Ok(Sort::Completed),
            "leech" => Ok(Sort::Leech),
            _ => Err(format!("Valeur de tri invalide : {}", s)),
        }
    }
}

impl FromStr for Order {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "asc" => Ok(Order::Ascending),
            "desc" => Ok(Order::Descending),
            _ => Err(format!("Ordre invalide : {}", s)),
        }
    }
}

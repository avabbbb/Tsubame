//! DLsite public metadata adapter (PR #14, docs/08-source-adapters.md).
//!
//! RJ code -> title, circle, voice actors (Japanese name canonical, known
//! romanizations as aliases), tags, languages, cover, release date.
//! Metadata only: no login, no downloads, no paywalled content. The network
//! is touched only on an explicit get_work / list_assets / refresh call.

use super::{
    Asset, AssetKind, Capabilities, SearchFilters, SourceAdapter, SourceError, VoiceActor, Work,
};
use regex::Regex;
use serde_json::Value;
use std::fmt::Write as _;
use std::sync::LazyLock;
use std::time::Duration;

const PRODUCT_API: &str = "https://www.dlsite.com/maniax/api/=/product.json?workno=";
/// Public catalog search (fsr/ajax): JSON whose search_result is the
/// result-list HTML. Scoped to voice/ASMR works; no login.
const SEARCH_DEFAULT_LIMIT: u32 = 30;
/// fsr only serves these page sizes.
const SEARCH_PAGE_SIZES: [u32; 3] = [30, 50, 100];

/// Romanizations DLsite does not carry (docs/08 "Name normalization").
/// Keyed by the canonical Japanese name; never used as an identity key.
const KNOWN_ALIASES: &[(&str, &[&str])] = &[
    ("大山チロル", &["Chiroru Oyama", "Ooyama Chiroru"]),
    ("秋山はるる", &["Haruru Akiyama"]),
    ("篠守ゆきこ", &["Yukiko Shinomori", "Shinomori Yukiko"]),
];

/// DLsite `options` language flags -> BCP 47.
const LANGUAGES: &[(&str, &str)] = &[
    ("JPN", "ja"),
    ("ENG", "en"),
    ("CHI_HANS", "zh-Hans"),
    ("CHI_HANT", "zh-Hant"),
    ("KO_KR", "ko"),
];

pub struct DlsiteAdapter;

impl DlsiteAdapter {
    pub fn new() -> Self {
        Self
    }
}

/// `RJ` + exactly 6 or 8 digits, upper-cased; anything else is invalid_input.
pub fn normalize_rj(remote_id: &str) -> Result<String, SourceError> {
    let upper = remote_id.trim().to_ascii_uppercase();
    let digits = upper.strip_prefix("RJ").unwrap_or("");
    if (digits.len() == 6 || digits.len() == 8) && digits.bytes().all(|b| b.is_ascii_digit()) {
        Ok(upper)
    } else {
        Err(SourceError::invalid_input(format!(
            "not a DLsite RJ code: {remote_id}"
        )))
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn voice_actors(product: &Value) -> Vec<VoiceActor> {
    // `creaters` is an object when present and `[]` when the work has none.
    let mut actors: Vec<VoiceActor> = Vec::new();
    let entries = product
        .get("creaters")
        .and_then(|c| c.get("voice_by"))
        .and_then(Value::as_array);
    for entry in entries.into_iter().flatten() {
        let Some(name) = str_field(entry, "name") else {
            continue;
        };
        if actors.iter().any(|actor| actor.name_ja == name) {
            continue;
        }
        let aliases = KNOWN_ALIASES
            .iter()
            .find(|(ja, _)| *ja == name)
            .map(|(_, aliases)| aliases.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        actors.push(VoiceActor {
            name_ja: name.to_string(),
            aliases,
        });
    }
    actors
}

fn cover(product: &Value) -> Option<Asset> {
    let image = product.get("image_main")?;
    let url = str_field(image, "url")?;
    let url = match url.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    };
    Some(Asset {
        id: "cover".into(),
        kind: AssetKind::Image,
        name: str_field(image, "file_name")
            .unwrap_or("cover.jpg")
            .to_string(),
        location: url,
        size: str_field(image, "file_size").and_then(|s| s.parse().ok()),
        sha256: None,
    })
}

/// Normalizes a product.json body (an array of products) into the work `rj`.
pub fn parse_product(rj: &str, body: &str) -> Result<Work, SourceError> {
    let products: Vec<Value> = serde_json::from_str(body)
        .map_err(|e| SourceError::parse(format!("DLsite product.json: {e}")))?;
    let product = products
        .iter()
        .find(|p| str_field(p, "workno").is_some_and(|w| w.eq_ignore_ascii_case(rj)))
        .ok_or_else(|| SourceError::not_found(format!("DLsite has no work {rj}")))?;
    let title = str_field(product, "work_name")
        .ok_or_else(|| SourceError::parse(format!("DLsite {rj}: missing work_name")))?;
    let tags = product
        .get("genres")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|genre| str_field(genre, "name").map(str::to_string))
        .collect();
    let options = str_field(product, "options").unwrap_or("");
    let languages = LANGUAGES
        .iter()
        .filter(|(flag, _)| options.split('#').any(|o| o == *flag))
        .map(|(_, tag)| tag.to_string())
        .collect();
    let release_date = str_field(product, "regist_date")
        .and_then(|d| d.get(..10))
        .map(str::to_string);
    let cover = cover(product);
    Ok(Work {
        source_id: "dlsite".into(),
        remote_id: rj.to_string(),
        rj_code: Some(rj.to_string()),
        title: title.to_string(),
        circle: str_field(product, "maker_name").map(str::to_string),
        voice_actors: voice_actors(product),
        languages,
        tags,
        cover: cover.as_ref().map(|asset| asset.location.clone()),
        release_date,
        assets: cover.into_iter().collect(),
    })
}

/// GET url as text. Called from sync commands / job threads: block_on
/// supplies the runtime reqwest's timers need (same as runtime::HttpFetcher).
fn get_text(url: String, what: &str) -> Result<String, SourceError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("YUZUKI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| SourceError::network(e.to_string()))?;
    tauri::async_runtime::block_on(async move {
        client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await
    })
    .map_err(|e| SourceError::network(format!("DLsite {what}: {e}")))
}

fn fetch_product(rj: &str) -> Result<String, SourceError> {
    get_text(format!("{PRODUCT_API}{rj}"), rj)
}

/// Percent-encodes one URL path segment, keeping RFC 3986 unreserved bytes.
fn encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn search_url(query: &str, per_page: u32) -> String {
    // Same origin and site section (maniax) as PRODUCT_API.
    let site = PRODUCT_API
        .split_once("/api/")
        .map_or(PRODUCT_API, |(site, _)| site);
    format!(
        "{site}/fsr/ajax/=/language/jp/keyword/{}/work_type_category%5B0%5D/audio/order/trend/per_page/{per_page}/page/1",
        encode_segment(query)
    )
}

fn unescape_html(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

struct SearchPatterns {
    thumb: Regex,
    title: Regex,
    circle: Regex,
}

static SEARCH_PATTERNS: LazyLock<SearchPatterns> = LazyLock::new(|| SearchPatterns {
    thumb: Regex::new(r"'(//img\.dlsite\.jp/[^']+\.jpg)'").expect("thumb pattern"),
    title: Regex::new(r#"(?s)class="work_name">.{0,2000}?<a [^>]{0,400}title="([^"]+)""#)
        .expect("title pattern"),
    circle: Regex::new(r#"class="maker_name"><a [^>]{0,400}>([^<]+)</a>"#).expect("circle pattern"),
});

/// Normalizes an fsr/ajax body into works. The list carries only RJ code,
/// title, circle and thumbnail; source.get_work fills actors, tags, languages.
pub fn parse_search(body: &str) -> Result<Vec<Work>, SourceError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|e| SourceError::parse(format!("DLsite search: {e}")))?;
    let html = value
        .get("search_result")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::parse("DLsite search: missing search_result"))?;
    let patterns: &SearchPatterns = &SEARCH_PATTERNS;
    let mut works: Vec<Work> = Vec::new();
    for item in html.split("data-list_item_product_id=\"").skip(1) {
        let Some((id, rest)) = item.split_once('"') else {
            continue;
        };
        let Ok(rj) = normalize_rj(id) else {
            continue;
        };
        if works.iter().any(|work| work.remote_id == rj) {
            continue;
        }
        let Some(title) = patterns.title.captures(rest).map(|c| unescape_html(&c[1])) else {
            continue;
        };
        let cover = patterns
            .thumb
            .captures(rest)
            .map(|c| format!("https:{}", &c[1]));
        works.push(Work {
            source_id: "dlsite".into(),
            remote_id: rj.clone(),
            rj_code: Some(rj),
            title,
            circle: patterns
                .circle
                .captures(rest)
                .map(|c| unescape_html(c[1].trim())),
            voice_actors: Vec::new(),
            languages: Vec::new(),
            tags: Vec::new(),
            cover: cover.clone(),
            release_date: None,
            assets: cover
                .into_iter()
                .map(|location| Asset {
                    id: "cover".into(),
                    kind: AssetKind::Image,
                    name: "cover.jpg".into(),
                    location,
                    size: None,
                    sha256: None,
                })
                .collect(),
        });
    }
    Ok(works)
}

impl SourceAdapter for DlsiteAdapter {
    fn id(&self) -> &'static str {
        "dlsite"
    }

    fn label(&self) -> &'static str {
        "DLsite"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            search: true,
            get_work: true,
            list_assets: true,
            refresh: true,
            network: true,
            ..Capabilities::default()
        }
    }

    fn get_work(&self, remote_id: &str) -> Result<Work, SourceError> {
        let rj = normalize_rj(remote_id)?;
        parse_product(&rj, &fetch_product(&rj)?)
    }

    fn search(&self, query: &str, filters: &SearchFilters) -> Result<Vec<Work>, SourceError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(SourceError::invalid_input("DLsite search needs a keyword"));
        }
        let limit = filters.limit.unwrap_or(SEARCH_DEFAULT_LIMIT).max(1);
        let per_page = SEARCH_PAGE_SIZES
            .into_iter()
            .find(|size| *size >= limit)
            .unwrap_or(SEARCH_PAGE_SIZES[2]);
        let mut works = parse_search(&get_text(search_url(query, per_page), "search")?)?;
        works.truncate(limit as usize);
        Ok(works)
    }

    fn list_assets(&self, remote_id: &str) -> Result<Vec<Asset>, SourceError> {
        Ok(self.get_work(remote_id)?.assets)
    }

    fn refresh(&self, record: &Work) -> Result<Work, SourceError> {
        self.get_work(&record.remote_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRODUCT: &str = include_str!("fixtures/dlsite_RJ239066_product.json");
    const ALIAS: &str = include_str!("fixtures/dlsite_alias_product.json");

    #[test]
    fn normalizes_product_fixture() {
        let work = parse_product("RJ239066", PRODUCT).unwrap();
        assert_eq!(work.source_id, "dlsite");
        assert_eq!(work.rj_code.as_deref(), Some("RJ239066"));
        assert_eq!(work.title, "チ○ポ係");
        assert_eq!(work.circle.as_deref(), Some("ベレス解部"));
        assert_eq!(
            work.voice_actors,
            [VoiceActor {
                name_ja: "陽向葵ゅか".into(),
                aliases: vec![]
            }]
        );
        assert_eq!(work.tags.len(), 6);
        assert_eq!(work.tags[0], "脚");
        assert_eq!(work.languages, ["ja"]);
        assert_eq!(work.release_date.as_deref(), Some("2018-11-19"));
        let url = "https://img.dlsite.jp/modpub/images2/work/doujin/RJ240000/RJ239066_img_main.jpg";
        assert_eq!(work.cover.as_deref(), Some(url));
        assert_eq!(work.assets.len(), 1);
        assert_eq!(work.assets[0].kind, AssetKind::Image);
        assert_eq!(work.assets[0].size, Some(74_809));
    }

    #[test]
    fn keeps_japanese_names_canonical_with_aliases() {
        let work = parse_product("RJ239066", ALIAS).unwrap();
        let names: Vec<&str> = work
            .voice_actors
            .iter()
            .map(|a| a.name_ja.as_str())
            .collect();
        assert_eq!(names, ["大山チロル", "秋山はるる"]);
        assert_eq!(
            work.voice_actors[0].aliases,
            ["Chiroru Oyama", "Ooyama Chiroru"]
        );
        assert_eq!(work.voice_actors[1].aliases, ["Haruru Akiyama"]);
    }

    #[test]
    fn rejects_bad_ids_and_missing_works() {
        assert_eq!(normalize_rj(" rj239066 ").unwrap(), "RJ239066");
        assert_eq!(normalize_rj("RJ01234567").unwrap(), "RJ01234567");
        for bad in ["RJ12345", "RJ12345a", "VJ239066", ""] {
            assert_eq!(
                normalize_rj(bad).unwrap_err().code,
                "invalid_input",
                "{bad}"
            );
        }
        assert_eq!(
            parse_product("RJ000001", "[]").unwrap_err().code,
            "not_found"
        );
        assert_eq!(
            parse_product("RJ000001", "<html>").unwrap_err().code,
            "parse"
        );
        assert!(!DlsiteAdapter::new().capabilities().acquire);
    }

    #[test]
    fn parses_search_result_list() {
        let html = r#"<li data-list_item_product_id="RJ01687946" class="x"><dl><dt><thumb-with-ng-filter-block :thumb-candidates="['//img.dlsite.jp/resize/images2/work/doujin/RJ01688000/RJ01687946_img_main_240x240.webp','//img.dlsite.jp/resize/images2/work/doujin/RJ01688000/RJ01687946_img_main_240x240.jpg']"></thumb-with-ng-filter-block></dt><dd class="work_name"><div class="multiline_truncate"><a href="w" title="夢見ぐらし。&amp;セット">夢見ぐらし。&amp;セット</a></div></dd><dd class="maker_name"><a href="m">CANDY VOICE</a></dd></dl></li><li data-list_item_product_id="RJ01687946"></li><li data-list_item_product_id="bad"></li>"#;
        let body = serde_json::json!({ "search_result": html, "page_info": { "count": 1 } });
        let works = parse_search(&body.to_string()).unwrap();
        assert_eq!(works.len(), 1);
        let work = &works[0];
        assert_eq!(work.remote_id, "RJ01687946");
        assert_eq!(work.title, "夢見ぐらし。&セット");
        assert_eq!(work.circle.as_deref(), Some("CANDY VOICE"));
        assert_eq!(
            work.cover.as_deref().and_then(|c| c.strip_prefix("https:")),
            Some("//img.dlsite.jp/resize/images2/work/doujin/RJ01688000/RJ01687946_img_main_240x240.jpg")
        );
        assert_eq!(work.assets.len(), 1);
        assert_eq!(parse_search("{}").unwrap_err().code, "parse");
        assert_eq!(encode_segment("耳 a"), "%E8%80%B3%20a");
        let site = PRODUCT_API.split_once("/api/").unwrap().0;
        let url = search_url("耳", 30);
        assert!(url.starts_with(site) && url.contains("/fsr/ajax/"), "{url}");
        assert!(
            url.contains("/keyword/%E8%80%B3/") && url.contains("/per_page/30/"),
            "{url}"
        );
        assert!(DlsiteAdapter::new().capabilities().search);
    }

    #[test]
    fn parses_live_search_markup() {
        // First three items of a real fsr/ajax response (2026-11).
        let works = parse_search(include_str!("fixtures/dlsite_search_live.json")).unwrap();
        assert_eq!(works.len(), 3);
        assert_eq!(works[0].remote_id, "RJ01719814");
        assert_eq!(works[0].circle.as_deref(), Some("いっしんふらん"));
        for work in &works {
            assert!(work.rj_code.is_some() && !work.title.is_empty(), "{work:?}");
            assert!(work.circle.is_some(), "{work:?}");
            let cover = work.cover.as_deref().unwrap_or_default();
            assert!(
                cover.starts_with("https:")
                    && cover.contains("//img.dlsite.jp/")
                    && cover.ends_with(".jpg"),
                "{cover}"
            );
        }
    }

    /// Real network through the env or system proxy:
    /// cargo test -p tsubame --lib dlsite_live -- --ignored.
    #[test]
    #[ignore = "network"]
    fn dlsite_live_search_and_product() {
        let works = parse_search(&get_text(search_url("耳", 30), "search").unwrap()).unwrap();
        assert!(!works.is_empty());
        let work = DlsiteAdapter::new().get_work(&works[0].remote_id).unwrap();
        assert_eq!(work.remote_id, works[0].remote_id);
        assert!(work.cover.is_some() && !work.title.is_empty(), "{work:?}");
    }
}

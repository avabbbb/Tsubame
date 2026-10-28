//! DLsite public metadata adapter (PR #14, docs/08-source-adapters.md).
//!
//! RJ code -> title, circle, voice actors (Japanese name canonical, known
//! romanizations as aliases), tags, languages, cover, release date.
//! Metadata only: no login, no downloads, no paywalled content. The network
//! is touched only on an explicit get_work / list_assets / refresh call.

use super::{Asset, AssetKind, Capabilities, SourceAdapter, SourceError, VoiceActor, Work};
use serde_json::Value;
use std::time::Duration;

const PRODUCT_API: &str = "https://www.dlsite.com/maniax/api/=/product.json?workno=";

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
        let Some(name) = str_field(entry, "name") else { continue };
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
        name: str_field(image, "file_name").unwrap_or("cover.jpg").to_string(),
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

fn fetch_product(rj: &str) -> Result<String, SourceError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("YUZUKI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| SourceError::network(e.to_string()))?;
    let url = format!("{PRODUCT_API}{rj}");
    // Called from sync commands / job threads: block_on supplies the runtime
    // reqwest's timers need (same pattern as runtime::HttpFetcher).
    tauri::async_runtime::block_on(async move {
        client.get(url).send().await?.error_for_status()?.text().await
    })
    .map_err(|e| SourceError::network(format!("DLsite {rj}: {e}")))
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
            [VoiceActor { name_ja: "陽向葵ゅか".into(), aliases: vec![] }]
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
        let names: Vec<&str> = work.voice_actors.iter().map(|a| a.name_ja.as_str()).collect();
        assert_eq!(names, ["大山チロル", "秋山はるる"]);
        assert_eq!(work.voice_actors[0].aliases, ["Chiroru Oyama", "Ooyama Chiroru"]);
        assert_eq!(work.voice_actors[1].aliases, ["Haruru Akiyama"]);
    }

    #[test]
    fn rejects_bad_ids_and_missing_works() {
        assert_eq!(normalize_rj(" rj239066 ").unwrap(), "RJ239066");
        assert_eq!(normalize_rj("RJ01234567").unwrap(), "RJ01234567");
        for bad in ["RJ12345", "RJ12345a", "VJ239066", ""] {
            assert_eq!(normalize_rj(bad).unwrap_err().code, "invalid_input", "{bad}");
        }
        assert_eq!(parse_product("RJ000001", "[]").unwrap_err().code, "not_found");
        assert_eq!(parse_product("RJ000001", "<html>").unwrap_err().code, "parse");
        assert!(!DlsiteAdapter::new().capabilities().acquire);
    }
}

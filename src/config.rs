use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
pub struct AppAssetConfig {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub url: String,
    pub assets: Vec<AppAssetConfig>,
    /// Optional CPU index to pin the session to.
    pub cpu: Option<usize>,
}

pub fn build_subscription_payload(assets: &[AppAssetConfig]) -> String {
    let asset_ids: Vec<&str> = assets.iter().map(|a| a.id.as_str()).collect();
    json!({
        "assets_ids": asset_ids,
        "type": "market",
    })
    .to_string()
}


use std::{env::consts::OS, os};

use color_eyre::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SmapiRequest {
    platform: Option<String>,
    #[serde(rename = "includeExtendedMetadata")]
    include_extended_metadata: bool,
    mods: Vec<SmapiMod>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SmapiMod {
    pub id: String,
    #[serde(default)]
    pub metadata: SmapiModMetadata,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SmapiModMetadata {
    #[serde(rename = "nexusID")]
    #[serde(default)]
    pub nexus_id: i32,
    #[serde(default)]
    pub main: SmapiModMetadataMain,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SmapiModMetadataMain {
    #[serde(default)]
    pub url: String,
}

const SMAPI_API_URL: &str = "https://smapi.io/api/v3.0/mods";

pub async fn resolve_mods(mods: Vec<SmapiMod>) -> Result<Vec<SmapiMod>> {
    static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
        reqwest::Client::builder()
            .user_agent("StardewValleyModManager (https://github.com/tricked/stardew-valley-mod-manager)")
            .build()
            .unwrap()
    });

    let request = SmapiRequest {
        mods,
        platform: match OS {
            "linux" => Some("Linux".to_owned()),
            "macos" => Some("Mac".to_owned()),
            "windows" => Some("Windows".to_owned()),
            "android" => Some("Android".to_owned()),
            _ => None,
        },
        include_extended_metadata: true,
    };

    let result = CLIENT
        .post(SMAPI_API_URL)
        .json(&request)
        .send()
        .await?
        .json::<Vec<SmapiMod>>()
        .await?;

    Ok(result)
}

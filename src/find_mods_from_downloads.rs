//TODO: logging
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};
use time::{macros::format_description, OffsetDateTime};
use walkdir::{DirEntry, WalkDir};
use zip::read::ZipArchive;

use crate::{mods_to_modelrc, InstalledMod, ModManifest, ModsZip};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ZipMod {
    path: PathBuf,
    created_at: SystemTime,
    manifests: Vec<ZipModMod>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ZipModMod {
    // relative path
    manifest_path: PathBuf,
    manifest: ModManifest,
}
// TODO: cache this
fn has_manifest(entry: &PathBuf) -> Result<ZipMod> {
    let file = File::open(entry)?;
    let created_at = file.metadata()?.created()?;
    let mut archive = ZipArchive::new(file)?;

    let mut mods: Vec<ZipModMod> = Vec::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let file_name = file.name().to_string();

        if file_name.ends_with("manifest.json") {
            let mut content = String::new();
            file.read_to_string(&mut content)?;

            let manifest = json5::from_str(&content)?;

            mods.push(ZipModMod {
                manifest_path: Path::new(&file_name).to_path_buf(),
                manifest,
            });
        }
    }

    if mods.is_empty() {
        return Err(color_eyre::eyre::eyre!("No manifest found"));
    }

    Ok(ZipMod {
        created_at,
        path: entry.clone(),
        manifests: mods,
    })
}

pub fn find_zips_with_manifests(base_dir: &Path) -> Vec<ZipMod> {
    WalkDir::new(base_dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension() == Some(std::ffi::OsStr::new("zip")))
        .filter_map(|e: DirEntry| has_manifest(&e.into_path()).ok())
        .collect()
}

impl From<&ZipMod> for ModsZip {
    fn from(value: &ZipMod) -> Self {
        let desc = format_description!("[year repr:last_two]-[month]-[day] [hour]:[minute]");

        let mods: Vec<InstalledMod> = value
            .manifests
            .clone()
            .into_iter()
            .map(|x| InstalledMod {
                active: false,
                manifest: x.manifest,
                modified: value.created_at,
                path: x.manifest_path,
            })
            .collect();

        ModsZip {
            mods: mods_to_modelrc(&mods),
            created: OffsetDateTime::from(value.created_at)
                .format(desc)
                .expect("Failed formatting date")
                .into(),
            name: value.path.file_name().unwrap().to_string_lossy().to_string().into(),
            path: value.path.to_string_lossy().to_string().into(),
        }
    }
}

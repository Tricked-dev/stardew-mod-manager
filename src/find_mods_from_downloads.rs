use color_eyre::Result;
use dirs;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    path::{self, Path, PathBuf},
};
use walkdir::{DirEntry, WalkDir};
use zip::read::ZipArchive;

use crate::ModManifest;
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ZipMod {
    path: PathBuf,
    manifests: Vec<ZipModMod>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ZipModMod {
    // relative path
    manifest_path: PathBuf,
    manifest: ModManifest,
}

fn has_manifest(entry: &PathBuf) -> Result<ZipMod> {
    let file = File::open(entry)?;
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

    if mods.len() == 0 {
        return Err(color_eyre::eyre::eyre!("No manifest found"));
    }

    Ok(ZipMod {
        path: entry.clone(),
        manifests: mods,
    })
}

fn find_zips_with_manifests(base_dir: &Path) -> Vec<ZipMod> {
    WalkDir::new(base_dir)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension() == Some(std::ffi::OsStr::new("zip")))
        .filter_map(|e| has_manifest(&e.into_path()).ok())
        .collect()
}
#[test]
fn main() {
    let base_dir = dirs::download_dir().expect("Failed to find the downloads directory");
    println!("{:?}", base_dir);
    let zips_with_manifests = find_zips_with_manifests(&base_dir);
    for path in zips_with_manifests {
        println!("{:?}", path);
    }
}

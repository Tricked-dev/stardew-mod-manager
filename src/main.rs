use std::{
    ffi::OsStr,
    fs::{create_dir_all, read_dir, read_to_string, DirEntry},
    iter::Once,
    path::PathBuf,
};

use color_eyre::eyre::{EyreContext, Result};
use log::{debug, info};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use slint::{Model, ModelRc, SharedString, VecModel};
use tokio::sync::OnceCell;
use walkdir::WalkDir;

slint::include_modules!();

mod find_game;

const SVMM: &str = "SVMM";
#[derive(Clone, Debug)]
struct GameData {
    installation_path: PathBuf,
    svmm_path: PathBuf,
    profile_path: PathBuf,
    mods_path: PathBuf,
}

static GAME_DATA: OnceCell<GameData> = OnceCell::const_new();

async fn get_game_data<'a>() -> Result<&'a GameData> {
    if GAME_DATA.initialized() {
        return Ok(GAME_DATA.get().unwrap());
    }

    let game_dir = find_game::get_game_dir("Stardew Valley")?;

    info!("using game dir: {game_dir:?}");

    let svmm_dir = game_dir.join(SVMM);

    let profile_dir = svmm_dir.clone().join("profiles");

    let mods_dir = game_dir.join("Mods");

    info!("using svmm dir: {svmm_dir:?}");

    if !profile_dir.try_exists()? {
        info!("Creating svmm directories");
        create_dir_all(&profile_dir)?;
    }

    if read_dir(&profile_dir)?.count() == 0 {
        info!("Creating svmm default profiles");
        for i in 1..=3 {
            let path = svmm_dir.join(format!("profiles/Profile {}", i));
            create_dir_all(path.join("enabled"))?;
            create_dir_all(path.join("disabled"))?;
        }
    }
    Ok(GAME_DATA
        .get_or_init(|| async {
            GameData {
                installation_path: game_dir,
                svmm_path: svmm_dir,
                profile_path: profile_dir,
                mods_path: mods_dir,
            }
        })
        .await)
}

async fn get_profiles() -> Result<Vec<DirEntry>> {
    let game_data = get_game_data().await?;
    Ok(read_dir(&game_data.profile_path)?
        .flat_map(|entry| entry.ok())
        .collect::<Vec<_>>())
}

async fn get_active_profile() -> Result<String> {
    let game_data = get_game_data().await?;

    let selected_profile = read_to_string(game_data.mods_path.join(".profile"));

    let result = match selected_profile {
        Ok(s) => s,
        _ => {
            let profiles = get_profiles().await?;
            profiles
                .get(0)
                .expect("No profiles found")
                .file_name()
                .to_string_lossy()
                .to_string()
        }
    };

    info!("active profile: {result}");

    Ok(result)
}

async fn get_profiles_names() -> Result<Vec<String>> {
    let profiles = get_profiles().await?;
    Ok(profiles
        .iter()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ModManifest {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "UniqueID")]
    unique_id: String,
}

struct InstalledMod {
    path: PathBuf,
    active: bool,
    manifest: ModManifest,
}

async fn load_mods_from_dir(path: &PathBuf, active: bool) -> Result<Vec<InstalledMod>> {
    let mut result = Vec::new();
    for entry in WalkDir::new(path).max_depth(3) {
        let entry = entry?;

        if entry.file_name() == OsStr::new("manifest.json") {
            let manifest: ModManifest = json5::from_str(read_to_string(entry.path())?.as_str())?;
            debug!(
                "found active mod: {} id: {}",
                &manifest.name, &manifest.unique_id
            );
            let imod = InstalledMod {
                // impossible
                path: entry.path().parent().unwrap().to_path_buf(),
                active,
                manifest,
            };
            result.push(imod);
        }
    }

    Ok(result)
}

fn mods_to_modelrc(mods: &[InstalledMod]) -> ModelRc<Mod> {
    let mods = mods
        .iter()
        .map(|imod| Mod {
            text: imod.manifest.name.clone().into(),
            id: imod.manifest.unique_id.clone().into(),
            path: imod.path.to_string_lossy().to_string().into(),
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(mods))
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "stardew_mod_manager=debug");
    }
    env_logger::init();

    let _ = get_game_data().await?;

    let ui = AppWindow::new()?;

    ui.set_profiles(ModelRc::new(VecModel::from(
        get_profiles_names()
            .await?
            .iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )));
    let game_data = get_game_data().await?;

    let active_mods = load_mods_from_dir(&game_data.mods_path, true).await?;

    let profile = get_active_profile().await?;

    let disabled_mods = game_data.profile_path.join(profile).join("disabled");

    let inactive_mods = load_mods_from_dir(&disabled_mods, false).await?;

    ui.set_enabledMods(mods_to_modelrc(&active_mods));
    ui.set_disabledMods(mods_to_modelrc(&inactive_mods));

    ui.on_select_change(|s| {
        info!("Changing profile to: {s:?}");
    });

    ui.run()?;
    Ok(())
}

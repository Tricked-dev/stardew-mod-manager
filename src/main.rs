use std::{
    ffi::OsStr,
    fs::{create_dir_all, read, read_dir, read_to_string, rename, write, DirEntry},
    future::Future,
    io,
    iter::Once,
    path::PathBuf,
    sync::Arc,
    time::SystemTime,
};

use color_eyre::eyre::{EyreContext, Result};
use futures::FutureExt;
use futures::TryFutureExt;
use log::{debug, error, info};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use slint::{
    private_unstable_api::re_exports::euclid::default, Model, ModelRc, SharedString, VecModel, Weak,
};
use tokio::{sync::OnceCell, task::JoinHandle};
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
        Ok(s) => s.trim().to_string(),
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
    #[serde(rename = "Author")]
    author: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Description")]
    description: Option<String>,
    #[serde(rename = "UniqueID")]
    unique_id: String,
}
#[derive(Clone, Debug)]
struct InstalledMod {
    path: PathBuf,
    active: bool,
    modified: SystemTime,
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
                modified: entry.metadata()?.modified()?,
                active,
                manifest,
            };
            result.push(imod);
        }
    }
    result.sort_by(|a, b| a.modified.cmp(&b.modified));

    Ok(result)
}

impl From<&InstalledMod> for Mod {
    fn from(imod: &InstalledMod) -> Self {
        Mod {
            text: imod.manifest.name.clone().into(),
            id: imod.manifest.unique_id.clone().into(),
            author: imod.manifest.author.clone().into(),
            description: imod.manifest.description.clone().unwrap_or_default().into(),
            version: imod.manifest.version.clone().into(),
            path: imod.path.to_string_lossy().to_string().into(),
            active: imod.active,
            nexus: "".into(),
        }
    }
}

fn mods_to_modelrc(mods: &[InstalledMod]) -> ModelRc<Mod> {
    let mods = mods.iter().map(Mod::from).collect::<Vec<_>>();
    ModelRc::new(VecModel::from(mods))
}

async fn find_mod<A: AsRef<str>>(id: A) -> Result<InstalledMod> {
    let id = id.as_ref();

    let (active_mods, inactive_mods) = load_mods().await?;

    let active_mod = active_mods
        .iter()
        .find(|imod| imod.manifest.unique_id == id);
    let inactive_mod = inactive_mods
        .iter()
        .find(|imod| imod.manifest.unique_id == id);

    match (active_mod, inactive_mod) {
        (Some(active_mod), _) => Ok(active_mod.clone()),
        (_, Some(inactive_mod)) => Ok(inactive_mod.clone()),
        _ => Err(io::Error::new(io::ErrorKind::NotFound, "Mod not found").into()),
    }
}

async fn switch_mod<A: AsRef<str>>(id: A) -> Result<()> {
    let game_data = get_game_data().await?;

    let profile = get_active_profile().await?;

    let disabled_mods = game_data.profile_path.join(profile).join("disabled");
    let active_mods_dir = &game_data.mods_path;

    let inactive_mods = load_mods_from_dir(&disabled_mods, false).await?;
    let active_mods = load_mods_from_dir(active_mods_dir, true).await?;

    let id = id.as_ref();

    let active_mod = active_mods
        .iter()
        .find(|imod| imod.manifest.unique_id == id);
    let inactive_mod = inactive_mods
        .iter()
        .find(|imod| imod.manifest.unique_id == id);

    match (active_mod, inactive_mod) {
        (Some(active_mod), None) => {
            info!("Making mod {id} inactive");
            // Move from active mods to inactive mods
            let relative_path = active_mod.path.strip_prefix(active_mods_dir)?;
            let target_path = disabled_mods.join(relative_path);
            if let Some(parent_dir) = target_path.parent() {
                create_dir_all(parent_dir)?; // Ensure parent directories exist
            }
            rename(&active_mod.path, target_path)?;
            info!("Moved mod to inactive: {}", id);
        }
        (None, Some(inactive_mod)) => {
            info!("Making mod {id} active");
            // Move from inactive mods to active mods
            let relative_path = inactive_mod.path.strip_prefix(&disabled_mods)?;
            let target_path = active_mods_dir.join(relative_path);
            if let Some(parent_dir) = target_path.parent() {
                create_dir_all(parent_dir)?; // Ensure parent directories exist
            }
            rename(&inactive_mod.path, target_path)?;
            info!("Moved mod to active: {}", id);
        }
        (Some(_), Some(_)) => {
            info!("Mod is present in both active and inactive dirs: {}", id);
            Err(io::Error::new(
                io::ErrorKind::Other,
                "Mod is in both active and inactive directories.",
            ))?;
        }
        (None, None) => {
            info!("Mod not found: {}", id);
            Err(io::Error::new(io::ErrorKind::NotFound, "Mod not found."))?;
        }
    }

    Ok(())
}

async fn reload(handle_copy: Weak<AppWindow>) -> Result<()> {
    let (active_mods, inactive_mods) = load_mods().await?;

    let profiles = get_profiles_names().await?;
    let profile = get_active_profile().await?;

    slint::invoke_from_event_loop(move || {
        let ui_weak = handle_copy.unwrap();

        ui_weak.set_profiles(ModelRc::new(VecModel::from(
            profiles.iter().map(SharedString::from).collect::<Vec<_>>(),
        )));

        ui_weak.set_profile(profile.into());

        ui_weak.set_enabledMods(mods_to_modelrc(&active_mods));
        ui_weak.set_disabledMods(mods_to_modelrc(&inactive_mods));
    })
    .unwrap();

    Ok(())
}

async fn load_mods() -> Result<(Vec<InstalledMod>, Vec<InstalledMod>)> {
    let game_data = get_game_data().await?;

    let active_mods = load_mods_from_dir(&game_data.mods_path, true).await?;

    let profile = get_active_profile().await?;

    let disabled_mods = game_data.profile_path.join(profile).join("disabled");

    let inactive_mods = load_mods_from_dir(&disabled_mods, false).await?;

    Ok((active_mods, inactive_mods))
}

fn spawn_logging<T, O: 'static>(future: T) -> JoinHandle<()>
where
    T: Future<Output = Result<O, color_eyre::eyre::Report>> + Send + 'static,
    T: Future + Send + 'static,
    T::Output: Send + 'static,
{
    tokio::spawn(async {
        let result = future.await;
        if let Err(err) = result {
            log::error!("{}", err);
        }
    })
}

async fn set_mod_active(modid: String, handle_copy: Weak<AppWindow>) -> Result<()> {
    let imod = find_mod(&modid).await?;

    slint::invoke_from_event_loop(move || {
        let ui_weak = handle_copy.unwrap();

        if ui_weak.get_active_mod().id == modid && ui_weak.get_active_mod_active() {
            ui_weak.set_active_mod_active(false)
        } else {
            ui_weak.set_active_mod(Mod::from(&imod));
            ui_weak.set_active_mod_active(true);
        }
    })
    .unwrap();

    Ok(())
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

    reload(ui.as_weak()).await?;

    let handle_weak = ui.as_weak();
    ui.global::<Logic>().on_mod_move(move |value| {
        let v = value.to_string();
        let handle_copy = handle_weak.clone();
        spawn_logging(switch_mod(v).and_then(|_| reload(handle_copy)));
    });

    let handle_weak = ui.as_weak();
    ui.global::<Logic>().on_active_mod(move |value| {
        let v = value.to_string();
        let handle_copy = handle_weak.clone();
        spawn_logging(set_mod_active(v, handle_copy));
    });

    let handle_weak = ui.as_weak();
    ui.global::<Logic>().on_update_ui(move || {
        let handle_copy = handle_weak.clone();
        spawn_logging(reload(handle_copy));
    });

    let handle_weak = ui.as_weak();
    ui.on_select_change(move |s| {
        let handle_copy = handle_weak.clone();
        let s = s.to_string();
        spawn_logging(switch_to_profile(s).and_then(|_| reload(handle_copy)));
    });

    let handle_weak = ui.as_weak();
    ui.on_switch_mod(move || {
        let handle_copy = handle_weak.clone();
        let modid = handle_copy.unwrap().get_active_mod().id.clone();
        spawn_logging(switch_mod(modid.to_string()).and_then(|_| reload(handle_copy)));
    });

    ui.run()?;
    Ok(())
}

async fn switch_to_profile(profile: String) -> Result<()> {
    info!("Changing profile to: {profile:?}");
    let game_data = get_game_data().await?;
    let active_profile = get_active_profile().await?;

    for file in read_dir(&game_data.mods_path)? {
        let file = file?;
        if file.path().is_file() {
            continue;
        }
        rename(
            file.path(),
            game_data
                .profile_path
                .join(&active_profile)
                .join("enabled")
                .join(file.path().file_name().unwrap()),
        )?;
    }

    for file in read_dir(&game_data.profile_path.join(&profile).join("enabled"))? {
        let file = file?;
        if file.path().is_file() {
            continue;
        }
        rename(
            file.path(),
            &game_data.mods_path.join(file.path().file_name().unwrap()),
        )?;
    }

    write(game_data.mods_path.join(".profile"), &profile)?;

    info!("Done loading profile reloading mods!");

    Ok(())
}

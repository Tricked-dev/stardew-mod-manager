#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs::{create_dir_all, read_dir, read_to_string, rename, write, DirEntry, File},
    future::Future,
    io,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use color_eyre::eyre::Result;
use find_mods_from_downloads::{find_zips_with_manifests, ZipMod};
use futures::TryFutureExt;
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use slint::{Model, ModelRc, SharedString, VecModel, Weak};
use smapiapi::{resolve_mods, SmapiMod};
use tokio::{sync::OnceCell, task::JoinHandle};
use walkdir::WalkDir;
use zip::ZipArchive;

slint::include_modules!();

mod find_game;
mod find_mods_from_downloads;
mod smapiapi;

// pub use crate::ModsZip;

const SVMM: &str = "SVMM";

#[allow(unused)]
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

    if !&svmm_dir.join("deleted").try_exists()? {
        create_dir_all(&svmm_dir.join("deleted"))?;
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
    #[serde(alias = "name")]
    name: String,
    #[serde(rename = "Author")]
    #[serde(alias = "author")]
    author: String,
    #[serde(rename = "Version")]
    #[serde(alias = "version")]
    version: String,
    #[serde(rename = "Description")]
    #[serde(alias = "description")]
    description: Option<String>,
    #[serde(rename = "UniqueID")]
    #[serde(alias = "UniqueId")]
    #[serde(alias = "unique_id")]
    unique_id: String,
    #[serde(rename = "Dependencies")]
    #[serde(alias = "dependencies")]
    #[serde(default)]
    dependencies: Vec<ModDependency>,
    #[serde(rename = "UpdateKeys")]
    #[serde(alias = "update_keys")]
    #[serde(default)]
    update_keys: Vec<String>,
}

#[allow(unused)]
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ModDependency {
    #[serde(rename = "UniqueID")]
    #[serde(alias = "UniqueId")]
    unique_id: String,
    #[serde(rename = "Version")]
    #[serde(alias = "MinimumVersion")]
    version: Option<String>,
    #[serde(rename = "IsRequired")]
    #[serde(default)]
    required: bool,
    #[serde(flatten)]
    other: HashMap<String, serde_json::Value>,
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
            debug!("found active mod: {} id: {}", &manifest.name, &manifest.unique_id);
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
#[derive(Clone, Debug)]
struct ResolvedMissingDependency {
    mod_data: SmapiMod,
    for_mods: Vec<String>,
    required: bool,
}

async fn load_missing_dependencies() -> Result<Vec<ResolvedMissingDependency>> {
    let (active_mods, _) = load_mods().await?;
    let missing = find_missing_dependencies(&active_mods);
    debug!("Missing dependencies count: {}", missing.len());
    let mods = resolve_mods(
        missing
            .iter()
            .map(|x| SmapiMod {
                id: x.modid.to_string(),
                ..Default::default()
            })
            .collect(),
    )
    .await?;
    debug!("Resolved mods: {mods:?}");

    let result: Vec<ResolvedMissingDependency> = mods
        .into_iter()
        .map(|resolved_mod| {
            let dependent_mods: Vec<String> = missing
                .iter()
                .filter(|&missing_mod| missing_mod.modid == resolved_mod.id)
                .flat_map(|missing_mod| missing_mod.for_mods.clone()) // Flatten the Vec<Vec<String>> to Vec<String>
                .collect();

            let is_required = missing
                .iter()
                .any(|missing_mod| missing_mod.modid == resolved_mod.id && missing_mod.required);

            ResolvedMissingDependency {
                mod_data: resolved_mod,
                for_mods: dependent_mods,
                required: is_required,
            }
        })
        .collect();

    Ok(result)
}

impl From<&InstalledMod> for Mod {
    fn from(imod: &InstalledMod) -> Self {
        let mut rmod = Mod {
            text: imod.manifest.name.clone().into(),
            id: imod.manifest.unique_id.clone().into(),
            author: imod.manifest.author.clone().into(),
            description: imod.manifest.description.clone().unwrap_or_default().into(),
            version: imod.manifest.version.clone().into(),
            path: imod.path.to_string_lossy().to_string().into(),
            active: imod.active,
            nexus: "".into(),
            github: "".into(),
            moddrop: "".into(),
        };

        for entry in imod.manifest.update_keys.iter() {
            if let Some((key, value)) = entry.splitn(2, ':').collect::<Vec<_>>().split_first() {
                let value = value[0].split('@').next().expect("Failed splitting at @");
                //https://community.playstarbound.com/threads/<name>.<id>/ we could add this later but i dont know if theres extra value
                match key.to_ascii_lowercase().as_str() {
                    "nexus" => rmod.nexus = format!("https://nexusmods.com/stardewvalley/mods/{}", value).into(),
                    "github" => rmod.github = format!("https://github.com/{}", value).into(),
                    "moddrop" => rmod.moddrop = format!("https://www.moddrop.com/stardew-valley/mods/{}", value).into(),
                    _ => {
                        debug!("Unknown update key {key} {value:?}");
                    }
                }
            }
        }
        rmod
    }
}

fn generic_to_modelrc<I, M>(mods: &[I]) -> ModelRc<M>
where
    M: for<'a> From<&'a I> + 'static + Clone,
{
    let mods = mods.iter().map(M::from).collect::<Vec<_>>();
    ModelRc::new(VecModel::from(mods))
}

fn mods_to_modelrc(mods: &[InstalledMod]) -> ModelRc<Mod> {
    generic_to_modelrc::<InstalledMod, Mod>(mods)
}

async fn find_mod<A: AsRef<str>>(id: A) -> Result<InstalledMod> {
    let id = id.as_ref();

    let (active_mods, inactive_mods) = load_mods().await?;

    let active_mod = active_mods.iter().find(|imod| imod.manifest.unique_id == id);
    let inactive_mod = inactive_mods.iter().find(|imod| imod.manifest.unique_id == id);

    match (active_mod, inactive_mod) {
        (Some(active_mod), _) => Ok(active_mod.clone()),
        (_, Some(inactive_mod)) => Ok(inactive_mod.clone()),
        _ => Err(io::Error::new(io::ErrorKind::NotFound, "Mod not found").into()),
    }
}
#[derive(Clone, Debug)]
struct MissingDependency {
    // imod: Option<SmapiMod>,
    modid: String,
    for_mods: Vec<String>,
    required: bool,
}

fn find_missing_dependencies(mods: &[InstalledMod]) -> Vec<MissingDependency> {
    let installed_ids: HashSet<_> = mods
        .iter()
        .map(|imod| imod.manifest.unique_id.trim().to_owned())
        .collect();

    let mut missing_deps: HashMap<String, MissingDependency> = HashMap::new();

    for imod in mods.iter() {
        for dependency in &imod.manifest.dependencies {
            let dep_id = dependency.unique_id.trim().to_owned();

            if !installed_ids.contains(&dep_id) {
                let entry = missing_deps.entry(dep_id.clone()).or_insert_with(|| MissingDependency {
                    modid: dep_id.clone(),
                    for_mods: vec![],
                    required: dependency.required,
                });

                // Insert the ID of the mod that's missing the dependency.
                let id = imod.manifest.unique_id.trim().to_owned();
                if !entry.for_mods.contains(&id) {
                    entry.for_mods.push(id);
                }

                // Ensure required status is updated.
                entry.required |= dependency.required;
            }
        }
    }
    missing_deps.values().cloned().collect()
}

impl From<&ResolvedMissingDependency> for SmapiApiMod {
    fn from(smapi_mod: &ResolvedMissingDependency) -> Self {
        SmapiApiMod {
            id: smapi_mod.mod_data.id.clone().into(),
            name: smapi_mod.mod_data.metadata.name.clone().into(),
            url: smapi_mod.mod_data.metadata.main.url.clone().into(),
            required_for: generic_to_modelrc(&smapi_mod.for_mods),
            required: smapi_mod.required,
        }
    }
}

async fn set_missing_mods(handle_copy: Weak<AppWindow>) -> Result<()> {
    let mut mods = load_missing_dependencies().await?;

    mods.sort_by(|a, b| (b.required as usize, &b.mod_data.id).cmp(&(a.required as usize, &a.mod_data.id)));

    slint::invoke_from_event_loop(move || {
        let handle_copy = handle_copy.unwrap();

        let model = generic_to_modelrc::<ResolvedMissingDependency, SmapiApiMod>(&mods);

        handle_copy.set_missing_dependencies(model);
    })
    .unwrap();

    Ok(())
}

async fn remove_mod<A: AsRef<str>>(id: A) -> Result<()> {
    let imod = find_mod(id).await?;

    let game_data = get_game_data().await?;

    let deleted_folder = game_data.svmm_path.join("deleted");

    let location = deleted_folder.join(format!(
        "{}-{}",
        imod.path.file_name().unwrap().to_string_lossy(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
    ));

    rename(&imod.path, location)?;

    Ok(())
}

async fn switch_mod<A: AsRef<str>>(id: A) -> Result<()> {
    let game_data = get_game_data().await?;

    let profile = get_active_profile().await?;

    let disabled_mods = game_data.profile_path.join(profile).join("disabled");
    let active_mods_dir = &game_data.mods_path;

    let inactive_mods = load_mods_from_dir(&disabled_mods, false).await?;
    let active_mods = load_mods_from_dir(active_mods_dir, true).await?;

    let id = id.as_ref();

    let active_mod = active_mods.iter().find(|imod| imod.manifest.unique_id == id);
    let inactive_mod = inactive_mods.iter().find(|imod| imod.manifest.unique_id == id);

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
    let then = Instant::now();
    let (active_mods, inactive_mods) = load_mods().await?;

    let profiles = get_profiles_names().await?;
    let profile = get_active_profile().await?;
    let copy2 = handle_copy.clone();
    slint::invoke_from_event_loop(move || {
        let ui_weak = handle_copy.unwrap();

        ui_weak.set_profiles(ModelRc::new(VecModel::from(
            profiles.iter().map(SharedString::from).collect::<Vec<_>>(),
        )));

        ui_weak.set_profile(profile.into());

        ui_weak.set_enabledMods(mods_to_modelrc(&active_mods));
        ui_weak.set_disabledMods(mods_to_modelrc(&inactive_mods));

        debug!("Reloading took: {}ms", then.elapsed().as_millis())
    })
    .unwrap();

    // dont block the ui for *too* long
    tokio::task::spawn_blocking(move || {
        let base_dir = dirs::download_dir().expect("Failed to find the downloads directory");
        let zips_with_manifests = find_zips_with_manifests(&base_dir);
        slint::invoke_from_event_loop(move || {
            let ui_weak = copy2.unwrap();
            ui_weak.set_mods_zip(generic_to_modelrc::<ZipMod, ModsZip>(&zips_with_manifests));
        })
    });

    debug!("Reload exited in: {}ms", then.elapsed().as_millis());
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
            error!("{}", err);
        } else {
            debug!("Spawned future completed");
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

async fn clear_active_mod(handle_copy: Weak<AppWindow>) -> Result<()> {
    slint::invoke_from_event_loop(move || {
        let ui_weak = handle_copy.unwrap();

        ui_weak.set_active_mod_active(false)
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

    // inits the game data - do it here so errors stay pretty :D
    let _ = get_game_data().await?;

    let ui = AppWindow::new()?;

    // we love a quickly starting application
    spawn_logging(reload(ui.as_weak()));

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

    ui.global::<Magic>().on_open(move |s| {
        let result = opener::open(s.to_string());
        if let Err(err) = result {
            error!("{}", err);
        }
    });

    ui.global::<Magic>().on_join(move |s, joiner| {
        debug!("To join called");
        let list = s.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        list.join(joiner.as_ref()).into()
    });

    ui.global::<Magic>().on_idx(move |list, item| {
        debug!("To idx called");
        (list.iter().position(|x| x == item).unwrap() as u32)
            .try_into()
            .unwrap()
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

    let handle_weak = ui.as_weak();
    ui.on_get_missing_dependencies(move || {
        let handle_copy = handle_weak.clone();
        spawn_logging(set_missing_mods(handle_copy));
    });

    let handle_weak = ui.as_weak();
    ui.on_delete_mod(move || {
        let handle_copy = handle_weak.clone();
        let handle_copy2 = handle_weak.clone();
        let modid: String = handle_copy.unwrap().get_active_mod().id.clone().to_string();
        spawn_logging(
            remove_mod(modid.clone())
                .and_then(|_| clear_active_mod(handle_copy))
                .and_then(|_| reload(handle_copy2)),
        );
    });

    let handle_weak = ui.as_weak();
    ui.global::<Logic>().on_delete_zip(move |s| {
        let handle_copy = handle_weak.clone();
        spawn_logging(
            tokio::fs::remove_file(s.to_string())
                .map_err(|_| color_eyre::eyre::eyre!("Failed to delete file"))
                .and_then(|_| reload(handle_copy)),
        );
    });

    let handle_weak = ui.as_weak();
    ui.global::<Logic>().on_install_zip(move |s| {
        let handle_copy = handle_weak.clone();
        spawn_logging(unzip(s.to_string()).and_then(|_| reload(handle_copy)));
    });

    ui.run()?;
    Ok(())
}

async fn unzip<A: AsRef<str>>(zip_path: A) -> Result<()> {
    debug!("Installing mods from zip: {}", zip_path.as_ref());
    let data = get_game_data().await?;
    let p = PathBuf::from(zip_path.as_ref());
    let zip = File::open(&p)?;
    let mut archive = ZipArchive::new(zip)?;

    let mut path = data.mods_path.clone();

    // unzip to zip directory if theres a manifest.json instead of the mod being in a subdirectory!
    if archive.by_name("manifest.json").is_ok() {
        let dir = p.file_name().unwrap().to_string_lossy().replace(".zip", "");
        debug!("Mods not in subdirectory extracting to {dir}");
        path = path.join(dir);
    }
    debug!("Extracting to {path:?}");
    archive.extract(path)?;
    debug!("Extracting complete!");
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
        rename(file.path(), &game_data.mods_path.join(file.path().file_name().unwrap()))?;
    }

    write(game_data.mods_path.join(".profile"), &profile)?;

    info!("Done loading profile reloading mods!");

    Ok(())
}

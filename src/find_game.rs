use log::debug;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn get_game_dir(name: &str) -> io::Result<PathBuf> {
    get_steam_game_dir(name)
}

fn get_steam_game_dir(name: &str) -> io::Result<PathBuf> {
    let steam_dir = get_steam_dir().ok_or(io::Error::new(
        io::ErrorKind::NotFound,
        "Steam dir not found",
    ))?;

    let mut library_folders = get_library_folders(&steam_dir.join("steamapps/libraryfolders.vdf"))?;
    library_folders.push(steam_dir); // include the main Steam directory

    for library_folder in &library_folders {
        let game_dir = find_game_dir(&library_folder.join("steamapps/common"), name);
        if let Some(game_dir) = game_dir {
            debug!("found a game directory: {}", game_dir.display());
            return Ok(game_dir); // exit the program after finding the game
        }
    }

    Err(io::Error::new(io::ErrorKind::NotFound, "Game not found"))
}

fn get_library_folders(vdf_path: &Path) -> io::Result<Vec<PathBuf>> {
    let file = File::open(vdf_path)?;
    let reader = io::BufReader::new(file);

    let mut library_folders = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.contains("path") {
            let start = line.find('\"').unwrap() + 1; // starting quote before the path
            let end = line.rfind('\"').unwrap(); // ending quote after the path
            let path = &line[start..end]; // get the path with surrounding characters
            let path = path.trim_start_matches("path\"\t\t\""); // remove leading characters
            let path = if cfg!(windows) {
                path.replace("\\\\", "\\")
            } else {
                path.to_string()
            };
            library_folders.push(PathBuf::from(path));
        }
    }
    Ok(library_folders)
}

fn find_game_dir(steam_dir: &Path, game_name: &str) -> Option<PathBuf> {
    for entry in WalkDir::new(steam_dir).max_depth(1) {
        let entry = entry.unwrap();
        debug!("found game: {}", entry.file_name().to_string_lossy());
        if entry.file_type().is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(game_name)
        {
            return Some(entry.into_path());
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn get_steam_dir() -> Option<PathBuf> {
    let mut path = dirs::data_local_dir()?;
    path.push("Steam");
    Some(path)
}

#[cfg(target_os = "macos")]
fn get_steam_dir() -> Option<PathBuf> {
    let mut path = dirs::home_dir()?;
    path.push("Library/Application Support/Steam");
    Some(path)
}

#[cfg(target_os = "linux")]
fn get_steam_dir() -> Option<PathBuf> {
    let mut path = dirs::home_dir()?;
    path.push(".steam/steam");
    Some(path)
}

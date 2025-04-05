use keyvalues_parser::{Obj, Vdf};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub struct Steamium {
	steam_root: PathBuf,
}

fn get_steam_root() -> anyhow::Result<PathBuf> {
	let mut steam_path = PathBuf::from(std::env::var("HOME")?);
	steam_path.push(".steam/steam");
	if !steam_path.exists() {
		anyhow::bail!("Steam path {:?} doesn't exist", steam_path);
	}
	Ok(steam_path)
}

pub type AppID = u64;

#[derive(Debug, Serialize, Deserialize)]
pub struct AppManifest {
	app_id: AppID,
	name: String,
	raw_state_flags: u64, // documentation: https://github.com/lutris/lutris/blob/master/docs/steam.rst
	last_played: Option<u64>, // unix timestamp
}

pub enum GameSortMethod {
	NameAsc,
	NameDesc,
	PlayDateDesc,
}

fn get_obj_first<'a>(obj: &'a Obj<'_>, key: &str) -> Option<&'a Obj<'a>> {
	obj.get(key)?.first()?.get_obj()
}

fn get_str_first<'a>(obj: &'a Obj<'_>, key: &str) -> Option<&'a str> {
	obj.get(key)?.first()?.get_str()
}

fn vdf_parse_libraryfolders<'a>(vdf_root: &'a Vdf<'a>) -> Option<Vec<AppEntry>> {
	let obj_libraryfolders = vdf_root.value.get_obj()?;

	let mut res = Vec::<AppEntry>::new();

	let mut num = 0;
	loop {
		let Some(library_folder) = get_obj_first(obj_libraryfolders, format!("{}", num).as_str())
		else {
			// no more libraries to find
			break;
		};

		let Some(apps) = get_obj_first(library_folder, "apps") else {
			// no apps?
			num += 1;
			continue;
		};

		let Some(path) = get_str_first(library_folder, "path") else {
			// no path?
			num += 1;
			continue;
		};

		//log::trace!("path: {}", path);

		res.extend(
			apps
				.iter()
				.filter_map(|item| item.0.parse::<u64>().ok())
				.map(|app_id| AppEntry {
					app_id,
					root_path: String::from(path),
				}),
		);

		num += 1;
	}

	Some(res)
}

fn vdf_parse_appstate<'a>(app_id: AppID, vdf_root: &'a Vdf<'a>) -> Option<AppManifest> {
	let app_state_obj = vdf_root.value.get_obj()?;

	let name = app_state_obj.get("name")?.first()?.get_str()?;

	let raw_state_flags = app_state_obj
		.get("StateFlags")?
		.first()?
		.get_str()?
		.parse::<u64>()
		.ok()?;

	let last_played = match app_state_obj.get("LastPlayed") {
		Some(s) => Some(s.first()?.get_str()?.parse::<u64>().ok()?),
		None => None,
	};

	Some(AppManifest {
		app_id,
		name: String::from(name),
		raw_state_flags,
		last_played,
	})
}

struct AppEntry {
	pub root_path: String,
	pub app_id: AppID,
}

pub fn stop(app_id: AppID, force_kill: bool) -> anyhow::Result<()> {
	log::info!("Stopping Steam game with AppID {}", app_id);

	for game in list_running_games()? {
		if game.app_id != app_id {
			continue;
		}

		log::info!("Killing process with PID {} and its children", game.pid);
		let _ = std::process::Command::new("pkill")
			.arg(if force_kill { "-9" } else { "-11" })
			.arg("-P")
			.arg(format!("{}", game.pid))
			.spawn()?;
	}
	Ok(())
}

pub fn launch(app_id: AppID) -> anyhow::Result<()> {
	log::info!("Launching Steam game with AppID {}", app_id);
	call_steam(&format!("steam://run/{}", app_id))?;
	Ok(())
}

#[derive(Serialize)]
pub struct RunningGame {
	pub app_id: AppID,
	pub pid: i32,
}

pub fn list_running_games() -> anyhow::Result<Vec<RunningGame>> {
	let mut res = Vec::<RunningGame>::new();

	let entries = std::fs::read_dir("/proc")?;
	for entry in entries.into_iter().flatten() {
		let path_cmdline = entry.path().join("cmdline");
		let Ok(cmdline) = std::fs::read(path_cmdline) else {
			continue;
		};

		let proc_file_name = entry.file_name();
		let Some(pid) = proc_file_name.to_str() else {
			continue;
		};

		let Ok(pid) = pid.parse::<i32>() else {
			continue;
		};

		let args: Vec<&str> = cmdline
			.split(|byte| *byte == 0x00)
			.filter_map(|arg| match std::str::from_utf8(arg) {
				Ok(arg) => Some(arg),
				Err(_) => None,
			})
			.collect();

		let mut has_steam_launch = false;
		for arg in &args {
			if *arg == "SteamLaunch" {
				has_steam_launch = true;
				break;
			}
		}

		if !has_steam_launch {
			continue;
		}

		// Running game process found. Parse AppID
		for arg in &args {
			let pat = "AppId=";
			let Some(pos) = arg.find(pat) else {
				continue;
			};

			if pos != 0 {
				continue;
			}

			let Some((_, second)) = arg.split_at_checked(pat.len()) else {
				continue;
			};

			let Ok(app_id_num) = second.parse::<u64>() else {
				continue;
			};

			// AppID found. Add it to the list
			res.push(RunningGame {
				app_id: app_id_num as AppID,
				pid,
			});

			break;
		}
	}

	Ok(res)
}

fn call_steam(arg: &str) -> anyhow::Result<()> {
	match std::process::Command::new("xdg-open").arg(arg).spawn() {
		Ok(_) => Ok(()),
		Err(_) => {
			std::process::Command::new("steam").arg(arg).spawn()?;
			Ok(())
		}
	}
}

impl Steamium {
	fn get_dir_steamapps(&self) -> PathBuf {
		self.steam_root.join("steamapps")
	}

	pub fn new() -> anyhow::Result<Self> {
		let steam_root = get_steam_root()?;

		Ok(Self { steam_root })
	}

	fn get_app_manifest(&self, app_entry: &AppEntry) -> anyhow::Result<AppManifest> {
		let manifest_path = PathBuf::from(&app_entry.root_path)
			.join(format!("steamapps/appmanifest_{}.acf", app_entry.app_id));

		let vdf_data = std::fs::read_to_string(manifest_path)?;
		let vdf_root = keyvalues_parser::Vdf::parse(&vdf_data)?;

		let Some(manifest) = vdf_parse_appstate(app_entry.app_id, &vdf_root) else {
			anyhow::bail!("Failed to parse AppState");
		};

		Ok(manifest)
	}

	pub fn list_installed_games(
		&self,
		sort_method: GameSortMethod,
	) -> anyhow::Result<Vec<AppManifest>> {
		let path = self.get_dir_steamapps().join("libraryfolders.vdf");
		let vdf_data = std::fs::read_to_string(path)?;

		let vdf_root = keyvalues_parser::Vdf::parse(&vdf_data)?;

		let Some(apps) = vdf_parse_libraryfolders(&vdf_root) else {
			anyhow::bail!("Failed to fetch installed Steam apps");
		};

		let mut games: Vec<AppManifest> = apps
			.iter()
			.filter_map(|app_entry| {
				let manifest = match self.get_app_manifest(app_entry) {
					Ok(manifest) => manifest,
					Err(e) => {
						log::error!(
							"Failed to get app manifest for AppID {}: {}",
							app_entry.app_id,
							e
						);
						return None;
					}
				};
				Some(manifest)
			})
			.collect();

		match sort_method {
			GameSortMethod::NameAsc => {
				games.sort_by(|a, b| a.name.cmp(&b.name));
			}
			GameSortMethod::NameDesc => {
				games.sort_by(|a, b| b.name.cmp(&a.name));
			}
			GameSortMethod::PlayDateDesc => {
				games.sort_by(|a, b| b.last_played.cmp(&a.last_played));
			}
		}

		Ok(games)
	}
}

use std::{
    env,
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
};

use etcetera::{AppStrategy, AppStrategyArgs, choose_app_strategy};

use autocrap::schema::Config;

/// The built-in preset to use when neither --config nor --preset is specified.
/// Set to `None` to require an explicit argument at runtime.
pub const DEFAULT_PRESET: Option<&str> = Some("nocturn-osc");

const PRESETS: &[(&str, &str)] = &[
    ("nocturn-midi", include_str!("../config/nocturn-midi.json")),
    ("nocturn-osc", include_str!("../config/nocturn-osc.json")),
    (
        "nocturn-osc-raw",
        include_str!("../config/nocturn-osc-raw.json"),
    ),
];

fn app_strategy() -> Result<impl AppStrategy, etcetera::HomeDirError> {
    choose_app_strategy(AppStrategyArgs {
        top_level_domain: "io".to_string(),
        author: "autocrap".to_string(),
        app_name: "autocrap".to_string(),
    })
}

/// Returns the platform-specific config directory for autocrap, if it can be determined.
/// - Linux/macOS: `~/.config/autocrap`
/// - Windows:     `%APPDATA%\autocrap`
pub fn config_dir() -> Option<PathBuf> {
    app_strategy().ok().map(|s| s.config_dir())
}

/// Prints the platform config directory to stdout, or an error to stderr.
pub fn print_config_dir() {
    match config_dir() {
        Some(dir) => println!("{}", dir.display()),
        None => eprintln!("error: could not determine config directory"),
    }
}

/// Resolves a `Config` from the given options, in priority order:
///   1. `--config <FILE>` path (absolute: used directly; relative: searched in CWD then config dir)
///   2. `--preset <NAME>` built-in preset
///   3. `DEFAULT_PRESET` built-in preset
///   4. Error listing available options
pub fn resolve_config(
    config_path: Option<&PathBuf>,
    preset_name: Option<&str>,
) -> Result<Config, Box<dyn std::error::Error>> {
    if let Some(path) = config_path {
        return load_from_path(path);
    }

    let preset = preset_name.or(DEFAULT_PRESET);

    if let Some(name) = preset {
        return load_preset(name);
    }

    let available = preset_names().join(", ");
    Err(format!(
        "no config specified; use --config <FILE> or --preset <NAME> (available presets: {available})"
    )
    .into())
}

fn load_from_path(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    if path.is_absolute() {
        return load_file(path);
    }

    // Relative path: search CWD first, then the platform config dir.
    let mut searched = vec![];

    let cwd_path = env::current_dir()?.join(path);
    searched.push(cwd_path.clone());
    if cwd_path.exists() {
        return load_file(&cwd_path);
    }

    if let Some(cfg_dir) = config_dir() {
        let cfg_path = cfg_dir.join(path);
        searched.push(cfg_path.clone());
        if cfg_path.exists() {
            return load_file(&cfg_path);
        }
    }

    let searched_list = searched
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    Err(format!(
        "config file '{}' not found; searched: {searched_list}",
        path.display()
    )
    .into())
}

fn load_file(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let file = File::open(path).map_err(|e| format!("could not open '{}': {e}", path.display()))?;
    let config = serde_json::from_reader(BufReader::new(file))
        .map_err(|e| format!("could not parse '{}': {e}", path.display()))?;
    Ok(config)
}

fn load_preset(name: &str) -> Result<Config, Box<dyn std::error::Error>> {
    let json = PRESETS
        .iter()
        .find(|(preset, _)| *preset == name)
        .map(|(_, json)| *json)
        .ok_or_else(|| {
            format!(
                "unknown preset '{name}', available presets: {}",
                preset_names().join(", ")
            )
        })?;

    let config = serde_json::from_str(json)
        .map_err(|e| format!("could not parse built-in preset '{name}': {e}"))?;
    Ok(config)
}

fn preset_names() -> Vec<&'static str> {
    PRESETS.iter().map(|(name, _)| *name).collect()
}

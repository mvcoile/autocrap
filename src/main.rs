use std::{error::Error, fs::File, io::BufReader, path::PathBuf};

use clap::Parser;
use log::info;

use autocrap::config::Config;

/// The built-in preset to use when neither --config nor --preset is specified.
/// Set to `None` to require an explicit argument at runtime.
const DEFAULT_PRESET: Option<&str> = Some("nocturn-midi");

const PRESETS: &[(&str, &str)] = &[
    ("nocturn-midi", include_str!("../config/nocturn-midi.json")),
    ("nocturn-osc", include_str!("../config/nocturn-osc.json")),
    (
        "nocturn-osc-raw",
        include_str!("../config/nocturn-osc-raw.json"),
    ),
];

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Options {
    /// Path to a config file
    #[arg(short, long, value_name = "FILE", conflicts_with = "preset")]
    config: Option<PathBuf>,

    /// Use a built-in preset config: nocturn-midi, nocturn-osc, nocturn-osc-raw
    #[arg(short, long, value_name = "NAME", conflicts_with = "config")]
    preset: Option<String>,

    /// Set logging level: Off, Error, Warn, Info, Debug or Trace
    #[arg(short, long)]
    log: Option<log::LevelFilter>,
}

fn resolve_config(options: &Options) -> Result<Config, Box<dyn Error>> {
    // Priority: --config > --preset > DEFAULT_PRESET
    if let Some(path) = &options.config {
        let config = serde_json::from_reader(BufReader::new(File::open(path)?))?;
        return Ok(config);
    }

    let preset_name = options.preset.as_deref().or(DEFAULT_PRESET);

    if let Some(name) = preset_name {
        let json = PRESETS
            .iter()
            .find(|(preset, _)| *preset == name)
            .map(|(_, json)| *json)
            .ok_or_else(|| {
                let available: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
                format!(
                    "unknown preset {}, available presets: {}",
                    name,
                    available.join(", ")
                )
            })?;
        let config = serde_json::from_str(json)?;
        return Ok(config);
    }

    let available: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
    Err(format!(
        "no config specified; use --config <FILE> or --preset <NAME> (available presets: {})",
        available.join(", ")
    )
    .into())
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse();

    let mut log_builder = env_logger::Builder::new();
    if let Some(level_filter) = options.log {
        log_builder.filter_level(level_filter);
    }
    log_builder.init();

    let config = resolve_config(&options)?;
    info!("config: {:?}", config);

    autocrap::run(config)?;
    Ok(())
}

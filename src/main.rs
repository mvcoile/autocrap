mod config;

use std::path::PathBuf;

use clap::Parser;
use log::info;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Options {
    /// Path to a config file (searches current directory and platform config dir)
    #[arg(short, long, value_name = "FILE", conflicts_with = "preset")]
    config: Option<PathBuf>,

    /// Use a built-in preset config: nocturn-midi, nocturn-osc, nocturn-osc-raw
    #[arg(short, long, value_name = "NAME", conflicts_with = "config")]
    preset: Option<String>,

    /// Print the platform config directory and exit
    #[arg(long)]
    print_config_dir: bool,

    /// Set logging level: Off, Error, Warn, Info, Debug or Trace
    #[arg(short, long)]
    log: Option<log::LevelFilter>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse();

    if options.print_config_dir {
        config::print_config_dir();
        return Ok(());
    }

    let mut log_builder = env_logger::Builder::new();
    if let Some(level_filter) = options.log {
        log_builder.filter_level(level_filter);
    }
    log_builder.init();

    let cfg = config::resolve_config(options.config.as_ref(), options.preset.as_deref())?;
    info!("config: {:?}", cfg);

    autocrap::run(cfg)?;
    Ok(())
}

use std::{error::Error, fs::File, io::BufReader, path::PathBuf};

use clap::Parser;
use log::info;

use autocrap::config::Config;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Options {
    /// Set a config file
    #[arg(short, long, value_name = "FILE")]
    config: PathBuf,

    /// Set logging level: Off, Error, Warn, Info, Debug or Trace
    #[arg(short, long)]
    log: Option<log::LevelFilter>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse();

    let mut log_builder = env_logger::Builder::new();
    if let Some(level_filter) = options.log {
        log_builder.filter_level(level_filter);
    }
    log_builder.init();

    let config: Config = serde_json::from_reader(BufReader::new(File::open(&options.config)?))?;
    info!("config: {:?}", config);

    autocrap::run(config)?;
    Ok(())
}

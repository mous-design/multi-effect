mod config;
mod control;
mod effects;
mod engine;
mod http;
mod logging;
mod app;

use anyhow::Result;
use config::Config;
use config::snapshot::ConfigSnapshot;
use app::Signal;

fn main() -> Result<()> {
    let mut first_run = true;
    loop {
        let (cfg, verbose, skip_state) = Config::from_args()?;

        if first_run {
            if skip_state {
                ConfigSnapshot::remove_state_file(&cfg.state_save_path)?;
            }
            logging::init(&cfg.log_target, verbose)?;
            first_run = false;
        }

        let rt = tokio::runtime::Runtime::new()?;
        let sig = rt.block_on(app::run(cfg))?;
        drop(rt);  // ALL tokio tasks die here, unconditionally
        match sig {
            Signal::Exit => return Ok(()),
            Signal::Reload => {},
        }
    }
}
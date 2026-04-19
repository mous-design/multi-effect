mod config;
mod control;
mod effects;
mod engine;
mod http;
mod logging;

use anyhow::Result;
use tracing::info;

use config::Config;
use config::master;
use config::master::ConfigRequest;
use config::snapshot::ConfigSnapshot;

#[tokio::main]
async fn main() -> Result<()> {
    let (cfg, verbose, skip_state) = Config::from_args()?;

    if skip_state {
        ConfigSnapshot::remove_state_file(&cfg.state_save_path)?;
    }

    logging::init(&cfg.log_target, verbose)?;

    // --- Audio engine ---
    let (audio_handle, audio_streams) = engine::AudioEngine::build(
        cfg.in_channels, cfg.out_channels, cfg.sample_rate, cfg.buffer_size, cfg.audio_device.clone())?;
    audio_streams.play()?;

    // Extract what we need before moving cfg into the master.
    let http_port          = cfg.http_port;
    let state_save_interval = cfg.state_save_interval;

    // --- Spawn ConfigMaster ---
    let (master_tx, bus) = master::spawn(cfg, audio_handle)?;

    if http_port > 0 {
        http::run(http_port, master_tx.clone(), bus.clone());
    }

    // Periodic state save.
    if state_save_interval > 0 {
        let master_tx_save = master_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(state_save_interval));
            interval.tick().await; // skip first immediate tick
            loop {
                interval.tick().await;
                if master_tx_save.send(ConfigRequest::SaveState).await.is_err() { break; }
            }
        });
    }


    // --- Signal loop ---
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sig_int  = signal(SignalKind::interrupt())?;
        let mut sig_term = signal(SignalKind::terminate())?;
        let mut sig_hup  = signal(SignalKind::hangup())?;

        loop {
            tokio::select! {
                _ = sig_int.recv()  => { info!("SIGINT received, shutting down."); break; }
                _ = sig_term.recv() => { info!("SIGTERM received, shutting down."); break; }
                _ = sig_hup.recv()  => {
                    info!("SIGHUP received.");
                    master_tx.send(ConfigRequest::Reload { resp: None }).await.ok();
                }
            }
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;

    // Shutdown: ask master to save state, then drop the sender to let it exit.
    master_tx.send(ConfigRequest::SaveState).await.ok();
    drop(master_tx);

    info!("Shutting down.");
    Ok(())
}

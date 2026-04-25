use anyhow::Result;
use tracing::info;
use tokio::sync::mpsc;

use super::config::Config;
use super::config::master;
use super::config::master::ConfigRequest;
use super::engine;
use super::http;
pub(super) enum Signal {
    Reload,
    Exit,
}
pub async fn run(cfg: Config) -> Result<Signal> {
    // --- Audio engine ---
    let (audio_handle, audio_streams) = engine::AudioEngine::build(
        cfg.in_channels, cfg.out_channels, cfg.sample_rate, cfg.buffer_size, cfg.audio_device.clone())?;
    audio_streams.play()?;

    // Extract what we need before moving cfg into the master.
    let http_port          = cfg.http_port;
    let state_save_interval = cfg.state_save_interval;

    // Have a channel to reload on demand.
    let (reload_tx, reload_rx) = mpsc::channel::<()>(1);
    // --- Spawn ConfigMaster ---
    let (master_tx, bus) = master::spawn(cfg, audio_handle, reload_tx)?;

    if http_port > 0 {
        http::run(http_port, master_tx.clone(), bus.clone());
    }

    // Periodic state save.
    if state_save_interval > 0 {
        periodic_save(state_save_interval, &master_tx);
    }
    
    let sig = wait_signal(reload_rx).await?;

    // Shutdown: ask master to save state, then drop the sender to let it exit.
    master_tx.send(ConfigRequest::SaveState).await.ok();
    drop(master_tx);

    Ok(sig)
}

fn periodic_save(state_save_interval: u64, master_tx: &mpsc::Sender<ConfigRequest>) {
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

async fn wait_signal(mut reload_rx:  mpsc::Receiver<()>) -> Result<Signal> {
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
                _ = sig_hup.recv()  => { info!("SIGHUP received."); return Ok(Signal::Reload); }
                _ = reload_rx.recv() => { info!("Reload received."); return Ok(Signal::Reload); }
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::select! {
            r = tokio::signal::ctrl_c() => {
                r?;
                info!("Ctrl-C received, shutting down.");
            },
            _ = reload_rx.recv() => { info!("Reload received."); return Ok(Signal::Reload); }
        }
    }

    Ok(Signal::Exit)
}

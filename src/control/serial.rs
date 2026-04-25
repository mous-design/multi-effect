use anyhow::Result;
use tokio::sync::{mpsc, watch};
use tokio_serial::SerialPortBuilderExt;
use tracing::info;

use crate::config::master::ConfigRequest;
use super::EventBus;
use super::handle::handle_client;

pub struct SerialControl {
    alias:     String,
    device:    String,
    baud:      u32,
    bus:       EventBus,
    master_tx: mpsc::Sender<ConfigRequest>,
}

impl SerialControl {
    pub fn new(
        alias:     String,
        device:    String,
        baud:      u32,
        bus:       EventBus,
        master_tx: mpsc::Sender<ConfigRequest>,
    ) -> Self {
        Self { alias, device, baud, bus, master_tx }
    }

    pub async fn run(self, mut active_rx: watch::Receiver<bool>) -> Result<()> {
        let Self { alias, device, baud, bus, master_tx } = self;

        loop {
            if !*active_rx.borrow() { return Ok(()); }

            // Open port — retry until available (handles cold-start and hot-plug).
            let port = loop {
                match tokio_serial::new(&device, baud).open_native_async() {
                    Ok(p)  => break p,
                    Err(e) => {
                        tracing::debug!("Serial '{device}': {e} — retrying in 5s");
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                            _ = active_rx.changed() => {}
                        }
                        if !*active_rx.borrow() { return Ok(()); }
                    }
                }
            };
            info!("Serial '{device}': connected");
            if let Err(e) = handle_client(
                port,
                bus.clone(),
                master_tx.clone(),
                &alias,
                active_rx.clone(),
            ).await {
                tracing::warn!("Serial '{device}': {e}");
            }
            if !*active_rx.borrow() { return Ok(()); }

            // Reconnect delay
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                _ = active_rx.changed() => {}
            }
        }
    }
}

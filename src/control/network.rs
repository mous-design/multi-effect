use std::sync::{Arc, RwLock};

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use crate::config::master::ConfigRequest;
use super::EventBus;
use super::mapping::ControllerDef;
use super::handle::handle_client;

/// Text-based TCP control server.
///
/// One command per line (UTF-8).  Responds with `OK` or `ERR <reason>`.
///
/// # Commands
///
/// ```text
/// SET  <key.param> <value>     — set a single parameter, e.g. SET 04-reverb.wet 0.6
/// CTRL <channel_id> <value>    — mapped control (same as serial CTRL)
/// UPDATE <json>                — partial patch update
/// CHAINS <json>                — replace chain structure (full chain array)
/// RESET                        — reset all effect state
/// PROGRAM <0-127>              — load preset number
/// SAVE_PRESET <0-127>          — save current chains to preset slot in config.json
/// ```
///
/// All connected clients also receive outbound events from the bus:
/// `CTRL <channel_id> <raw>` for mapped params, `SET <key.param> <value>` otherwise.
///
/// Multiple clients per port are handled concurrently via `tokio::spawn`.
pub struct NetworkControl {
    alias:       String,
    host:        String,
    port:        u16,
    fallback:    bool,
    bus:         EventBus,
    pub mappings: Arc<RwLock<ControllerDef>>,
    master_tx:   mpsc::Sender<ConfigRequest>,
}

impl NetworkControl {
    pub fn new(
        alias:     String,
        host:      String,
        port:      u16,
        fallback:  bool,
        bus:       EventBus,
        mappings:  Arc<RwLock<ControllerDef>>,
        master_tx: mpsc::Sender<ConfigRequest>,
    ) -> Self {
        Self { alias, host, port, fallback, bus, mappings, master_tx }
    }

    pub async fn run(
        self,
        mut active_rx: watch::Receiver<bool>,
    ) -> Result<()> {
        if !*active_rx.borrow() { return Ok(()); }

        let listener = TcpListener::bind((self.host.as_str(), self.port)).await?;
        tracing::info!("Control server listening on {}:{}", self.host, self.port);

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (socket, addr) = result?;
                    tracing::info!("Control connection from {addr}");

                    let bus        = self.bus.clone();
                    let mappings   = Arc::clone(&self.mappings);
                    let fallback   = self.fallback;
                    let master_tx  = self.master_tx.clone();
                    let alias      = self.alias.clone();
                    let client_rx  = active_rx.clone();

                    tokio::spawn(async move {
                        if let Err(e) = handle_client(socket, bus, mappings, fallback, master_tx, &alias, client_rx).await {
                            tracing::warn!("Client {addr}: {e}");
                        }
                    });
                }
                _ = active_rx.changed() => {
                    if !*active_rx.borrow() {
                        tracing::info!("Net control on :{} deactivated", self.port);
                        return Ok(());
                    }
                }
            }
        }
    }
}

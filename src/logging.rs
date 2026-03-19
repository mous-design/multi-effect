use anyhow::Result;
use tracing::Level;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Syslog layer
// ---------------------------------------------------------------------------

struct SyslogLayer {
    logger: std::sync::Mutex<syslog::Logger<syslog::LoggerBackend, syslog::Formatter3164>>,
}

impl SyslogLayer {
    fn new() -> Result<Self> {
        let formatter = syslog::Formatter3164 {
            facility: syslog::Facility::LOG_DAEMON,
            hostname: None,
            process: "multi-effect".into(),
            pid: std::process::id(),
        };
        let logger = syslog::unix(formatter)?;
        Ok(Self { logger: std::sync::Mutex::new(logger) })
    }
}

struct MsgVisitor(String);

impl tracing::field::Visit for MsgVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" { self.0.push_str(value); }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write;
            let _ = write!(self.0, "{value:?}");
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SyslogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        let msg = format!("[{}] {}", event.metadata().target(), v.0);

        if let Ok(mut logger) = self.logger.lock() {
            let _ = match *event.metadata().level() {
                Level::ERROR => logger.err(&msg),
                Level::WARN  => logger.warning(&msg),
                Level::INFO  => logger.info(&msg),
                Level::DEBUG | Level::TRACE => logger.debug(&msg),
            };
        }
    }
}

// ---------------------------------------------------------------------------
// Public init
// ---------------------------------------------------------------------------

/// Initialise the global tracing subscriber.
///
/// `target`:  `"stderr"` (default) or `"syslog"`.
/// `verbose`: if true, force stderr and set level to `multi_effect=debug`.
///            Overrides `target`; ignored when `RUST_LOG` is set.
pub fn init(target: &str, verbose: bool) -> Result<()> {
    // RUST_LOG always wins; otherwise compute a sensible default.
    let filter = if std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else if verbose {
        EnvFilter::new("warn,multi_effect=debug")
    } else {
        EnvFilter::new("warn,multi_effect=info")
    };

    if verbose || target != "syslog" {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(SyslogLayer::new()?)
            .init();
    }
    Ok(())
}

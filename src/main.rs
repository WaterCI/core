use clap::Parser;
#[cfg(feature = "sentry-integration")]
use sentry;
#[cfg(feature = "sentry-integration")]
use sentry_tracing;
use std::env;
use std::sync::mpsc;
#[cfg(feature = "tracing-logs")]
use tracing_subscriber::layer::SubscriberExt;
#[cfg(feature = "tracing-logs")]
use tracing_subscriber::util::SubscriberInitExt;
#[cfg(feature = "tracing-logs")]
use tracing_subscriber::{fmt, EnvFilter};
use waterci_core::core::run;
use waterci_core::get_config;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = "Sync server for water-ci")]
struct Args {
    #[clap(short, long, default_value = "watercore.yml")]
    config_file: String,
}

fn try_init_logs() {
    #[cfg(feature = "tracing-logs")]
    let registry = tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env());
    #[cfg(feature = "sentry-integration")]
    let registry = registry.with(sentry_tracing::layer());

    registry.init();
}

pub(crate) fn try_init_sentry() -> Option<sentry::ClientInitGuard> {
    if let Ok(dsn) = env::var("SENTRY_DSN") {
        return Some(sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                traces_sample_rate: 1.0,
                attach_stacktrace: true,
                ..Default::default()
            },
        )));
    }
    None
}

fn main() {
    #[cfg(feature = "sentry-integration")]
    let _guard = try_init_sentry();
    #[cfg(feature = "tracing-logs")]
    try_init_logs();
    let args = Args::parse();
    let conf = get_config(&args.config_file).expect("Could not read config file");
    let (_should_stop_tx, should_stop_rx) = mpsc::channel();
    run(&conf, should_stop_rx).expect("Error! error! error!");
    todo!()
}

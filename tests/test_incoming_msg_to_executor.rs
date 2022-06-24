use ntest::timeout;
use pretty_assertions::assert_eq;
use std::borrow::Cow;
use std::net::TcpStream;
use std::sync::mpsc::{channel, Sender};
use std::thread::{sleep, JoinHandle};
use std::time::Duration;
use std::{env, thread};
use tracing::{debug, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};
use waterci_core::core::run;
use waterci_core::Config;
use waterlib::config::{JobConfig, RepositoryConfig};
use waterlib::net::ExecutorMessage::ExecutorStatusResponse;
use waterlib::net::ExecutorStatus::{Available, Busy};
use waterlib::net::{BuildRequestFromRepo, ExecutorMessage, IncomingMessage};

fn launch_core(config: Config) -> (JoinHandle<()>, Sender<bool>) {
    let (tx, rx) = channel();
    let handle = thread::spawn(move || {
        let _guard = try_init_sentry();
        let config = config.clone();
        run(&config, rx).expect("could not run");
    });
    (handle, tx)
}

fn try_init_logs() {
    let registry = tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env());

    registry.init();
}
pub(crate) fn try_init_sentry() -> Option<sentry::ClientInitGuard> {
    if let Ok(dsn) = env::var("SENTRY_DSN") {
        return Some(sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                environment: Some(Cow::Borrowed("tests")),
                traces_sample_rate: 1.0,
                attach_stacktrace: true,
                ..Default::default()
            },
        )));
    }
    None
}

#[test]
#[timeout(3000)]
fn test_simple_br() {
    try_init_logs();
    let _guard = try_init_sentry();
    let config = Config {
        incoming_bind_host: "127.0.0.1".to_string(),
        incoming_bind_port: 15632,
        executor_bind_host: "127.0.0.1".to_string(),
        executor_bind_port: 15633,
    };
    debug!("Launching core…");
    let (core_handle, should_stop_tx) = launch_core(config.clone());
    debug!("core launched, sleeping 1s");
    sleep(Duration::from_millis(1000));
    debug!("connecting as executor to core…");
    let mut executor_stream = TcpStream::connect(format!(
        "{}:{}",
        config.executor_bind_host, config.executor_bind_port
    ))
    .expect("could not connect to core");
    // let's register our executor
    ExecutorMessage::ExecutorRegister
        .write(&mut executor_stream)
        .expect("bzzzzzzt");
    let executor_id =
        match ExecutorMessage::from(&mut executor_stream).expect("could not read msg from core") {
            ExecutorMessage::ExecutorRegisterResponse { id } => id,
            _ => {
                panic!("Invalid msg from core");
            }
        };
    info!(
        "Executor successfully connected to core with id {}",
        executor_id
    );
    debug!("connecting to core incoming…");
    let incoming_stream = TcpStream::connect(format!(
        "{}:{}",
        config.incoming_bind_host, config.incoming_bind_port
    ))
    .expect("could not connect to core");
    let payload = BuildRequestFromRepo {
        repo_url: "test-repourl".to_string(),
        reference: "test-ref".to_string(),
        repo_config: RepositoryConfig {
            id: "test-repoconfig".to_string(),
            jobs: vec![JobConfig {
                id: "test".to_string(),
                name: "testing".to_string(),
                image: "testing:latest".to_string(),
                steps: vec!["echo \"hello world\"".to_string()],
            }],
        },
    };
    info!("Sending payload…");
    IncomingMessage::BuildRequestFromRepo(payload.clone())
        .write(incoming_stream)
        .expect("could not send incoming payload");
    info!("Waiting for ExecutorStatusQuery");
    if let ExecutorMessage::ExecutorStatusQuery =
        ExecutorMessage::from(&mut executor_stream).expect("Could not deserialize ExecutorMessage")
    {
        info!("Got ExecutorStatusQuery! Signaling ourselves as available.");
        ExecutorStatusResponse(Available)
            .write(&mut executor_stream)
            .unwrap();
    } else {
        panic!();
    }

    info!("Waiting for build request to arrive…");
    let br =
        ExecutorMessage::from(&mut executor_stream).expect("Could not deserialize ExecutorMessage");
    match br {
        ExecutorMessage::BuildRequest(br) => {
            assert_eq!(br, payload);
        }
        _ => {
            panic!("Got unexpected message: {br:?}");
        }
    }

    info!("Waiting for ExecutorStatusQuery, part 2");
    if let ExecutorMessage::ExecutorStatusQuery =
        ExecutorMessage::from(&mut executor_stream).expect("Could not deserialize ExecutorMessage")
    {
        ExecutorStatusResponse(Busy)
            .write(&mut executor_stream)
            .unwrap();
    } else {
        panic!();
    }

    should_stop_tx
        .send(true)
        .expect("Could not tell core to stop");
    core_handle.join().expect("could not join core handle");
}

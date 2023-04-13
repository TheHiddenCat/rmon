use std::{net::SocketAddr, convert::Infallible, time::Duration};

use tokio::{signal, sync::broadcast};
use futures::stream::Stream;
use serde::{Serialize, Deserialize};
use sysinfo::{System, SystemExt, CpuExt, ProcessExt};
use axum::{Router, routing::get, Server, response::{Html, Sse, sse::Event}, extract::State};
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
struct AppState {
    tx: broadcast::Sender<Snapshot>
}

#[derive(Serialize, Deserialize, Clone)]
struct Snapshot {
    cpu: Vec<f32>,
    ram: Ram,
    processes: Vec<Process>
}

#[derive(Serialize, Deserialize, Clone, PartialEq, PartialOrd)]
struct Ram {
    used: u64,
    available: u64,
    total: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct Process {
    name: String,
    memory: u64,
    cpu: f32,
}

impl From<&System> for Snapshot {
    fn from(sys: &System) -> Self {
        let cpu = sys
        .cpus()
        .iter()
        .map(|cpu| cpu.cpu_usage())
        .collect();

        let ram = Ram {
            used: sys.used_memory(),
            available: sys.available_memory(),
            total: sys.total_memory(),
        };

        let mut processes: Vec<Process> = sys
            .processes()
            .iter()
            .map(|(_, proc)| {
                Process {
                    name: proc.name().to_owned(),
                    memory: proc.memory(),
                    cpu: proc.cpu_usage(),
                }
            })
            .collect();

        processes.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap());

        Snapshot {
            cpu,
            ram,
            processes
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rmon=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let (tx, _) = broadcast::channel::<Snapshot>(1);
    let app_state = AppState { tx: tx.clone() };

    let router = Router::new()
        .route("/", get(get_html))
        .route("/api/sse", get(sse_handler))
        .with_state(app_state.clone());

    let _task = tokio::task::spawn(async move {
        let mut sys = System::new();
        loop {
            sys.refresh_cpu();
            sys.refresh_memory();
            sys.refresh_processes();

            let _ = tx.send(Snapshot::from(&sys));

            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    
    tracing::info!("listening on {}", addr);

    Server::bind(&addr)
        .serve(router.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

#[inline(always)]
async fn get_html() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut receiver = state.tx.subscribe();

    let stream = async_stream::stream! {
        while let Ok(msg) = receiver.recv().await {
            let json = serde_json::to_string(&msg).unwrap();
            let event = Event::default().data(json);
            yield Ok(event);
        };
    };

    Sse::new(stream)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

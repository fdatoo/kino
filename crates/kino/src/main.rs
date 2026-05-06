use tracing::Instrument;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    async {
        startup::run();
    }
    .instrument(tracing::info_span!(
        "kino::startup::run",
        request_id = tracing::field::Empty,
        binary = "kino"
    ))
    .await;

    Ok(())
}

fn init_tracing() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::Level::INFO.into())
                .from_env_lossy(),
        )
        .try_init()
}

mod startup {
    pub(super) fn run() {
        tracing::info!("ready");
    }
}

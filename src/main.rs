mod app;
#[cfg(feature = "server")]
mod backend;
mod contracts;

#[cfg(not(feature = "server"))]
fn main() {
    dioxus::launch(app::App);
}

#[cfg(feature = "server")]
#[tokio::main]
async fn main() -> dioxus::Result<()> {
    init_tracing();
    backend::run()
        .await
        .map_err(|error| dioxus::CapturedError::msg(error.to_string()))
}

#[cfg(feature = "server")]
fn init_tracing() {
    use dioxus::logger::tracing::{Level, subscriber::set_global_default};
    use tracing_subscriber::{EnvFilter, fmt, fmt::time::ChronoLocal};

    let level = if cfg!(debug_assertions) {
        Level::DEBUG
    } else {
        Level::INFO
    };
    let filter = EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy()
        .add_directive("dioxus_server=warn".parse().expect("valid log filter"))
        .add_directive("hyper_util=warn".parse().expect("valid log filter"));
    let subscriber = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_timer(ChronoLocal::new("%Y-%m-%d %H:%M:%S".to_owned()))
        .with_ansi(std::env::var_os("NO_COLOR").is_none());

    let result = if std::env::var_os("DIOXUS_CLI_ENABLED").is_some() {
        set_global_default(subscriber.without_time().finish())
    } else {
        set_global_default(subscriber.finish())
    };
    result.expect("tracing subscriber must only be initialized once");
}

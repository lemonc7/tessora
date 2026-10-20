use std::{env, fmt, io::IsTerminal};

use chrono::{SecondsFormat, Utc};
use chrono_tz::Tz;
use tracing_subscriber::{
    EnvFilter,
    fmt::{format::Writer, time::FormatTime},
};

use crate::error::{AppError, Result};

pub const DEFAULT_TIMEZONE: &str = "Asia/Shanghai";
pub const DEFAULT_FILTER: &str = "info";

#[derive(Clone, Copy)]
struct ZonedTimer(Tz);

impl FormatTime for ZonedTimer {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        let timestamp = Utc::now()
            .with_timezone(&self.0)
            .to_rfc3339_opts(SecondsFormat::Millis, false);
        writer.write_str(&timestamp)
    }
}

pub fn init() -> Result<()> {
    let timezone_name = env::var("TZ").unwrap_or_else(|_| DEFAULT_TIMEZONE.into());
    let timezone = timezone_name.parse::<Tz>();
    let filter_value = env::var("RUST_LOG").unwrap_or_else(|_| DEFAULT_FILTER.into());
    let filter = EnvFilter::try_new(&filter_value);

    // Install a usable subscriber before reporting configuration errors, so startup
    // failures also use the same structured logging path.
    tracing_subscriber::fmt()
        .with_env_filter(
            filter
                .as_ref()
                .cloned()
                .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER)),
        )
        .with_timer(ZonedTimer(timezone.unwrap_or(chrono_tz::Asia::Shanghai)))
        .with_ansi(std::io::stdout().is_terminal())
        .with_target(true)
        .compact()
        .init();

    let timezone = timezone.map_err(|_| {
        AppError::Config(format!(
            "TZ must be a valid IANA timezone, got {timezone_name:?}"
        ))
    })?;
    filter.map_err(|error| AppError::Config(format!("invalid RUST_LOG filter: {error}")))?;

    tracing::info!(
        timezone = %timezone,
        filter = %filter_value,
        "logging initialized"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timezone_is_valid() {
        assert!(DEFAULT_TIMEZONE.parse::<Tz>().is_ok());
    }
}

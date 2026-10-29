use std::fmt::Write as _;
use std::sync::Arc;

use james_core::logs::{InstanceLogs, LogLevel};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::{LogConfig, LogFormat};

/// Installs the global tracing subscriber. `RUST_LOG` takes precedence over the config.
/// Messages about an instance (and everything plugins log, also debug) are also kept in the
/// returned per-instance logs, whatever the configured level.
pub fn init(config: &LogConfig) -> Arc<InstanceLogs> {
    // The directives in effect: `RUST_LOG` if it is valid, else the configured level.
    let (filter, directives) = match EnvFilter::try_from_default_env() {
        Ok(filter) => (filter, std::env::var("RUST_LOG").unwrap_or_default()),
        Err(_) => (EnvFilter::new(&config.level), config.level.clone()),
    };
    let logs = Arc::new(InstanceLogs::default());
    let capture_plugin_debug = plugin_debug(&directives);
    let capture = InstanceLayer(logs.clone()).with_filter(capture_filter(capture_plugin_debug));
    let registry = tracing_subscriber::registry().with(capture);
    match config.format {
        LogFormat::Pretty => registry
            .with(tracing_subscriber::fmt::layer().with_filter(filter))
            .init(),
        LogFormat::Json => registry
            .with(tracing_subscriber::fmt::layer().json().with_filter(filter))
            .init(),
    }
    logs
}

/// Whether the filter directives show plugins' debug output: a directive for the `plugin` target
/// wins over a global one (as in `EnvFilter`).
fn plugin_debug(directives: &str) -> bool {
    let debug = |level: &str| matches!(level.trim().to_ascii_lowercase().as_str(), "debug" | "trace");
    let mut global = false;
    let mut specific = None;
    for directive in directives.split(',').map(str::trim) {
        match directive.rsplit_once('=') {
            Some((target, level)) => {
                let target = target.split('[').next().unwrap_or_default();
                if target == "plugin" || target.starts_with("plugin::") {
                    specific = Some(debug(level));
                }
            }
            None => global = debug(directive),
        }
    }
    specific.unwrap_or(global)
}

/// James' own messages (`james*` crates, not libraries such as the TeamSpeak protocol) and what
/// plugins log (`plugin`; debug only when the configured level shows debug output, as plugins
/// may log their settings there). `Targets` matches prefixes.
fn capture_filter(plugin_debug: bool) -> Targets {
    let plugin = if plugin_debug { Level::DEBUG } else { Level::INFO };
    Targets::new()
        .with_target("plugin", plugin)
        .with_target("james", Level::INFO)
}

/// Copies events that name an instance (in a field or an enclosing span) into its log.
struct InstanceLayer(Arc<InstanceLogs>);

/// The instance of a span (see `james_runtime`'s instance tasks).
struct SpanInstance(String);

#[derive(Default)]
struct Fields {
    message: String,
    instance: Option<String>,
    plugin: Option<String>,
    /// `instance_log = false`: only for James' own log output.
    skip: bool,
    rest: String,
}

impl Visit for Fields {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record(field, value.to_string());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record(field, value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record(field, format!("{value:?}"));
    }
}

impl Fields {
    fn record(&mut self, field: &Field, value: String) {
        match field.name() {
            "message" => self.message = value,
            "instance" => self.instance = Some(value),
            "plugin" => self.plugin = Some(value),
            "instance_log" => self.skip = value == "false",
            name => {
                let _ = write!(self.rest, " {name}={value}");
            }
        }
    }
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for InstanceLayer {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        if let (Some(instance), Some(span)) = (fields.instance, ctx.span(id)) {
            span.extensions_mut().insert(SpanInstance(instance));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        if fields.skip {
            return;
        }
        let instance = fields.instance.or_else(|| {
            ctx.event_scope(event)?
                .find_map(|span| span.extensions().get::<SpanInstance>().map(|i| i.0.clone()))
        });
        let Some(instance) = instance.filter(|i| !i.is_empty()) else {
            return;
        };
        let level = match *event.metadata().level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            Level::INFO => LogLevel::Info,
            _ => LogLevel::Debug,
        };
        // Only plugins' own output is shown as theirs; James' messages about a plugin name it.
        let (plugin, about) = match (event.metadata().target() == "plugin", fields.plugin) {
            (true, plugin) => (plugin, String::new()),
            (false, Some(plugin)) => (None, format!(" plugin={plugin}")),
            (false, None) => (None, String::new()),
        };
        let message = format!("{}{}{}", fields.message, about, fields.rest);
        self.0.push(&instance, level, plugin.as_deref(), &message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_debug_follows_the_filter() {
        assert!(plugin_debug("debug"));
        assert!(plugin_debug("info,plugin=debug"));
        assert!(!plugin_debug("info,james_api=debug"));
        assert!(!plugin_debug("info"));
        assert!(!plugin_debug("debug,plugin=info"), "the specific directive wins");
        assert!(plugin_debug("info,plugin[x]=debug"));
    }

    #[test]
    fn events_about_an_instance_are_kept() {
        let logs = Arc::new(InstanceLogs::default());
        let subscriber = tracing_subscriber::registry()
            .with(InstanceLayer(logs.clone()).with_filter(capture_filter(false)));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(instance = %"a", error = "boom", "connection failed");
            tracing::info!(instance = %"a", plugin = %"greeter", "plugin started");
            tracing::warn!(target: "plugin", plugin = %"greeter", instance = %"a", "hello");
            tracing::debug!(target: "plugin", plugin = %"greeter", instance = %"a", "secret settings");
            tracing::info!("not about an instance");
            tracing::warn!(target: "tsproto::resend", instance = %"a", "library detail");
            tracing::debug!(instance = %"a", "debug of James");
            let span = tracing::info_span!("instance", instance = %"b");
            span.in_scope(|| tracing::error!("inside"));
        });
        let a = logs.since("a", 0, 10);
        assert_eq!(a.len(), 3, "no library or debug messages");
        assert_eq!(a[0].message, "connection failed error=boom");
        assert_eq!(
            (a[1].plugin.as_deref(), a[1].message.as_str()),
            (None, "plugin started plugin=greeter")
        );
        assert_eq!(
            (a[2].level, a[2].plugin.as_deref()),
            (LogLevel::Warn, Some("greeter"))
        );
        assert_eq!(logs.since("b", 0, 10)[0].message, "inside");
    }
}

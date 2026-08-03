//! Typed, fail-fast configuration.
//!
//! Every setting is sourced, in ascending order of precedence, from:
//!
//! 1. the compiled-in defaults on [`Config`],
//! 2. an optional TOML file (`atlas.toml`, or `$ATLAS_CONFIG_FILE`),
//! 3. `ATLAS_*` environment variables.
//!
//! A `.env` file is *not* read by the process itself — that is the job of the
//! shell or the process supervisor. See `.env.example` for the full list.

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::Deserialize;

/// Prefix for every Atlas environment variable.
pub const ENV_PREFIX: &str = "ATLAS_";

/// Environment variable naming the TOML config file to load.
pub const CONFIG_FILE_ENV: &str = "ATLAS_CONFIG_FILE";

/// Default TOML config file, loaded from the working directory when present.
pub const DEFAULT_CONFIG_FILE: &str = "atlas.toml";

/// Which deployment this process believes it is.
///
/// Drives log formatting and whether a master key is mandatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppEnv {
    /// Local development: pretty logs, master key optional.
    #[default]
    Dev,
    /// Production: JSON logs, master key required.
    Prod,
}

/// How log events are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable, multi-line, coloured.
    Pretty,
    /// One JSON object per line, for log shippers.
    Json,
}

/// A string that must never reach a log line, a `Debug` dump, or an API response.
///
/// The whole point of the type is that [`fmt::Debug`] and [`fmt::Display`] are
/// dead ends, so `tracing::info!(?config)` cannot leak the master key. There is
/// deliberately no `Serialize` impl. Phase 11 replaces this with the full
/// zeroizing `Secret<T>` from the vault; until then this is the minimum needed
/// to honour the non-negotiable in `CLAUDE.md`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps a string as a secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Reveals the underlying secret.
    ///
    /// Deliberately verbose and greppable: every call site is an audit point.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Fully resolved Atlas configuration.
///
/// `Debug` is safe to log: [`SecretString`] redacts itself.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Address the HTTP server binds to. `ATLAS_BIND_ADDR`.
    #[serde(default = "default_bind_addr")]
    pub bind_addr: SocketAddr,

    /// SQLite connection URL. `ATLAS_DATABASE_URL`.
    #[serde(default = "default_database_url")]
    pub database_url: String,

    /// Directory for the database, uploads and attachments. `ATLAS_DATA_DIR`.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,

    /// Directory for cloned repositories / agent workspaces. `ATLAS_WORKSPACE_DIR`.
    #[serde(default = "default_workspace_dir")]
    pub workspace_dir: PathBuf,

    /// `tracing` filter directives. `ATLAS_LOG_LEVEL`. `RUST_LOG` overrides this.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Log rendering. `ATLAS_LOG_FORMAT`. Defaults to pretty in dev, JSON in prod.
    #[serde(default)]
    pub log_format: Option<LogFormat>,

    /// Deployment environment. `ATLAS_ENV`.
    #[serde(default)]
    pub env: AppEnv,

    /// Base64 master key for the secrets vault. `ATLAS_MASTER_KEY`. Required in prod.
    #[serde(default)]
    pub master_key: Option<SecretString>,

    /// Comma-separated CORS origins, or `*`. `ATLAS_CORS_ALLOWED_ORIGINS`.
    #[serde(default = "default_cors_allowed_origins")]
    pub cors_allowed_origins: String,

    /// Size of the reader pool. `ATLAS_READER_POOL_SIZE`. The writer pool is always 1.
    #[serde(default = "default_reader_pool_size")]
    pub reader_pool_size: u32,

    /// Directory of the built frontend assets to serve. `ATLAS_STATIC_DIR`.
    /// When absent (the default) no static files are served — the dev Vite server
    /// proxies API calls instead. Set to `frontend/dist` in production.
    #[serde(default)]
    pub static_dir: Option<PathBuf>,

    /// This instance's externally-reachable base URL (e.g. `https://atlas.example.com`),
    /// no trailing slash. `ATLAS_PUBLIC_URL`.
    ///
    /// The one piece of self-knowledge Atlas cannot derive from `bind_addr` — a loopback
    /// or LAN bind address tells GitHub nothing about where to actually reach this
    /// instance. When set, linking a project to a repo also installs a GitHub webhook
    /// pointed at `{public_url}/webhooks/github`. When absent (the default — most
    /// instances are behind NAT or have no public address at all), no webhook is
    /// installed and Atlas has no push-driven updates for that repo until the poll
    /// fallback exists.
    #[serde(default)]
    pub public_url: Option<String>,
}

fn default_bind_addr() -> SocketAddr {
    ([127, 0, 0, 1], 8080).into()
}

fn default_database_url() -> String {
    "sqlite://data/atlas.db".to_owned()
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_workspace_dir() -> PathBuf {
    PathBuf::from("workspaces")
}

fn default_log_level() -> String {
    "info,atlas=debug,tower_http=info,sqlx=warn".to_owned()
}

fn default_cors_allowed_origins() -> String {
    "http://localhost:5173".to_owned()
}

fn default_reader_pool_size() -> u32 {
    8
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind_addr: default_bind_addr(),
            database_url: default_database_url(),
            data_dir: default_data_dir(),
            workspace_dir: default_workspace_dir(),
            log_level: default_log_level(),
            log_format: None,
            env: AppEnv::default(),
            master_key: None,
            cors_allowed_origins: default_cors_allowed_origins(),
            reader_pool_size: default_reader_pool_size(),
            static_dir: None,
            public_url: None,
        }
    }
}

/// A configuration problem, reported with the environment variable to fix.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(String);

impl Config {
    /// Loads configuration from defaults, then the TOML file, then `ATLAS_*` vars.
    ///
    /// Fails fast, naming the offending variable.
    pub fn load() -> Result<Self, ConfigError> {
        let config_file =
            std::env::var(CONFIG_FILE_ENV).unwrap_or_else(|_| DEFAULT_CONFIG_FILE.to_owned());
        Self::from_figment(&Self::figment(Path::new(&config_file)))
    }

    /// Builds the provider chain. Split out so tests can exercise it in isolation.
    fn figment(config_file: &Path) -> Figment {
        Figment::new()
            // A missing TOML file is not an error: env-only deployment is normal.
            .merge(Toml::file(config_file))
            .merge(Env::prefixed(ENV_PREFIX))
    }

    fn from_figment(figment: &Figment) -> Result<Self, ConfigError> {
        let config: Self = figment
            .extract()
            .map_err(|err| ConfigError(describe(&err)))?;
        config.validate()?;
        Ok(config)
    }

    /// Rejects combinations that would fail later, at a worse time.
    fn validate(&self) -> Result<(), ConfigError> {
        if let Some(key) = &self.master_key
            && key.expose_secret().trim().is_empty()
        {
            return Err(ConfigError(format!(
                "{ENV_PREFIX}MASTER_KEY is set but empty. Unset it, or generate one with:\n  \
                 openssl rand -base64 32"
            )));
        }

        if self.env == AppEnv::Prod && self.master_key.is_none() {
            return Err(ConfigError(format!(
                "{ENV_PREFIX}MASTER_KEY is required when {ENV_PREFIX}ENV=prod — without it the \
                 secrets vault cannot decrypt stored API keys. Generate one with:\n  \
                 openssl rand -base64 32"
            )));
        }

        if self.reader_pool_size == 0 {
            return Err(ConfigError(format!(
                "{ENV_PREFIX}READER_POOL_SIZE must be at least 1 (got 0)."
            )));
        }

        Ok(())
    }

    /// Log format, resolving the dev/prod default when unset.
    pub fn log_format(&self) -> LogFormat {
        self.log_format.unwrap_or(match self.env {
            AppEnv::Dev => LogFormat::Pretty,
            AppEnv::Prod => LogFormat::Json,
        })
    }

    /// CORS origins, split and trimmed. A single `*` means "any origin".
    pub fn cors_origins(&self) -> Vec<&str> {
        self.cors_allowed_origins
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .collect()
    }

    /// Whether CORS is configured to allow any origin.
    pub fn cors_allows_any_origin(&self) -> bool {
        self.cors_origins() == ["*"]
    }

    /// Creates the directories Atlas writes to.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.workspace_dir)?;
        Ok(())
    }
}

/// Turns a figment error into a message that names the environment variable to fix.
///
/// figment reports a dotted path (`reader_pool_size`); operators set
/// `ATLAS_READER_POOL_SIZE`. Translating between the two is the whole point.
fn describe(err: &figment::Error) -> String {
    use std::fmt::Write as _;

    let mut out = String::from("invalid Atlas configuration:");
    for e in err.clone() {
        // Writing to a String is infallible; the Result exists only to satisfy
        // the `fmt::Write` signature.
        if e.path.is_empty() {
            let _ = write!(out, "\n  - {e}");
        } else {
            let var = format!("{ENV_PREFIX}{}", e.path.join("_").to_uppercase());
            let _ = write!(out, "\n  - {var}: {e}");
        }
    }
    out.push_str("\n\nSee .env.example for every supported variable and its default.");
    out
}

// `Jail::expect_with` takes a closure returning `Result<(), figment::Error>`,
// and figment::Error is 208 bytes. That is figment's signature, not ours: there
// is nothing to box.
#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;

    #[test]
    fn defaults_are_usable_with_no_env_and_no_file() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            let config = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap();
            assert_eq!(config.bind_addr, default_bind_addr());
            assert_eq!(config.env, AppEnv::Dev);
            assert_eq!(config.log_format(), LogFormat::Pretty);
            assert!(config.master_key.is_none());
            Ok(())
        });
    }

    #[test]
    fn env_vars_override_defaults() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("ATLAS_BIND_ADDR", "0.0.0.0:9999");
            jail.set_env("ATLAS_READER_POOL_SIZE", "3");
            jail.set_env("ATLAS_LOG_LEVEL", "warn,atlas=trace");
            let config = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap();
            assert_eq!(config.bind_addr, "0.0.0.0:9999".parse().unwrap());
            assert_eq!(config.reader_pool_size, 3);
            // A comma-separated filter must survive as a string, not be parsed as a list.
            assert_eq!(config.log_level, "warn,atlas=trace");
            Ok(())
        });
    }

    #[test]
    fn env_overrides_the_toml_file() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.create_file(
                "atlas.toml",
                "bind_addr = \"0.0.0.0:1111\"\nreader_pool_size = 2\n",
            )?;
            jail.set_env("ATLAS_BIND_ADDR", "0.0.0.0:2222");
            let config = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap();
            assert_eq!(config.bind_addr, "0.0.0.0:2222".parse().unwrap());
            assert_eq!(config.reader_pool_size, 2);
            Ok(())
        });
    }

    #[test]
    fn prod_without_a_master_key_fails_and_names_the_variable() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("ATLAS_ENV", "prod");
            let err = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap_err();
            assert!(err.to_string().contains("ATLAS_MASTER_KEY"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn prod_with_a_master_key_is_accepted_and_defaults_to_json_logs() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("ATLAS_ENV", "prod");
            jail.set_env(
                "ATLAS_MASTER_KEY",
                "c2VjcmV0LW5vdC1yZWFsbHktMzItYnl0ZXMtbG9uZw==",
            );
            let config = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap();
            assert_eq!(config.log_format(), LogFormat::Json);
            assert!(config.master_key.is_some());
            Ok(())
        });
    }

    #[test]
    fn a_bad_value_names_the_environment_variable_to_fix() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("ATLAS_BIND_ADDR", "not-a-socket-address");
            let err = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap_err();
            assert!(err.to_string().contains("ATLAS_BIND_ADDR"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn zero_reader_pool_is_rejected() {
        Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("ATLAS_READER_POOL_SIZE", "0");
            let err = Config::from_figment(&Config::figment(Path::new("atlas.toml"))).unwrap_err();
            assert!(err.to_string().contains("ATLAS_READER_POOL_SIZE"), "{err}");
            Ok(())
        });
    }

    #[test]
    fn cors_origins_are_split_and_trimmed() {
        let config = Config {
            cors_allowed_origins: "http://a.test , http://b.test,".to_owned(),
            ..Config::default()
        };
        assert_eq!(config.cors_origins(), ["http://a.test", "http://b.test"]);
        assert!(!config.cors_allows_any_origin());

        let any = Config {
            cors_allowed_origins: "*".to_owned(),
            ..Config::default()
        };
        assert!(any.cors_allows_any_origin());
    }

    #[test]
    fn secrets_are_redacted_in_debug_and_display() {
        let secret = SecretString::new("hunter2");
        assert_eq!(format!("{secret:?}"), "SecretString([REDACTED])");
        assert_eq!(format!("{secret}"), "[REDACTED]");
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert_eq!(secret.expose_secret(), "hunter2");
    }

    #[test]
    fn config_debug_never_contains_the_master_key() {
        let config = Config {
            master_key: Some(SecretString::new("super-secret-key")),
            ..Config::default()
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("super-secret-key"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");
    }
}

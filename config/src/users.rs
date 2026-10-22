use anyhow::Context;

/// Resolve the local user name from the environment.
///
/// Used for mux client identity and for the common name on locally
/// generated certificates. Not ssh specific.
pub fn username_from_env() -> anyhow::Result<String> {
    #[cfg(unix)]
    const USER: &str = "USER";
    #[cfg(windows)]
    const USER: &str = "USERNAME";

    std::env::var(USER).with_context(|| format!("while resolving {} env var", USER))
}

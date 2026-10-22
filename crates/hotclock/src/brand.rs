//! The product's display name lives here, in one constant (architecture 0). A rename is a
//! search-and-replace of the three spellings `hotclock`, `HOTCLOCK` and `Hotclock`.

use common::config::GlobalConfig;

pub const DISPLAY_NAME: &str = "Hotclock";

/// The version string the API reports: the crate version plus build metadata. `HOTCLOCK_BUILD` is set by the
/// packaging script to the package revision and the short commit (`1.g9641eb2`); a plain `cargo build` says
/// `dev`.
pub fn version() -> String {
    format!(
        "{}+build.{}",
        env!("CARGO_PKG_VERSION"),
        option_env!("HOTCLOCK_BUILD").unwrap_or("dev")
    )
}

/// The name to show: `brand.display_name` from the global config when set, else the constant.
pub fn display_name(cfg: &GlobalConfig) -> &str {
    cfg.brand.display_name.as_deref().unwrap_or(DISPLAY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_overrides_constant() {
        let mut c = GlobalConfig::default();
        assert_eq!(display_name(&c), "Hotclock");
        c.brand.display_name = Some("Studio Radio".into());
        assert_eq!(display_name(&c), "Studio Radio");
    }
}

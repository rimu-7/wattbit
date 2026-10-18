use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use wattbit::config::{Config, ConfigError, ConfigOption, ConfigStore, CONFIG_VERSION};
use wattbit::policy::Thresholds;

/// A scratch directory under the system temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("wattbit-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_file_loads_as_none() {
    let dir = TempDir::new("missing");
    let store = ConfigStore::new(dir.0.join("wattbit.json"));
    assert!(store.load().unwrap().is_none());
}

#[test]
fn save_then_load_round_trips() {
    let dir = TempDir::new("roundtrip");
    let path = dir.0.join("wattbit.json");
    let store = ConfigStore::new(&path);

    let mut config = Config {
        charge_limit: Some(Thresholds::new(85, Some(70)).unwrap()),
        ..Config::default()
    };
    config.set_option(ConfigOption::MagsafeLed, true);
    store.save(&config).unwrap();

    assert_eq!(store.load().unwrap(), Some(config.clone()));
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644);

    // Overwriting leaves no temporary files behind.
    config.adapter_enabled = false;
    store.save(&config).unwrap();
    assert_eq!(store.load().unwrap(), Some(config));
    let entries: Vec<_> = fs::read_dir(&dir.0).unwrap().collect();
    assert_eq!(entries.len(), 1);
}

#[test]
fn defaults_fill_missing_fields() {
    let dir = TempDir::new("defaults");
    let path = dir.0.join("wattbit.json");
    fs::write(
        &path,
        r#"{"version":1,"charge_limit":{"upper":80,"lower":75}}"#,
    )
    .unwrap();
    let config = ConfigStore::new(&path).load().unwrap().unwrap();
    assert_eq!(config.charge_limit, Some(Thresholds::default()));
    assert!(config.adapter_enabled);
    assert!(config.allow_non_root_access);
    assert!(!config.adapter_fallback);
}

#[test]
fn invalid_thresholds_are_rejected() {
    let dir = TempDir::new("invalid");
    let path = dir.0.join("wattbit.json");
    for bad in [
        r#"{"charge_limit":{"upper":40,"lower":35}}"#,
        r#"{"charge_limit":{"upper":80,"lower":80}}"#,
        r#"{"charge_limit":{"upper":80}}"#,
    ] {
        fs::write(&path, bad).unwrap();
        assert!(
            matches!(
                ConfigStore::new(&path).load(),
                Err(ConfigError::Parse { .. })
            ),
            "{bad} should be rejected"
        );
    }
}

#[test]
fn malformed_and_future_files_are_rejected() {
    let dir = TempDir::new("malformed");
    let path = dir.0.join("wattbit.json");
    fs::write(&path, "{not json").unwrap();
    assert!(matches!(
        ConfigStore::new(&path).load(),
        Err(ConfigError::Parse { .. })
    ));

    fs::write(&path, format!(r#"{{"version":{}}}"#, CONFIG_VERSION + 1)).unwrap();
    assert!(matches!(
        ConfigStore::new(&path).load(),
        Err(ConfigError::Invalid { .. })
    ));
}

#[test]
fn oversized_file_is_rejected() {
    let dir = TempDir::new("oversized");
    let path = dir.0.join("wattbit.json");
    fs::write(&path, vec![b' '; 70 * 1024]).unwrap();
    assert!(matches!(
        ConfigStore::new(&path).load(),
        Err(ConfigError::Invalid { .. })
    ));
}

#[test]
fn every_option_can_be_toggled() {
    use clap::ValueEnum;
    let mut config = Config::default();
    for option in ConfigOption::value_variants() {
        let before = config.option(*option);
        config.set_option(*option, !before);
        assert_eq!(config.option(*option), !before, "{option:?}");
    }
}

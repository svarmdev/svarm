use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use svarm_agent::AgentKind;

use crate::{
    app::{Checkout, SIDEBAR_DEFAULT_WIDTH},
    theme::ThemeName,
};

pub const DEFAULT_HARNESS_UPDATE_INTERVAL_MINUTES: u64 = 60;
pub const HARNESS_UPDATE_INTERVALS_MINUTES: [u64; 7] = [15, 30, 60, 180, 360, 720, 1_440];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeName,
    pub workspaces: Vec<PathBuf>,
    pub last_agent: Option<AgentKind>,
    pub last_checkout: Checkout,
    pub sidebar_width: u16,
    pub harness_update_interval_minutes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeName::default(),
            workspaces: Vec::new(),
            last_agent: None,
            last_checkout: Checkout::default(),
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            harness_update_interval_minutes: DEFAULT_HARNESS_UPDATE_INTERVAL_MINUTES,
        }
    }
}

pub(crate) struct SettingsStore {
    path: Option<PathBuf>,
}

impl SettingsStore {
    pub fn discover() -> Self {
        Self {
            path: settings_path(),
        }
    }

    pub fn load(&self) -> (Settings, Option<String>) {
        let Some(path) = self.path.as_deref() else {
            return (Settings::default(), None);
        };
        match Settings::load(path) {
            Ok(settings) => (settings, None),
            Err(error) => (
                Settings::default(),
                Some(format!("could not load {}: {error}", path.display())),
            ),
        }
    }

    pub fn save(&self, settings: &Settings) -> Result<(), String> {
        let Some(path) = self.path.as_deref() else {
            return Err("could not save settings: HOME is not set".into());
        };
        settings
            .save(path)
            .map_err(|error| format!("could not save {}: {error}", path.display()))
    }
}

impl Settings {
    pub fn effective_harness_update_interval_minutes(&self) -> u64 {
        self.harness_update_interval_minutes.clamp(1, 525_600)
    }

    pub fn record_successful_launch(
        &mut self,
        workspace: PathBuf,
        kind: AgentKind,
        checkout: Checkout,
    ) {
        self.workspaces.retain(|saved| saved != &workspace);
        self.workspaces.insert(0, workspace);
        self.last_agent = Some(kind);
        self.last_checkout = checkout;
    }

    pub fn cycle_harness_update_interval(&mut self, delta: isize) {
        let current = HARNESS_UPDATE_INTERVALS_MINUTES
            .iter()
            .position(|minutes| *minutes == self.harness_update_interval_minutes)
            .unwrap_or_else(|| {
                HARNESS_UPDATE_INTERVALS_MINUTES
                    .iter()
                    .position(|minutes| *minutes == DEFAULT_HARNESS_UPDATE_INTERVAL_MINUTES)
                    .unwrap_or(0)
            });
        let next = (current as isize + delta)
            .rem_euclid(HARNESS_UPDATE_INTERVALS_MINUTES.len() as isize)
            as usize;
        self.harness_update_interval_minutes = HARNESS_UPDATE_INTERVALS_MINUTES[next];
    }

    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        match fs::read(path) {
            Ok(contents) => Ok(serde_json::from_slice(&contents)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

fn settings_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|config| config.join("svarm/settings.json"))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn missing_settings_default_then_round_trip_as_json() {
        let path = env::temp_dir()
            .join(format!(
                "svarm-settings-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("settings.json");
        let settings = Settings {
            theme: ThemeName::TokyoNight,
            workspaces: vec![PathBuf::from("/tmp/one"), PathBuf::from("/tmp/two")],
            last_agent: Some(AgentKind::Claude),
            last_checkout: Checkout::NewWorktree,
            sidebar_width: 16,
            harness_update_interval_minutes: 180,
        };

        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(fs::read_to_string(&path).unwrap().contains("tokyo-night"));

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn theme_only_files_remain_compatible() {
        let settings: Settings = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();

        assert_eq!(settings.theme, ThemeName::Light);
        assert!(settings.workspaces.is_empty());
        assert_eq!(settings.last_agent, None);
        assert_eq!(settings.last_checkout, Checkout::Local);
        assert_eq!(settings.sidebar_width, SIDEBAR_DEFAULT_WIDTH);
        assert_eq!(
            settings.harness_update_interval_minutes,
            DEFAULT_HARNESS_UPDATE_INTERVAL_MINUTES
        );
    }

    #[test]
    fn successful_launches_promote_exact_paths_without_capping_history() {
        let mut settings = Settings {
            theme: ThemeName::Nord,
            workspaces: (0..20)
                .map(|index| PathBuf::from(format!("/tmp/workspace-{index}")))
                .collect(),
            last_agent: Some(AgentKind::Claude),
            last_checkout: Checkout::Local,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            harness_update_interval_minutes: DEFAULT_HARNESS_UPDATE_INTERVAL_MINUTES,
        };

        settings.record_successful_launch(
            PathBuf::from("/tmp/workspace-7"),
            AgentKind::Codex,
            Checkout::NewWorktree,
        );

        assert_eq!(settings.theme, ThemeName::Nord);
        assert_eq!(settings.workspaces.len(), 20);
        assert_eq!(settings.workspaces[0], PathBuf::from("/tmp/workspace-7"));
        assert_eq!(settings.last_agent, Some(AgentKind::Codex));
        assert_eq!(settings.last_checkout, Checkout::NewWorktree);
    }

    #[test]
    fn harness_update_interval_cycles_through_supported_presets() {
        let mut settings = Settings::default();

        settings.cycle_harness_update_interval(1);
        assert_eq!(settings.harness_update_interval_minutes, 180);
        settings.cycle_harness_update_interval(-1);
        assert_eq!(settings.harness_update_interval_minutes, 60);

        settings.harness_update_interval_minutes = u64::MAX;
        assert_eq!(
            settings.effective_harness_update_interval_minutes(),
            525_600
        );
    }
}

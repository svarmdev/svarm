use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::theme::ThemeName;

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeName,
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

    pub fn load(&self) -> (ThemeName, Option<String>) {
        let Some(path) = self.path.as_deref() else {
            return (ThemeName::default(), None);
        };
        match Settings::load(path) {
            Ok(settings) => (settings.theme, None),
            Err(error) => (
                ThemeName::default(),
                Some(format!("could not load {}: {error}", path.display())),
            ),
        }
    }

    pub fn save_theme(&self, theme: ThemeName) -> Result<(), String> {
        let Some(path) = self.path.as_deref() else {
            return Err("could not save settings: HOME is not set".into());
        };
        (Settings { theme })
            .save(path)
            .map_err(|error| format!("could not save {}: {error}", path.display()))
    }
}

impl Settings {
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
        };

        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), settings);
        assert!(fs::read_to_string(&path).unwrap().contains("tokyo-night"));

        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

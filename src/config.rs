use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone, serde_derive::Deserialize)]
pub struct Config {
    pub project: Vec<Project>,
    #[serde(default = "default_debounce")]
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub debounce: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            project: Default::default(),
            debounce: default_debounce(),
        }
    }
}

#[derive(Default, Debug, Clone, serde_derive::Deserialize)]
pub struct Project {
    pub sync: Vec<FileSync>,
    /// commands to run after sync any sync
    #[serde(default)]
    pub on_sync: Vec<String>,
}

fn default_debounce() -> Duration {
    Duration::from_millis(100)
}

#[derive(Default, Debug, Clone, serde_derive::Deserialize)]
pub struct FileSync {
    pub src: PathBuf,
    /// Watch src recursively. If src is a file then this flag is ignored
    /// default=true
    #[serde(default = "default_recursive")]
    pub recursive: bool,
    pub dst: PathBuf,
    pub rsync_flags: Option<String>,
    /// commands to run after sync
    #[serde(default)]
    pub on_sync: Vec<String>,
    /// commands to run after the first sync
    #[serde(default)]
    pub on_init: Vec<String>,
}

fn default_recursive() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixed_watch() {
        let toml = r#"
debounce = "1s 30ms"
[[project]]
[[project.sync]]
src = "asd"
dst = "remote:~/asd"
"#;

        let config: Config = toml::de::from_str(toml).unwrap();

        assert_eq!(config.project[0].sync[0].src.as_os_str(), "asd");
        assert_eq!(config.project[0].sync[0].dst.as_os_str(), "remote:~/asd");
        assert_eq!(config.debounce, Duration::from_millis(1030));
    }
}

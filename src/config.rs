use std::collections::HashMap;
use std::{path::PathBuf, time::Duration};

pub type ProjectName = String;

#[derive(Debug, Clone, serde_derive::Deserialize)]
pub struct Config {
    pub projects: HashMap<ProjectName, Project>,
    #[serde(default = "default_debounce")]
    #[serde(deserialize_with = "duration_str::deserialize_duration")]
    pub debounce: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            projects: Default::default(),
            debounce: default_debounce(),
        }
    }
}

#[derive(Default, Debug, Clone, serde_derive::Deserialize)]
pub struct Project {
    pub sync: Vec<FileSync>,
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
        let yaml = r#"
debounce: 1s 30ms
projects:
    asd:
      sync:
          - src: asd
            dst: remote:~/asd
"#;

        let config: Config = serde_yaml::from_str(yaml).unwrap();

        assert_eq!(config.projects.len(), 1);

        assert_eq!(config.projects["asd"].sync[0].src.as_os_str(), "asd");
        assert_eq!(
            config.projects["asd"].sync[0].dst.as_os_str(),
            "remote:~/asd"
        );
        assert_eq!(config.debounce, Duration::from_millis(1030));
    }
}

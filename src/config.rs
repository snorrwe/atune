use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;
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
    /// cancel in-progress on_sync commands if a new change happens while they're running
    #[serde(default = "default_true")]
    pub restart: bool,
}

fn default_debounce() -> Duration {
    Duration::from_millis(100)
}

#[derive(Default, Debug, Clone, serde_derive::Deserialize)]
pub struct FileSync {
    pub src: PathBuf,
    /// Watch src recursively. If src is a file then this flag is ignored
    /// default=true
    #[serde(default = "default_true")]
    pub recursive: bool,
    pub dst: PathBuf,
    pub rsync_flags: Option<String>,
    /// commands to run after sync
    #[serde(default)]
    #[serde(deserialize_with = "deser_command_list")]
    pub on_sync: Vec<CommandConfig>,
    /// commands to run after the first sync
    #[serde(default)]
    #[serde(deserialize_with = "deser_command_list")]
    pub on_init: Vec<CommandConfig>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct CommandConfig {
    pub command: String,
}

impl FromStr for CommandConfig {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(CommandConfig {
            command: s.to_owned(),
        })
    }
}

fn default_true() -> bool {
    true
}

struct CommandConfigDe(pub CommandConfig);

impl<'de> Deserialize<'de> for CommandConfigDe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CommandVisitor;
        impl<'de> Visitor<'de> for CommandVisitor {
            type Value = CommandConfigDe;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("string or map")
            }

            fn visit_str<E>(self, value: &str) -> Result<CommandConfigDe, E>
            where
                E: de::Error,
            {
                Ok(CommandConfigDe(FromStr::from_str(value).unwrap()))
            }

            fn visit_map<M>(self, map: M) -> Result<CommandConfigDe, M::Error>
            where
                M: MapAccess<'de>,
            {
                Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(CommandConfigDe)
            }
        }

        deserializer.deserialize_any(CommandVisitor)
    }
}

fn deser_command_list<'de, D>(deserializer: D) -> Result<Vec<CommandConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    struct V;
    impl<'de> Visitor<'de> for V {
        type Value = Vec<CommandConfig>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("List of CommandConfigs")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: de::SeqAccess<'de>,
        {
            let mut v = Vec::with_capacity(seq.size_hint().unwrap_or(1));
            while let Some(CommandConfigDe(c)) = seq.next_element()? {
                v.push(c);
            }
            Ok(v)
        }
    }

    deserializer.deserialize_any(V)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_yaml_deser() {
        let yaml = r#"
debounce: 1s 30ms
projects:
    asd:
      sync:
          - src: asd
            dst: remote:~/asd
            on_sync:
                - echo done
                - command: echo hi
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

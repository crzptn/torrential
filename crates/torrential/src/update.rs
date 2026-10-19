use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Say when a newer version is out
    #[default]
    Notify,
    /// Install newer versions in the background
    Auto,
    /// Never look for newer versions
    Off,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notify => "notify",
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }
}

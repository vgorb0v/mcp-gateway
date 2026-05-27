use std::collections::BTreeMap;

use super::{ClientsConfig, Config, GroupConfig, LimitsConfig};
use crate::Result;

impl Config {
    pub fn native_empty_core() -> Self {
        Self {
            limits: LimitsConfig {
                max_clients: 128,
                session_ttl_seconds: 300,
                ..LimitsConfig::default()
            },
            groups: BTreeMap::from([("coding".to_string(), GroupConfig::default())]),
            clients: ClientsConfig::default(),
            ..Config::default()
        }
    }
}

pub fn render_native_empty_core_yaml() -> Result<String> {
    Ok(serde_yaml::to_string(&Config::native_empty_core())?)
}

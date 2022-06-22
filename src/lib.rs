use serde::Deserialize;
use serde_yaml;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub mod core;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default = "Config::default_incoming_bind_host")]
    pub incoming_bind_host: String,
    #[serde(default = "Config::default_incoming_bind_port")]
    pub incoming_bind_port: u32,
    #[serde(default = "Config::default_executor_bind_host")]
    pub executor_bind_host: String,
    #[serde(default = "Config::default_executor_bind_port")]
    pub executor_bind_port: u32,
}
impl Config {
    fn default_incoming_bind_host() -> String {
        "0.0.0.0".to_string()
    }
    fn default_incoming_bind_port() -> u32 {
        5632
    }
    fn default_executor_bind_host() -> String {
        "0.0.0.0".to_string()
    }
    fn default_executor_bind_port() -> u32 {
        5633
    }
}
impl Default for Config {
    fn default() -> Self {
        Self {
            incoming_bind_host: Config::default_incoming_bind_host(),
            incoming_bind_port: Config::default_incoming_bind_port(),
            executor_bind_host: Config::default_executor_bind_host(),
            executor_bind_port: Config::default_executor_bind_port(),
        }
    }
}

pub fn get_config(path: &str) -> anyhow::Result<Config> {
    let p = Path::new(path);
    if p.exists() {
        let mut f = File::open(p)?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        let c = serde_yaml::from_str(&s)?;
        return Ok(c);
    }
    Ok(Config::default())
}

use serde::Deserialize;
use std::fs;

use crate::error::Error;

#[derive(Debug, Clone)]
pub enum VtSelection {
    None,
    Current,
    Next,
    Specific(u32),
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    terminal: TerminalSection,
    general: Option<GeneralSection>,
    default_session: SessionSection,
    initial_session: Option<SessionSection>,
}

#[derive(Debug, Deserialize)]
struct TerminalSection {
    vt: toml::Value,
    #[serde(default = "default_true")]
    switch: bool,
}

#[derive(Debug, Deserialize)]
struct GeneralSection {
    #[serde(default = "default_true")]
    source_profile: bool,
    #[serde(default = "default_runfile")]
    runfile: String,
    #[serde(default = "default_service")]
    service: String,
}

#[derive(Debug, Deserialize)]
struct SessionSection {
    command: String,
    #[serde(default = "default_greeter_user")]
    user: String,
    service: Option<String>,
}

fn default_true() -> bool { true }
fn default_runfile() -> String { "/run/greetd.run".into() }
fn default_service() -> String { "greetd".into() }
fn default_greeter_user() -> String { "greeter".into() }

#[derive(Debug)]
pub struct Config {
    pub vt: VtSelection,
    pub switch: bool,
    pub source_profile: bool,
    pub runfile: String,
    pub service: String,
    pub greeter_command: String,
    pub greeter_user: String,
    pub greeter_service: String,
    pub initial_session: Option<InitialSession>,
}

#[derive(Debug)]
pub struct InitialSession {
    pub command: String,
    pub user: String,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Error> {
        let content = fs::read_to_string(path)
            .map_err(|e| Error::Config(format!("failed to read {path}: {e}")))?;
        Self::parse(&content)
    }

    pub fn parse(content: &str) -> Result<Self, Error> {
        let file: ConfigFile = toml::from_str(content)
            .map_err(|e| Error::Config(format!("failed to parse config: {e}")))?;

        let vt = parse_vt(&file.terminal.vt)?;

        let general = file.general.unwrap_or(GeneralSection {
            source_profile: true,
            runfile: default_runfile(),
            service: default_service(),
        });

        let greeter_service = file.default_session.service
            .unwrap_or_else(|| "greetd-greeter".into());

        let initial_session = file.initial_session.map(|s| InitialSession {
            command: s.command,
            user: s.user,
        });

        Ok(Config {
            vt,
            switch: file.terminal.switch,
            source_profile: general.source_profile,
            runfile: general.runfile,
            service: general.service,
            greeter_command: file.default_session.command,
            greeter_user: file.default_session.user,
            greeter_service,
            initial_session,
        })
    }
}

fn parse_vt(value: &toml::Value) -> Result<VtSelection, Error> {
    match value {
        toml::Value::String(s) => match s.as_str() {
            "none" => Ok(VtSelection::None),
            "current" => Ok(VtSelection::Current),
            "next" => Ok(VtSelection::Next),
            _ => Err(Error::Config(format!("invalid vt value: {s}"))),
        },
        toml::Value::Integer(n) => Ok(VtSelection::Specific(*n as u32)),
        _ => Err(Error::Config("vt must be string or integer".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config() {
        let config = Config::parse(r#"
[terminal]
vt = 1

[default_session]
command = "agreety"
"#).unwrap();

        assert!(matches!(config.vt, VtSelection::Specific(1)));
        assert!(config.switch);
        assert_eq!(config.greeter_command, "agreety");
        assert_eq!(config.greeter_user, "greeter");
        assert_eq!(config.greeter_service, "greetd-greeter");
        assert!(config.source_profile);
        assert_eq!(config.runfile, "/run/greetd.run");
        assert_eq!(config.service, "greetd");
        assert!(config.initial_session.is_none());
    }

    #[test]
    fn vt_selection_none() {
        let config = Config::parse(r#"
[terminal]
vt = "none"

[default_session]
command = "test"
"#).unwrap();

        assert!(matches!(config.vt, VtSelection::None));
    }

    #[test]
    fn vt_selection_current() {
        let config = Config::parse(r#"
[terminal]
vt = "current"

[default_session]
command = "test"
"#).unwrap();

        assert!(matches!(config.vt, VtSelection::Current));
    }

    #[test]
    fn vt_selection_next() {
        let config = Config::parse(r#"
[terminal]
vt = "next"

[default_session]
command = "test"
"#).unwrap();

        assert!(matches!(config.vt, VtSelection::Next));
    }

    #[test]
    fn vt_selection_specific() {
        let config = Config::parse(r#"
[terminal]
vt = 7

[default_session]
command = "test"
"#).unwrap();

        assert!(matches!(config.vt, VtSelection::Specific(7)));
    }

    #[test]
    fn invalid_vt_string() {
        let result = Config::parse(r#"
[terminal]
vt = "invalid"

[default_session]
command = "test"
"#);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid vt value"));
    }

    #[test]
    fn switch_disabled() {
        let config = Config::parse(r#"
[terminal]
vt = 1
switch = false

[default_session]
command = "test"
"#).unwrap();

        assert!(!config.switch);
    }

    #[test]
    fn custom_general_section() {
        let config = Config::parse(r#"
[terminal]
vt = 1

[general]
source_profile = false
runfile = "/custom/path"
service = "custom-pam"

[default_session]
command = "test"
"#).unwrap();

        assert!(!config.source_profile);
        assert_eq!(config.runfile, "/custom/path");
        assert_eq!(config.service, "custom-pam");
    }

    #[test]
    fn custom_session_user_and_service() {
        let config = Config::parse(r#"
[terminal]
vt = 1

[default_session]
command = "my-greeter"
user = "custom-user"
service = "custom-greeter-pam"
"#).unwrap();

        assert_eq!(config.greeter_command, "my-greeter");
        assert_eq!(config.greeter_user, "custom-user");
        assert_eq!(config.greeter_service, "custom-greeter-pam");
    }

    #[test]
    fn initial_session() {
        let config = Config::parse(r#"
[terminal]
vt = 1

[default_session]
command = "greeter"

[initial_session]
command = "sway"
user = "john"
"#).unwrap();

        let initial = config.initial_session.unwrap();
        assert_eq!(initial.command, "sway");
        assert_eq!(initial.user, "john");
    }

    #[test]
    fn missing_terminal_section() {
        let result = Config::parse(r#"
[default_session]
command = "test"
"#);

        assert!(result.is_err());
    }

    #[test]
    fn missing_default_session() {
        let result = Config::parse(r#"
[terminal]
vt = 1
"#);

        assert!(result.is_err());
    }

    #[test]
    fn missing_command() {
        let result = Config::parse(r#"
[terminal]
vt = 1

[default_session]
user = "greeter"
"#);

        assert!(result.is_err());
    }
}

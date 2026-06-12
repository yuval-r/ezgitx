use std::collections::BTreeMap;

use serde::Deserialize;

pub const CONFIG_FILE: &str = ".ezgitx.yml";

/// Raw shape of `.ezgitx.yml` (PRD §4.1). Unknown keys are rejected so schema
/// drift fails loudly with `config_invalid`.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub version: u32,
    pub groups: BTreeMap<String, Vec<RepoEntry>>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    pub path: String,
    pub default_cmd: Option<String>,
    pub check_cmd: Option<String>,
    pub depends_on: Option<Vec<String>>,
}

pub fn parse(text: &str) -> Result<ConfigFile, String> {
    let cfg: ConfigFile = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    if cfg.version != 1 {
        return Err(format!("unsupported config version {}", cfg.version));
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let cfg = parse(
            "version: 1\n\
             groups:\n\
             \x20 core:\n\
             \x20   - path: ./a\n\
             \x20     default_cmd: \"make\"\n\
             \x20     check_cmd: \"make test\"\n\
             \x20     depends_on: [\"b\"]\n\
             \x20   - path: ./b\n",
        )
        .unwrap();
        assert_eq!(cfg.groups["core"].len(), 2);
        assert_eq!(
            cfg.groups["core"][0].depends_on.as_deref(),
            Some(&["b".to_string()][..])
        );
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = parse("version: 1\ngroups:\n  g:\n    - path: ./a\n      bogus: true\n");
        assert!(err.is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let err = parse("version: 9\ngroups: {}\n");
        assert!(err.is_err());
    }
}

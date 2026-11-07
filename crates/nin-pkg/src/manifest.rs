//! `nin-package.toml` manifest.

use crate::error::{PkgError, Result};
use semver::{Version, VersionReq};
use std::collections::BTreeMap;

/// Parsed and validated package manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: Version,
    pub description: String,
    pub authors: Vec<String>,
    pub license: Option<String>,
    pub entry: Option<String>,
    pub dependencies: BTreeMap<String, VersionReq>,
}

/// `[a-z0-9][a-z0-9_-]{0,63}`
pub fn is_valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 64 {
        return false;
    }
    let first = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    first(b[0]) && b[1..].iter().all(|&c| first(c) || c == b'_' || c == b'-')
}

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(PkgError::Manifest(m.into()))
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest> {
        let doc: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| PkgError::Manifest(format!("TOML syntax error: {e}")))?;
        let pkg = match doc.get("package") {
            Some(toml::Value::Table(t)) => t,
            Some(_) => return err("`package` must be a table"),
            None => return err("missing [package] section"),
        };
        let get_str = |key: &str| -> Result<Option<String>> {
            match pkg.get(key) {
                None => Ok(None),
                Some(toml::Value::String(s)) => Ok(Some(s.clone())),
                Some(_) => err(format!("`package.{key}` must be a string")),
            }
        };
        let name = match get_str("name")? {
            Some(n) => n,
            None => return err("missing required field `package.name`"),
        };
        if !is_valid_name(&name) {
            return err(format!(
                "invalid package name `{name}`: must match [a-z0-9][a-z0-9_-]{{0,63}}"
            ));
        }
        let vs = match get_str("version")? {
            Some(v) => v,
            None => return err("missing required field `package.version`"),
        };
        let version = Version::parse(&vs)
            .map_err(|e| PkgError::Manifest(format!("invalid version `{vs}`: {e}")))?;
        let description = get_str("description")?.unwrap_or_default();
        let license = get_str("license")?;
        let entry = get_str("entry")?;
        if let Some(e) = &entry {
            if let Err(why) = crate::archive::validate_path(e) {
                return err(format!("invalid entry `{e}`: {why}"));
            }
        }
        let authors = match pkg.get("authors") {
            None => Vec::new(),
            Some(toml::Value::Array(a)) => {
                let mut out = Vec::new();
                for v in a {
                    match v {
                        toml::Value::String(s) => out.push(s.clone()),
                        _ => return err("`package.authors` must be an array of strings"),
                    }
                }
                out
            }
            Some(_) => return err("`package.authors` must be an array of strings"),
        };
        let mut dependencies = BTreeMap::new();
        match doc.get("dependencies") {
            None => {}
            Some(toml::Value::Table(t)) => {
                for (dn, dv) in t {
                    if !is_valid_name(dn) {
                        return err(format!("invalid dependency name `{dn}`"));
                    }
                    if *dn == name {
                        return err(format!("package `{name}` depends on itself"));
                    }
                    let rs = match dv {
                        toml::Value::String(s) => s,
                        _ => return err(format!("dependency `{dn}` must be a version requirement string")),
                    };
                    let req = VersionReq::parse(rs).map_err(|e| {
                        PkgError::Manifest(format!("invalid requirement `{rs}` for dependency `{dn}`: {e}"))
                    })?;
                    dependencies.insert(dn.clone(), req);
                }
            }
            Some(_) => return err("`dependencies` must be a table"),
        }
        Ok(Manifest { name, version, description, authors, license, entry, dependencies })
    }

    pub fn to_toml(&self) -> String {
        let mut pkg = toml::Table::new();
        pkg.insert("name".into(), self.name.clone().into());
        pkg.insert("version".into(), self.version.to_string().into());
        pkg.insert("description".into(), self.description.clone().into());
        pkg.insert(
            "authors".into(),
            toml::Value::Array(self.authors.iter().cloned().map(toml::Value::String).collect()),
        );
        if let Some(l) = &self.license {
            pkg.insert("license".into(), l.clone().into());
        }
        if let Some(e) = &self.entry {
            pkg.insert("entry".into(), e.clone().into());
        }
        let mut deps = toml::Table::new();
        for (k, v) in &self.dependencies {
            deps.insert(k.clone(), v.to_string().into());
        }
        let mut doc = toml::Table::new();
        doc.insert("package".into(), toml::Value::Table(pkg));
        doc.insert("dependencies".into(), toml::Value::Table(deps));
        toml::to_string(&doc).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
[package]
name = "my-lib"
version = "1.2.3"
description = "d"
authors = ["a", "b"]
license = "MIT"
entry = "main.nin"
[dependencies]
other-lib = "^1.0"
"#;

    #[test]
    fn parses_and_roundtrips() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!(m.name, "my-lib");
        assert_eq!(m.version, Version::new(1, 2, 3));
        assert_eq!(m.authors, vec!["a", "b"]);
        assert!(m.dependencies["other-lib"].matches(&Version::new(1, 5, 0)));
        assert_eq!(Manifest::parse(&m.to_toml()).unwrap(), m);
    }

    #[test]
    fn minimal() {
        let m = Manifest::parse("[package]\nname=\"x\"\nversion=\"0.1.0\"").unwrap();
        assert!(m.entry.is_none() && m.dependencies.is_empty() && m.description.is_empty());
    }

    #[test]
    fn errors() {
        let bad = [
            "",
            "[package]\nversion=\"1.0.0\"",
            "[package]\nname=\"x\"",
            "[package]\nname=\"X\"\nversion=\"1.0.0\"",
            "[package]\nname=\"-x\"\nversion=\"1.0.0\"",
            "[package]\nname=\"x\"\nversion=\"1.0\"",
            "[package]\nname=\"x\"\nversion=1",
            "[package\nname",
            "[package]\nname=\"x\"\nversion=\"1.0.0\"\nauthors=\"me\"",
            "[package]\nname=\"x\"\nversion=\"1.0.0\"\nentry=\"../evil\"",
            "[package]\nname=\"x\"\nversion=\"1.0.0\"\n[dependencies]\ny=\"nope\"",
            "[package]\nname=\"x\"\nversion=\"1.0.0\"\n[dependencies]\nx=\"1\"",
            "[package]\nname=\"x\"\nversion=\"1.0.0\"\n[dependencies]\ny=3",
        ];
        for b in bad {
            assert!(matches!(Manifest::parse(b), Err(PkgError::Manifest(_))), "should fail: {b}");
        }
        assert!(!is_valid_name(&"a".repeat(65)));
        assert!(is_valid_name(&"a".repeat(64)));
    }
}

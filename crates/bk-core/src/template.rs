//! `{{name}}` / `{{name:default}}` placeholders.
//!
//! Only identifier-shaped names count, so Go/Jinja/docker templates such as
//! `docker ps --format '{{.Names}}'` pass through untouched. `\{{` escapes a literal.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::Serialize;

use crate::model::Bookmark;

static PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\\?)\{\{\s*([A-Za-z_][A-Za-z0-9_]*)\s*(?::([^}]*?))?\s*\}\}").expect("valid regex"));

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Param {
    pub name: String,
    pub default: Option<String>,
}

/// Distinct params in order of first appearance; the first default seen wins.
pub fn params_in<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<Param> {
    let mut out: Vec<Param> = Vec::new();
    for text in texts {
        for c in PLACEHOLDER.captures_iter(text) {
            if !c[1].is_empty() {
                continue; // escaped
            }
            let name = c[2].to_string();
            let default = c.get(3).map(|m| m.as_str().trim().to_string());
            match out.iter_mut().find(|p| p.name == name) {
                Some(p) if p.default.is_none() => p.default = default,
                Some(_) => {}
                None => out.push(Param { name, default }),
            }
        }
    }
    out
}

/// All params used by a bookmark's steps (and step working dirs).
pub fn bookmark_params(b: &Bookmark) -> Vec<Param> {
    let steps = b.effective_steps();
    let texts = steps.iter().flat_map(|s| std::iter::once(s.run.as_str()).chain(s.cwd.as_deref()));
    params_in(texts)
}

/// Params that have neither a provided value nor a default.
pub fn missing(params: &[Param], values: &BTreeMap<String, String>) -> Vec<String> {
    params.iter().filter(|p| p.default.is_none() && !values.contains_key(&p.name)).map(|p| p.name.clone()).collect()
}

/// Substitute values (falling back to defaults). Unknown names without a default are an error.
pub fn render(text: &str, values: &BTreeMap<String, String>) -> Result<String, String> {
    let mut missing = Vec::new();
    let out = PLACEHOLDER.replace_all(text, |c: &Captures| {
        if !c[1].is_empty() {
            // `\{{x}}` → literal `{{x}}`
            return c[0][1..].to_string();
        }
        let name = &c[2];
        if let Some(v) = values.get(name) {
            v.clone()
        } else if let Some(d) = c.get(3) {
            d.as_str().trim().to_string()
        } else {
            missing.push(name.to_string());
            String::new()
        }
    });
    if missing.is_empty() {
        Ok(out.into_owned())
    } else {
        missing.dedup();
        Err(format!("missing value for {}", missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn finds_params_with_defaults() {
        let ps = params_in(["python t.py --epochs {{epochs:10}} --lr {{ lr }} {{epochs}}"]);
        assert_eq!(
            ps,
            vec![
                Param { name: "epochs".into(), default: Some("10".into()) },
                Param { name: "lr".into(), default: None }
            ]
        );
    }

    #[test]
    fn ignores_go_templates_and_escapes() {
        let t = r#"docker ps --format '{{.Names}}' {{ json . }} \{{name}}"#;
        assert!(params_in([t]).is_empty());
        assert_eq!(render(t, &vals(&[])).unwrap(), r#"docker ps --format '{{.Names}}' {{ json . }} {{name}}"#);
    }

    #[test]
    fn renders_values_defaults_and_errors() {
        let t = "run {{a}} {{b:2}} {{c:}}";
        assert_eq!(render(t, &vals(&[("a", "x y")])).unwrap(), "run x y 2 ");
        assert_eq!(render(t, &vals(&[("a", "1"), ("b", "3")])).unwrap(), "run 1 3 ");
        let err = render("{{a}} {{z}}", &vals(&[])).unwrap_err();
        assert!(err.contains("`a`") && err.contains("`z`"), "{err}");
    }

    #[test]
    fn missing_list() {
        let ps = params_in(["{{a}} {{b:1}}"]);
        assert_eq!(missing(&ps, &vals(&[])), vec!["a".to_string()]);
        assert!(missing(&ps, &vals(&[("a", "")])).is_empty());
    }
}

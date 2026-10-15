//! `/output-style`: pick how the agent talks. The choice is saved to the
//! project's `settings.local.json`.

use io_runtime::output_style::{self, OutputStyle};
use io_runtime::settings::{self, Settings, SettingsPaths};
use io_tui::picker;
use std::path::Path;

/// Resolve `arg` (a style name, or empty to pick from a list) and save it.
/// `Ok(None)` if the user backed out.
pub fn run(arg: &str, root: &Path) -> anyhow::Result<Option<String>> {
    let home = dirs::home_dir();
    let styles = output_style::discover(root, home.as_deref());
    let current = Settings::load(root).output_style;
    let chosen = match arg.trim() {
        "" => match pick(&styles, current.as_deref())? {
            Some(name) => name,
            None => return Ok(None),
        },
        name => resolve(&styles, name)?,
    };
    let paths = SettingsPaths::for_project(root);
    settings::set_value(&paths.local, "outputStyle", serde_json::json!(chosen))
        .map_err(anyhow::Error::msg)?;
    Ok(Some(format!("Output style: {chosen}")))
}

fn resolve(styles: &[OutputStyle], name: &str) -> anyhow::Result<String> {
    styles
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
        .map(|s| s.name.clone())
        .ok_or_else(|| {
            let known: Vec<&str> = styles.iter().map(|s| s.name.as_str()).collect();
            anyhow::anyhow!(
                "unknown output style '{name}' (available: {})",
                known.join(", ")
            )
        })
}

fn pick(styles: &[OutputStyle], current: Option<&str>) -> anyhow::Result<Option<String>> {
    let items: Vec<(&str, &str)> = styles
        .iter()
        .map(|s| (s.name.as_str(), s.description.as_str()))
        .collect();
    let at = styles
        .iter()
        .position(|s| Some(s.name.as_str()) == current)
        .or(Some(0));
    println!();
    match picker::pick_with_hint(&items, at) {
        Ok(i) => Ok(Some(styles[i].name.clone())),
        Err(e) if e.is::<picker::Dismissed>() => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_case_insensitively_and_unknown_ones_list_the_options() {
        let styles = output_style::builtin_styles();
        assert_eq!(resolve(&styles, "Concise").unwrap(), "concise");
        let err = resolve(&styles, "nope").unwrap_err().to_string();
        assert!(err.contains("nope") && err.contains("explanatory"), "{err}");
    }
}

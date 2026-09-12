//! Embedded assets: bundled theme families, chrome icons, and the brand mark, with gpui-kit's Lucide set behind them.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../assets/"]
#[include = "themes/*.json"]
#[include = "icons/*.svg"]
#[include = "brand/*.svg"]
struct Embedded;

/// Our embedded assets first, gpui-kit's icon set second.
pub struct AppAssets;

impl AssetSource for AppAssets {
  fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
    if let Some(file) = Embedded::get(path) {
      return Ok(Some(file.data));
    }
    gpui_kit::assets::Assets.load(path)
  }

  fn list(&self, path: &str) -> Result<Vec<SharedString>> {
    let mut names: Vec<SharedString> = Embedded::iter()
      .filter(|name| name.starts_with(path))
      .map(|name| SharedString::from(name.to_string()))
      .collect();
    names.extend(gpui_kit::assets::Assets.list(path)?);
    Ok(names)
  }
}

pub fn theme_files() -> Vec<(String, Cow<'static, [u8]>)> {
  Embedded::iter()
    .filter(|name| name.starts_with("themes/") && name.ends_with(".json"))
    .filter_map(|name| {
      let id = name.trim_start_matches("themes/").trim_end_matches(".json").to_owned();
      let data = Embedded::get(&name)?.data;
      Some((id, data))
    })
    .collect()
}

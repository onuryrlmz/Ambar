//! The native menu. Its labels are read from the frontend's string tables, so there is
//! still only one place for UI text. Every item with an id emits a `menu` event carrying
//! that id; predefined items (cut, paste, quit...) do their own work.

use std::collections::HashMap;
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItem, MenuItemBuilder, SubmenuBuilder,
};
use tauri::{AppHandle, Emitter, Runtime};

const TR: &str = include_str!("../../src/lib/i18n/tr.ts");
const EN: &str = include_str!("../../src/lib/i18n/en.ts");

/// Reads the `"menu.x": "text",` lines of a string table.
fn parse_labels(table: &'static str) -> HashMap<&'static str, &'static str> {
    table
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix('"')?;
            let (key, value) = rest.split_once("\": \"")?;
            let value = value.strip_suffix("\",")?;
            key.starts_with("menu.").then_some((key, value))
        })
        .collect()
}

fn table(lang: &str) -> &'static str {
    if lang == "en" {
        EN
    } else {
        TR
    }
}

fn item<R: Runtime>(
    app: &AppHandle<R>,
    id: &str,
    label: &str,
    accelerator: Option<&str>,
) -> tauri::Result<MenuItem<R>> {
    let mut b = MenuItemBuilder::with_id(id, label);
    if let Some(a) = accelerator {
        b = b.accelerator(a);
    }
    b.build(app)
}

pub fn build<R: Runtime>(app: &AppHandle<R>, lang: &str, show_hidden: bool) -> tauri::Result<Menu<R>> {
    let labels = parse_labels(table(lang));
    // A missing key shows up as the key itself; a test keeps that from happening.
    let l = |key: &'static str| -> &'static str { labels.get(key).copied().unwrap_or(key) };

    let about = item(app, "about", l("menu.about"), None)?;

    let mut file = SubmenuBuilder::new(app, l("menu.file"))
        .item(&item(app, "open", l("menu.open"), Some("CmdOrCtrl+O"))?)
        .separator()
        .item(&item(app, "extract", l("menu.extract"), Some("CmdOrCtrl+E"))?)
        .item(&item(app, "test", l("menu.test"), None)?)
        .item(&item(app, "show", l("menu.show"), None)?)
        .item(&item(app, "info", l("menu.info"), Some("CmdOrCtrl+I"))?)
        .separator()
        // The Mac menu owns Cmd+W. Elsewhere the page handles the key, or it would fire twice.
        .item(&item(
            app,
            "close-tab",
            l("menu.closeTab"),
            cfg!(target_os = "macos").then_some("CmdOrCtrl+W"),
        )?)
        .item(&item(
            app,
            "close-window",
            l("menu.closeWindow"),
            Some("CmdOrCtrl+Shift+W"),
        )?);
    if !cfg!(target_os = "macos") {
        file = file.separator().quit_with_text(l("menu.quit"));
    }
    let file = file.build()?;

    let edit = SubmenuBuilder::new(app, l("menu.edit"))
        .undo_with_text(l("menu.undo"))
        .redo_with_text(l("menu.redo"))
        .separator()
        .cut_with_text(l("menu.cut"))
        .copy_with_text(l("menu.copy"))
        .paste_with_text(l("menu.paste"))
        // Not the predefined item: this selects rows, or the text while typing a path.
        .item(&item(app, "select-all", l("menu.selectAll"), Some("CmdOrCtrl+A"))?)
        .build()?;

    let language = SubmenuBuilder::new(app, l("menu.language"))
        .item(
            &CheckMenuItemBuilder::with_id("lang-tr", l("menu.langTr"))
                .checked(lang != "en")
                .build(app)?,
        )
        .item(
            &CheckMenuItemBuilder::with_id("lang-en", l("menu.langEn"))
                .checked(lang == "en")
                .build(app)?,
        )
        .build()?;

    let mut view = SubmenuBuilder::new(app, l("menu.view"))
        .item(&item(app, "refresh", l("menu.refresh"), Some("CmdOrCtrl+R"))?)
        .item(
            &CheckMenuItemBuilder::with_id("show-hidden", l("menu.showHidden"))
                .checked(show_hidden)
                .build(app)?,
        )
        .separator()
        .item(&language);
    if cfg!(target_os = "macos") {
        view = view
            .separator()
            .minimize_with_text(l("menu.minimize"))
            .fullscreen_with_text(l("menu.fullscreen"));
    }
    let view = view.build()?;

    let help = SubmenuBuilder::new(app, l("menu.help")).item(&about).build()?;

    let mut bar = MenuBuilder::new(app);
    if cfg!(target_os = "macos") {
        // The Mac shows the app's own name on the first menu.
        let app_menu = SubmenuBuilder::new(app, "Ambar")
            .item(&about)
            .separator()
            .services_with_text(l("menu.services"))
            .separator()
            .hide_with_text(l("menu.hide"))
            .hide_others_with_text(l("menu.hideOthers"))
            .show_all_with_text(l("menu.showAll"))
            .separator()
            .quit_with_text(l("menu.quit"))
            .build()?;
        bar = bar.item(&app_menu);
    }
    bar.item(&file).item(&edit).item(&view).item(&help).build()
}

/// Sends every custom menu item to the frontend as a `menu` event.
pub fn forward_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let _ = app.emit("menu", id);
}

#[tauri::command]
pub fn set_menu_language(
    app: AppHandle,
    lang: String,
    show_hidden: bool,
) -> Result<(), crate::error::ArchiveError> {
    build(&app, &lang, show_hidden)
        .and_then(|menu| app.set_menu(menu).map(|_| ()))
        .map_err(|e| crate::error::ArchiveError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &[&str] = &[
        "menu.file", "menu.edit", "menu.view", "menu.help", "menu.open", "menu.closeTab", "menu.closeWindow",
        "menu.extract", "menu.test", "menu.show", "menu.info", "menu.quit", "menu.undo",
        "menu.redo", "menu.cut", "menu.copy", "menu.paste", "menu.selectAll", "menu.refresh",
        "menu.language", "menu.langTr", "menu.langEn", "menu.fullscreen", "menu.minimize",
        "menu.about", "menu.services", "menu.hide", "menu.hideOthers", "menu.showAll", "menu.showHidden",
    ];

    #[test]
    fn every_menu_label_exists_in_both_languages() {
        for (name, src) in [("tr", TR), ("en", EN)] {
            let labels = parse_labels(src);
            for k in KEYS {
                assert!(labels.get(k).is_some_and(|v| !v.is_empty()), "{name} lacks {k}");
            }
        }
    }

    #[test]
    fn labels_are_read_verbatim() {
        assert_eq!(parse_labels(TR)["menu.file"], "Dosya");
        assert_eq!(parse_labels(TR)["menu.quit"], "Ambar'dan Çık");
        assert_eq!(parse_labels(TR)["menu.services"], "Hizmetler");
        assert_eq!(parse_labels(EN)["menu.file"], "File");
        assert!(!parse_labels(TR).contains_key("col.name"));
    }

    #[test]
    fn an_unknown_language_falls_back_to_turkish() {
        assert_eq!(parse_labels(table("xx"))["menu.help"], "Yardım");
    }
}

//! Host extensions: a page that embeds GridCraft (or any control-channel client) adds its own
//! items to the cell menu and to a ribbon tab, paints decorations on cells, and hears when they
//! are used. Everything here is UI state: nothing is saved in the workbook, undone or journaled.
//!
//! - `ui.host.items.set {items: [{id, label, icon?, tooltip?, places?: ["cell", "ribbon"]}],
//!   tab?}`: menu entries. `tab` names the ribbon tab that holds the `ribbon` items (default
//!   "Add-ins"). Labels are shown as given (send them translated).
//! - `ui.decorations.set {sheet?, cells: {"B6": {fill?, badge?, tooltip?, id?}}, replace?}`:
//!   a tint (`#RRGGBB` or `#RRGGBBAA`) and a corner badge (`#RRGGBB`) on cells of a sheet
//!   (default: the active one). `replace` (default true) drops the sheet's previous ones.
//! - `ui.decorations.clear {sheet?}`: one sheet's, or every sheet's without `sheet`.
//! - `ui.decorations.get {sheet?}`: what is set, by sheet (lowercase) then cell.
//! - `ui.host.invoke {id}`: as if the user chose that item.
//! - `ui.host.events`: takes the events waiting (control-channel clients poll; the web bridge
//!   posts them to the parent page each frame).
//!
//! Events: `{type: "gridcraft:action", id, sheet, active, ranges}` when an item is chosen, and
//! `{type: "gridcraft:cell-click", sheet, cell, id}` when a decorated cell is clicked (also
//! when it was already the active cell).

use std::collections::{BTreeMap, HashMap};

use egui::{Color32, Painter, Stroke, StrokeKind, pos2};
use gridcraft_engine::core::CellRef;
use gridcraft_engine::model::Sheet;
use serde_json::{Value as Json, json};

use crate::SheetApp;
use crate::grid::Geo;
use crate::widgets::big_button;

/// Host items kept; more are ignored.
const MAX_ITEMS: usize = 40;
/// Decorations kept across all sheets; more are refused.
const MAX_DECORATIONS: usize = 20_000;
/// Events kept for a client that never reads them (the oldest go first).
const MAX_EVENTS: usize = 200;
/// Longest label, tooltip or id taken from the host.
const MAX_TEXT: usize = 300;

/// The ribbon tab's name when the host doesn't give one.
const DEFAULT_TAB: &str = "Add-ins";

#[derive(Clone, Debug, PartialEq)]
pub struct HostItem {
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    pub tooltip: Option<String>,
    pub cell_menu: bool,
    pub ribbon: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decoration {
    pub fill: Option<Color32>,
    pub badge: Option<Color32>,
    pub tooltip: Option<String>,
    pub id: Option<String>,
}

#[derive(Default)]
pub struct HostExt {
    pub items: Vec<HostItem>,
    /// The ribbon tab holding the `ribbon` items, when there are some.
    pub tab: Option<String>,
    /// Sheet name (lowercase) → decorated cells.
    decorations: HashMap<String, BTreeMap<CellRef, Decoration>>,
    events: Vec<Json>,
}

fn text(v: Option<&Json>) -> Option<String> {
    let s: String = v?.as_str()?.trim().chars().take(MAX_TEXT).collect();
    (!s.is_empty()).then_some(s)
}

/// `#RRGGBB` or `#RRGGBBAA` (the `#` is optional).
pub fn parse_color(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    if !h.is_ascii() || !(h.len() == 6 || h.len() == 8) {
        return None;
    }
    let byte = |i: usize| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok());
    let (r, g, b) = (byte(0)?, byte(2)?, byte(4)?);
    let a = if h.len() == 8 { byte(6)? } else { 255 };
    Some(Color32::from_rgba_unmultiplied(r, g, b, a))
}

impl HostExt {
    /// `ui.host.items.set`.
    pub fn set_items(&mut self, p: &Json) -> Result<Json, String> {
        let list = match p.get("items") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Array(list)) => list.clone(),
            Some(_) => return Err("`items` must be an array".into()),
        };
        let mut items = Vec::new();
        for it in list.iter().take(MAX_ITEMS) {
            let (Some(id), Some(label)) = (text(it.get("id")), text(it.get("label"))) else {
                return Err("every item needs an `id` and a `label`".into());
            };
            let places: Vec<&str> =
                it.get("places").and_then(Json::as_array).map(|a| a.iter().filter_map(Json::as_str).collect()).unwrap_or_default();
            let (cell_menu, ribbon) = if places.is_empty() { (true, true) } else { (places.contains(&"cell"), places.contains(&"ribbon")) };
            // `|` splits a label in `Language::tr`; host labels are shown as given.
            items.push(HostItem {
                id,
                label: label.replace('|', "/"),
                icon: text(it.get("icon")),
                tooltip: text(it.get("tooltip")),
                cell_menu,
                ribbon,
            });
        }
        self.tab = items.iter().any(|i| i.ribbon).then(|| text(p.get("tab")).map(|t| t.replace('|', "/")).unwrap_or_else(|| DEFAULT_TAB.to_string()));
        self.items = items;
        Ok(json!({"items": self.items.len(), "tab": self.tab}))
    }

    pub fn clear_items(&mut self) {
        self.items.clear();
        self.tab = None;
    }

    fn total(&self) -> usize {
        self.decorations.values().map(BTreeMap::len).sum()
    }

    /// `ui.decorations.set`; `active_sheet` names the sheet when `sheet` is absent.
    pub fn set_decorations(&mut self, p: &Json, active_sheet: &str) -> Result<Json, String> {
        let sheet = text(p.get("sheet")).unwrap_or_else(|| active_sheet.to_string()).to_lowercase();
        let cells = match p.get("cells") {
            None | Some(Json::Null) => serde_json::Map::new(),
            Some(Json::Object(cells)) => cells.clone(),
            Some(_) => return Err("`cells` must be an object of A1 cell → decoration".into()),
        };
        let replace = p.get("replace").and_then(Json::as_bool).unwrap_or(true);
        let mut parsed = BTreeMap::new();
        for (a1, d) in &cells {
            let Some(cell) = CellRef::parse(a1.trim()) else { return Err(format!("not a cell: {a1:?}")) };
            let color = |key: &str| d.get(key).and_then(Json::as_str).and_then(parse_color);
            parsed.insert(cell, Decoration { fill: color("fill"), badge: color("badge"), tooltip: text(d.get("tooltip")), id: text(d.get("id")) });
        }
        let kept = if replace { 0 } else { self.decorations.get(&sheet).map_or(0, BTreeMap::len) };
        let others = self.total().saturating_sub(self.decorations.get(&sheet).map_or(0, BTreeMap::len));
        if others.saturating_add(kept).saturating_add(parsed.len()) > MAX_DECORATIONS {
            return Err(format!("too many decorations (at most {MAX_DECORATIONS})"));
        }
        let entry = self.decorations.entry(sheet.clone()).or_default();
        if replace {
            entry.clear();
        }
        entry.extend(parsed);
        let count = entry.len();
        if count == 0 {
            self.decorations.remove(&sheet);
        }
        Ok(json!({"sheet": sheet, "cells": count}))
    }

    /// `ui.decorations.clear`.
    pub fn clear_decorations(&mut self, sheet: Option<&str>) {
        match sheet {
            Some(s) => {
                self.decorations.remove(&s.to_lowercase());
            }
            None => self.decorations.clear(),
        }
    }

    /// `ui.decorations.get`: `{sheet: {A1: {fill, badge, tooltip, id}}}`, one sheet or all.
    pub fn get_decorations(&self, sheet: Option<&str>) -> Json {
        let hex = |c: Color32| {
            let [r, g, b, a] = c.to_srgba_unmultiplied();
            format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
        };
        let mut out = serde_json::Map::new();
        for (name, cells) in &self.decorations {
            if sheet.is_some_and(|s| !s.eq_ignore_ascii_case(name)) {
                continue;
            }
            let cells: serde_json::Map<String, Json> = cells
                .iter()
                .map(|(c, d)| (c.a1(), json!({"fill": d.fill.map(hex), "badge": d.badge.map(hex), "tooltip": d.tooltip, "id": d.id})))
                .collect();
            out.insert(name.clone(), Json::Object(cells));
        }
        Json::Object(out)
    }

    pub fn decoration(&self, sheet: &str, cell: CellRef) -> Option<&Decoration> {
        self.decorations.get(&sheet.to_lowercase())?.get(&cell)
    }

    fn sheet_decorations(&self, sheet: &str) -> Option<&BTreeMap<CellRef, Decoration>> {
        self.decorations.get(&sheet.to_lowercase())
    }

    pub fn push(&mut self, event: Json) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    /// The events waiting, oldest first.
    pub fn take_events(&mut self) -> Vec<Json> {
        std::mem::take(&mut self.events)
    }
}

/// The active workbook's sheet and selection, as events carry them.
fn selection_fields(app: &SheetApp) -> Json {
    let Some(doc) = app.session.active() else { return json!({}) };
    let sheet = doc.wb.active().map(|s| s.name.clone()).unwrap_or_default();
    let ranges: Vec<String> = doc.selection.ranges.iter().map(|r| r.a1()).collect();
    json!({"sheet": sheet, "active": doc.selection.active.a1(), "ranges": ranges})
}

/// A host item was chosen (cell menu or ribbon).
pub fn invoke(app: &mut SheetApp, id: &str) {
    let mut event = selection_fields(app);
    if let Some(fields) = event.as_object_mut() {
        fields.insert("type".into(), json!("gridcraft:action"));
        fields.insert("id".into(), json!(id));
    }
    app.host.push(event);
}

/// A decorated cell was clicked.
pub fn cell_clicked(app: &mut SheetApp, sheet: &str, cell: CellRef) {
    let Some(d) = app.host.decoration(sheet, cell) else { return };
    let event = json!({"type": "gridcraft:cell-click", "sheet": sheet, "cell": cell.a1(), "id": d.id});
    app.host.push(event);
}

/// The host tab of the ribbon: one big button per `ribbon` item.
pub fn ribbon(app: &mut SheetApp, ui: &mut egui::Ui) {
    let items: Vec<HostItem> = app.host.items.iter().filter(|i| i.ribbon).cloned().collect();
    for item in items {
        let icon = item.icon.as_deref().and_then(crate::icons::from_name).unwrap_or(crate::icons::Icon::Link);
        let tip = item.tooltip.clone().unwrap_or_else(|| item.label.clone());
        let resp = big_button(ui, icon, &item.label, &tip, false);
        let label = item.label.replace('\n', " ");
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        if resp.clicked() {
            invoke(app, &item.id);
        }
    }
}

/// Tints and badges on the visible decorated cells, and the hovered one's tooltip.
pub(crate) fn paint(app: &SheetApp, ui: &egui::Ui, p: &Painter, geo: &Geo, sh: &Sheet) {
    let Some(cells) = app.host.sheet_decorations(&sh.name) else { return };
    let clip = p.clip_rect();
    for (c, d) in cells {
        let r = geo.cell_rect(sh, *c);
        if !clip.intersects(r) || geo.hidden_under_frozen(sh, *c) {
            continue;
        }
        if let Some(fill) = d.fill {
            p.rect_filled(r.shrink(1.0), 0.0, fill);
        }
        if let Some(badge) = d.badge {
            let s = (r.height() * 0.5).clamp(7.0, 11.0);
            let tri = vec![r.left_top(), pos2(r.left() + s, r.top()), pos2(r.left(), r.top() + s)];
            p.add(egui::Shape::convex_polygon(tri, badge, Stroke::NONE));
            p.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, badge.gamma_multiply(0.55)), StrokeKind::Inside);
        }
    }
    if app.editor.is_none()
        && let Some(hover) = app.grid.hover_cell
        && let Some(tip) = cells.get(&hover).and_then(|d| d.tooltip.clone())
    {
        egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new("host_decoration_tip"), egui::PopupAnchor::Pointer).show(|ui| {
            ui.label(tip);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_with_and_without_alpha() {
        assert_eq!(parse_color("#2E7D32"), Some(Color32::from_rgb(0x2E, 0x7D, 0x32)));
        assert_eq!(parse_color("2e7d3240"), Some(Color32::from_rgba_unmultiplied(0x2E, 0x7D, 0x32, 0x40)));
        assert_eq!(parse_color("#12"), None);
        assert_eq!(parse_color("#zzzzzz"), None);
        assert_eq!(parse_color("#ééé"), None);
    }

    #[test]
    fn items_default_to_both_places_and_name_a_tab() {
        let mut h = HostExt::default();
        let r = h.set_items(&json!({"items": [{"id": "ref", "label": "Référencer…"}], "tab": "Koori"}));
        assert!(r.is_ok());
        assert_eq!(h.tab.as_deref(), Some("Koori"));
        assert!(h.items.first().is_some_and(|i| i.cell_menu && i.ribbon));
        assert!(h.set_items(&json!({"items": [{"id": "x"}]})).is_err());
        assert!(h.set_items(&json!({})).is_ok());
        assert!(h.items.is_empty() && h.tab.is_none());
    }

    #[test]
    fn decorations_replace_merge_and_clear() {
        let mut h = HostExt::default();
        let c = |a: &str| CellRef::parse(a).unwrap_or_default();
        assert!(h.set_decorations(&json!({"cells": {"B6": {"fill": "#0000FF22", "id": "r1"}}}), "Feuil1").is_ok());
        assert_eq!(h.decoration("FEUIL1", c("B6")).and_then(|d| d.id.clone()).as_deref(), Some("r1"));
        assert!(h.set_decorations(&json!({"cells": {"C1": {}}, "replace": false}), "Feuil1").is_ok());
        assert!(h.decoration("Feuil1", c("B6")).is_some() && h.decoration("Feuil1", c("C1")).is_some());
        assert!(h.set_decorations(&json!({"cells": {"C1": {}}}), "Feuil1").is_ok());
        assert!(h.decoration("Feuil1", c("B6")).is_none());
        assert!(h.set_decorations(&json!({"cells": {"nope": {}}}), "Feuil1").is_err());
        assert_eq!(h.get_decorations(Some("FEUIL1"))["feuil1"]["C1"]["fill"], Json::Null);
        assert!(h.get_decorations(Some("other")).as_object().is_some_and(|m| m.is_empty()));
        h.clear_decorations(None);
        assert!(h.decoration("Feuil1", c("C1")).is_none());
        assert!(h.set_decorations(&json!({}), "Feuil1").is_ok());
    }

    #[test]
    fn events_are_capped() {
        let mut h = HostExt::default();
        for i in 0..(MAX_EVENTS + 5) {
            h.push(json!({"n": i}));
        }
        let events = h.take_events();
        assert_eq!(events.len(), MAX_EVENTS);
        assert_eq!(events.first().and_then(|e| e["n"].as_u64()), Some(5));
        assert!(h.take_events().is_empty());
    }
}

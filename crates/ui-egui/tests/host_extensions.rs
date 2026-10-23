//! Host extensions: an embedding page's cell-menu items, ribbon tab and cell decorations, and
//! the events they raise (`ui.host.events`).

use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::kittest::Queryable;
use gridcraft_engine::Session;
use gridcraft_engine::core::CellRef;
use gridcraft_ui_egui::{SheetApp, grid};
use serde_json::{Value as Json, json};

type Harness = egui_kittest::Harness<'static, SheetApp>;

fn harness() -> Harness {
    let mut session = Session::new();
    session.new_workbook();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(
        |ui, app: &mut SheetApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        SheetApp::new(session, Default::default()),
    );
    h.run_steps(4);
    h
}

fn cell_center(h: &Harness, cell: &str) -> Pos2 {
    let app = h.state();
    let sh = app.session.active().unwrap().wb.active().unwrap();
    let geo = grid::Geo::new(sh, app.grid.rect.unwrap(), app.view().scroll);
    geo.cell_rect(sh, CellRef::parse(cell).unwrap()).center()
}

fn click(h: &mut Harness, pos: Pos2, button: PointerButton) {
    h.input_mut().events.push(Event::PointerMoved(pos));
    h.input_mut().events.push(Event::PointerButton { pos, button, pressed: true, modifiers: Modifiers::NONE });
    h.input_mut().events.push(Event::PointerButton { pos, button, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

fn events(h: &mut Harness) -> Vec<Json> {
    h.state_mut().run("ui.host.events", json!({})).unwrap().as_array().cloned().unwrap()
}

#[test]
fn a_host_item_in_the_cell_menu_reports_the_selection() {
    let mut h = harness();
    h.state_mut().run("ui.host.items.set", json!({"items": [{"id": "koori.reference", "label": "Référencer…"}], "tab": "Koori"})).unwrap();
    h.run_steps(2);
    let at = cell_center(&h, "C4");
    click(&mut h, at, PointerButton::Secondary);
    h.get_by_label("Référencer…").click();
    h.run_steps(2);
    let got = events(&mut h);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["type"], "gridcraft:action");
    assert_eq!(got[0]["id"], "koori.reference");
    assert_eq!(got[0]["active"], "C4");
    assert_eq!(got[0]["sheet"], h.state().session.active().unwrap().wb.active().unwrap().name.as_str());
}

#[test]
fn the_host_tab_holds_its_ribbon_items() {
    let mut h = harness();
    h.state_mut()
        .run(
            "ui.host.items.set",
            json!({"items": [{"id": "koori.panel", "label": "Documents", "places": ["ribbon"], "icon": "book"}], "tab": "Koori"}),
        )
        .unwrap();
    h.run_steps(2);
    h.state_mut().run("ui.ribbonTab", json!({"tab": "Koori"})).unwrap();
    h.run_steps(2);
    h.get_by_label("Documents").click();
    h.run_steps(2);
    let got = events(&mut h);
    assert_eq!(got.first().map(|e| e["id"].clone()), Some(json!("koori.panel")));
}

#[test]
fn a_ribbon_only_item_stays_out_of_the_cell_menu() {
    let mut h = harness();
    h.state_mut()
        .run(
            "ui.host.items.set",
            json!({"items": [
                {"id": "koori.panel", "label": "Documents", "places": ["ribbon"]},
                {"id": "koori.reference", "label": "Référencer…", "places": ["cell"]},
            ], "tab": "Koori"}),
        )
        .unwrap();
    h.run_steps(2);
    let at = cell_center(&h, "C4");
    click(&mut h, at, PointerButton::Secondary);
    assert!(h.query_by_label("Référencer…").is_some());
    assert!(h.query_by_label("Documents").is_none());
}

#[test]
fn clicking_a_decorated_cell_reports_it_even_when_already_active() {
    let mut h = harness();
    h.state_mut()
        .run("ui.decorations.set", json!({"cells": {"B3": {"fill": "#2E7D3233", "badge": "#1565C0", "tooltip": "Facture p.1", "id": "ref-1"}}}))
        .unwrap();
    h.run_steps(2);
    let at = cell_center(&h, "B3");
    click(&mut h, at, PointerButton::Primary);
    // Past the double-click window (wall clock), which would edit the cell instead.
    std::thread::sleep(std::time::Duration::from_millis(500));
    click(&mut h, at, PointerButton::Primary);
    let got = events(&mut h);
    let clicks: Vec<&Json> = got.iter().filter(|e| e["type"] == "gridcraft:cell-click").collect();
    assert_eq!(clicks.len(), 2, "{got:?}");
    assert_eq!(clicks[0]["cell"], "B3");
    assert_eq!(clicks[0]["id"], "ref-1");
    // An undecorated cell says nothing.
    let other = cell_center(&h, "D5");
    click(&mut h, other, PointerButton::Primary);
    assert!(events(&mut h).is_empty());
    // Decorations are UI-only: the workbook stays clean.
    assert!(!h.state().session.active().unwrap().is_dirty());
}

#[test]
fn invoking_a_host_item_programmatically_reports_like_a_click() {
    let mut h = harness();
    assert!(h.state_mut().run("ui.host.invoke", json!({"id": "koori.reference"})).is_err());
    h.state_mut().run("ui.host.items.set", json!({"items": [{"id": "koori.reference", "label": "Référencer…"}]})).unwrap();
    h.state_mut().run("selection.set", json!({"cell": "E7"})).unwrap();
    h.state_mut().run("ui.host.invoke", json!({"id": "koori.reference"})).unwrap();
    let got = events(&mut h);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["active"], "E7");
}

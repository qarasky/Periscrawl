//! Rendering: panels, session list, crawl form, live view, data tables,
//! detail inspector, diff view and settings.

use std::cmp::Ordering;

use eframe::egui::{self, Align, Color32, Layout, RichText};
use egui_extras::{Column, TableBuilder};

use rustyseo_core::diff::DiffKind;
use rustyseo_core::export::{self, Sheet};

use crate::app::{AssetKind, CentralTab, DetailView, ExportKind, RustySeoApp};

const ACCENT: Color32 = Color32::from_rgb(90, 150, 255);
const OK_GREEN: Color32 = Color32::from_rgb(120, 215, 140);
const WARN_YELLOW: Color32 = Color32::from_rgb(255, 195, 100);
const ERR_RED: Color32 = Color32::from_rgb(255, 110, 110);

pub fn draw(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    top_panel(app, ui);
    left_panel(app, ui);
    bottom_panel(app, ui);
    detail_panel(app, ui);
    central(app, ui);
    detail_resize_grip(app, ui);
    settings_window(app, ui.ctx());
}

/// Explicit left-edge grip for the details pane. egui's built-in right-panel
/// handle sits at the window edge (and macOS may claim those pixels for
/// native window resizing), so the dashboard-style "drag the border next to
/// the table" interaction is implemented here.
fn detail_resize_grip(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    let Some(rect) = app.detail_rect else {
        return;
    };
    let grip_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() - 4.0, rect.top()),
        egui::pos2(rect.left() + 4.0, rect.bottom()),
    );
    let response = ui.interact(
        grip_rect,
        egui::Id::new("details_resize_grip"),
        egui::Sense::drag(),
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if response.dragged() {
        app.detail_width =
            (app.detail_width - response.drag_delta().x).clamp(320.0, 1600.0);
    }
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

fn top_panel(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    egui::Panel::top("top").show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(RichText::new("RustySEO").strong().color(ACCENT));
            ui.separator();

            if let Some(id) = app.selected_session {
                if let Some(session) = app.sessions.iter().find(|s| s.id == id) {
                    ui.label(RichText::new(format!("Session #{id}")).strong());
                    ui.label(RichText::new(&session.start_url).weak());
                    ui.label(format!(
                        "{} pages • {} failed",
                        session.pages_fetched, session.pages_failed
                    ));
                } else {
                    ui.label(RichText::new(format!("Session #{id}")).strong());
                }
            } else {
                ui.label(RichText::new("No session selected").weak());
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⚙ Settings").clicked() {
                    app.open_settings();
                }
                let exporting_enabled = !app.sheets.is_empty() && app.crawl.is_none();
                if ui
                    .add_enabled(exporting_enabled, egui::Button::new("Export PDF"))
                    .clicked()
                {
                    app.export_current(ExportKind::Pdf);
                }
                if ui
                    .add_enabled(exporting_enabled, egui::Button::new("Export .json"))
                    .clicked()
                {
                    app.export_current(ExportKind::Json);
                }
                if ui
                    .add_enabled(exporting_enabled, egui::Button::new("Export CSVs"))
                    .clicked()
                {
                    app.export_current(ExportKind::CsvZip);
                }
                if ui
                    .add_enabled(exporting_enabled, egui::Button::new("Export .xlsx"))
                    .clicked()
                {
                    app.export_current(ExportKind::Xlsx);
                }
            });
        });
        ui.add_space(4.0);
    });
}

fn left_panel(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    egui::Panel::left("side")
        .resizable(true)
        .default_size(330.0)
        .show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading("New crawl");
            ui.add_space(4.0);
            egui::Grid::new("crawl_form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("URL");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.form.url)
                            .hint_text("https://example.com")
                            .desired_width(190.0),
                    );
                    ui.end_row();

                    ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut app.form.name)
                            .hint_text("optional label")
                            .desired_width(190.0),
                    );
                    ui.end_row();

                    ui.label("Max pages");
                    ui.add(egui::DragValue::new(&mut app.form.max_pages).range(1..=100_000));
                    ui.end_row();

                    ui.label("Concurrency");
                    ui.add(egui::DragValue::new(&mut app.form.concurrency).range(1..=64));
                    ui.end_row();

                    ui.label("JavaScript");
                    ui.checkbox(&mut app.form.javascript, "render with Chrome");
                    ui.end_row();
                });

            ui.add_space(6.0);
            let crawling = app.crawl.is_some();
            if ui
                .add_enabled(
                    !crawling,
                    egui::Button::new(RichText::new("Start crawl").strong()),
                )
                .clicked()
            {
                app.start_crawl();
            }
            if crawling {
                ui.label(RichText::new("Crawl in progress…").italics());
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Sessions");
                if ui.small_button("⟳").on_hover_text("Reload list").clicked() {
                    app.refresh_sessions();
                }
            });
            ui.add_space(2.0);

            let mut clicked: Option<i64> = None;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if app.sessions.is_empty() {
                        ui.label(RichText::new("No sessions yet — run a crawl.").weak());
                    }
                    for session in &app.sessions {
                        let label = format!(
                            "#{} {} — {} pages",
                            session.id,
                            session
                                .name
                                .clone()
                                .unwrap_or_else(|| short_url(&session.start_url)),
                            session.pages_fetched
                        );
                        let selected = app.selected_session == Some(session.id);
                        if ui
                            .selectable_label(selected, label)
                            .on_hover_text(format!(
                                "{}\nstarted {}",
                                session.start_url,
                                format_started(&session.started_at)
                            ))
                            .clicked()
                        {
                            clicked = Some(session.id);
                        }
                        ui.label(
                            RichText::new(format!(
                                "    {} • {}",
                                session.status,
                                format_started(&session.started_at)
                            ))
                            .weak()
                            .small(),
                        );
                    }
                });
            if let Some(id) = clicked {
                app.load_session(id);
            }
        });
}

fn bottom_panel(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("status").show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            match &app.crawl {
                Some(crawl) => {
                    ui.spinner();
                    ui.label(format!(
                        "Crawling • {} pages • {} failed • queued {} • processing {} • {} robots-blocked • {:.0}s{}",
                        crawl.pages,
                        crawl.failed,
                        crawl.queued,
                        crawl.processing,
                        crawl.robots_blocked,
                        crawl.started.elapsed().as_secs_f32(),
                        if crawl.stop_requested { " • stopping" } else { "" }
                    ));
                }
                None => {
                    ui.label(&app.status);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let sync_port = app.sync_port.lock().ok().and_then(|port| *port);
                match sync_port {
                    Some(port) => {
                        ui.label(
                            RichText::new(format!("sync ● 127.0.0.1:{port}"))
                                .color(OK_GREEN)
                                .small(),
                        )
                        .on_hover_text(
                            "rustyseo status / watch / stop and other instances attach here",
                        );
                    }
                    None => {
                        ui.label(RichText::new("sync off").weak().small())
                            .on_hover_text(
                                "No sync server is listening (another instance may already hold it)",
                            );
                    }
                }
                ui.label(
                    RichText::new(format!("{} sessions", app.sessions.len()))
                        .weak()
                        .small(),
                );
            });
        });
        ui.add_space(2.0);
    });
}

fn central(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        if app.crawl.is_some() {
            live_view(app, ui);
            return;
        }
        if app.loading_session {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading session…");
            });
            return;
        }
        if app.sheets.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Start a crawl or pick a session from the left.")
                        .weak()
                        .size(15.0),
                );
            });
            return;
        }

        tab_bar(app, ui);
        ui.add_space(4.0);

        match app.active_tab {
            CentralTab::Sheet(index) => {
                filter_row(app, ui);
                ui.separator();
                sheet_body(app, ui, index);
            }
            CentralTab::Diff => {
                ui.separator();
                diff_body(app, ui);
            }
        }
    });
}

fn tab_bar(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    let mut new_tab: Option<CentralTab> = None;
    ui.horizontal_wrapped(|ui| {
        for (index, sheet) in app.sheets.iter().enumerate() {
            let label = format!("{} ({})", sheet.name, sheet.rows.len());
            let selected = app.active_tab == CentralTab::Sheet(index);
            if ui.selectable_label(selected, label).clicked() {
                new_tab = Some(CentralTab::Sheet(index));
            }
        }
        let diff_selected = app.active_tab == CentralTab::Diff;
        if ui
            .selectable_label(diff_selected, "⇄ Diff")
            .on_hover_text("Compare this session against another crawl")
            .clicked()
        {
            new_tab = Some(CentralTab::Diff);
        }
    });
    if let Some(tab) = new_tab {
        if tab != app.active_tab {
            app.active_tab = tab;
            app.active_sort = None;
            app.filter.clear();
        }
    }
}

fn filter_row(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("Filter:");
        ui.add(
            egui::TextEdit::singleline(&mut app.filter)
                .hint_text("type to filter rows…")
                .desired_width(260.0),
        );
        if !app.filter.is_empty() && ui.small_button("clear").clicked() {
            app.filter.clear();
        }
        if let Some((column, ascending)) = app.active_sort {
            ui.label(
                RichText::new(format!(
                    "sorted by column {} {}",
                    column + 1,
                    if ascending { "▲" } else { "▼" }
                ))
                .weak()
                .small(),
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Sheet tables
// ---------------------------------------------------------------------------

enum ClickTarget {
    Page(String),
    Asset(AssetKind, String),
    Issue(String),
}

fn asset_kind_for_sheet(sheet_name: &str) -> Option<AssetKind> {
    match sheet_name {
        "Images" => Some(AssetKind::Images),
        "CSS" => Some(AssetKind::Css),
        "JavaScript" => Some(AssetKind::JavaScript),
        "Files" => Some(AssetKind::Files),
        _ => None,
    }
}

fn sheet_body(app: &mut RustySeoApp, ui: &mut egui::Ui, sheet_index: usize) {
    let selected_url: Option<String> = match &app.detail {
        Some(DetailView::Page { index }) => app.pages.get(*index).map(|p| p.url.clone()),
        _ => None,
    };
    let filter = app.filter.clone();
    let sort = app.active_sort;

    let mut clicked_target: Option<ClickTarget> = None;
    let mut new_sort: Option<(usize, bool)> = None;

    if let Some(sheet) = app.sheets.get(sheet_index) {
        sheet_table(
            ui,
            sheet,
            &filter,
            sort,
            selected_url.as_deref(),
            &mut clicked_target,
            &mut new_sort,
        );
    }

    if let Some(sort) = new_sort {
        app.active_sort = Some(sort);
    }
    if let Some(target) = clicked_target {
        match target {
            ClickTarget::Page(url) => match app.resolve_page(&url) {
                Some(index) => app.detail = Some(DetailView::Page { index }),
                None => app.status = format!("Page not in this session: {url}"),
            },
            ClickTarget::Asset(kind, url) => {
                app.detail = Some(DetailView::Asset { kind, url });
            }
            ClickTarget::Issue(name) => {
                app.detail = Some(DetailView::Issue { name });
            }
        }
    }
}

fn column_width(header: &str) -> f32 {
    let header = header.to_lowercase();
    if header.contains("sample") || header.contains("changes") {
        260.0
    } else if header.contains("url") || header.contains("target") {
        320.0
    } else if header.contains("title") || header.contains("description") || header.contains("anchor")
    {
        240.0
    } else {
        110.0
    }
}

fn sort_indices(rows: &[Vec<String>], indices: &mut [usize], column: usize, ascending: bool) {
    indices.sort_by(|a, b| {
        let value_a = rows[*a].get(column).map(String::as_str).unwrap_or("");
        let value_b = rows[*b].get(column).map(String::as_str).unwrap_or("");
        let order = match (value_a.parse::<f64>(), value_b.parse::<f64>()) {
            (Ok(number_a), Ok(number_b)) => number_a.partial_cmp(&number_b).unwrap_or(Ordering::Equal),
            _ => value_a.to_lowercase().cmp(&value_b.to_lowercase()),
        };
        if ascending { order } else { order.reverse() }
    });
}

#[allow(clippy::too_many_arguments)]
fn sheet_table(
    ui: &mut egui::Ui,
    sheet: &Sheet,
    filter: &str,
    sort: Option<(usize, bool)>,
    selected_url: Option<&str>,
    clicked_target: &mut Option<ClickTarget>,
    new_sort: &mut Option<(usize, bool)>,
) {
    // Rows must be clickable all the way through: text never captures clicks.
    ui.style_mut().interaction.selectable_labels = false;
    let needle = filter.trim().to_lowercase();
    let mut indices: Vec<usize> = if needle.is_empty() {
        (0..sheet.rows.len()).collect()
    } else {
        sheet
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.iter().any(|cell| cell.to_lowercase().contains(&needle)))
            .map(|(index, _)| index)
            .collect()
    };
    if let Some((column, ascending)) = sort {
        if column < sheet.header.len() {
            sort_indices(&sheet.rows, &mut indices, column, ascending);
        }
    }

    ui.label(
        RichText::new(format!(
            "{} rows total • {} shown",
            sheet.rows.len(),
            indices.len()
        ))
        .weak()
        .small(),
    );
    ui.add_space(2.0);

    let sheet_name = sheet.name.clone();
    let header = sheet.header.clone();

    // Horizontal scrolling: all columns get explicit widths; the scroll area
    // provides the horizontal scrollbar when their sum exceeds the viewport.
    // Drag-to-scroll is disabled so a slightly-moving click is never turned
    // into a table pan (which also cancels the click).
    egui::ScrollArea::horizontal()
        .id_salt(("hscroll", sheet_name.clone()))
        .auto_shrink([false, false])
        .scroll_source(egui::containers::scroll_area::ScrollSource {
            drag: egui::containers::scroll_area::DragScroll::Never,
            ..Default::default()
        })
        .show(ui, |ui| {
            let mut table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .min_scrolled_height(0.0);

            for title in &header {
                table = table.column(
                    Column::initial(column_width(title))
                        .at_least(60.0)
                        .clip(true),
                );
            }

            table
                .header(24.0, |mut header_row| {
                    for (index, title) in header.iter().enumerate() {
                        header_row.col(|ui| {
                            let arrow = match sort {
                                Some((column, ascending)) if column == index => {
                                    if ascending { " ▲" } else { " ▼" }
                                }
                                _ => "",
                            };
                            if ui
                                .button(RichText::new(format!("{title}{arrow}")).strong())
                                .clicked()
                            {
                                let ascending = match sort {
                                    Some((column, was_ascending)) if column == index => {
                                        !was_ascending
                                    }
                                    _ => true,
                                };
                                *new_sort = Some((index, ascending));
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(18.0, indices.len(), |mut row| {
                        let source = indices[row.index()];
                        let data_row = &sheet.rows[source];
                        for (column_index, cell) in data_row.iter().enumerate() {
                            if column_index >= header.len() {
                                break;
                            }
                            row.col(|ui| {
                                ui.label(cell);
                            });
                        }
                        let is_selected = selected_url
                            .is_some_and(|url| data_row.iter().any(|cell| cell == url));
                        row.set_selected(is_selected);
                        // Open details on pointer *press*: egui cancels a
                        // `clicked()` when the pointer moves a few pixels
                        // between press and release (trackpad jitter makes
                        // this feel random), so press-driven opening gives
                        // a stable single-click interaction.
                        let response = row.response();
                        if response.clicked() || response.is_pointer_button_down_on() {
                            *clicked_target = click_target_for(&sheet_name, data_row);
                        }
                    });
                });
        });
}

fn click_target_for(sheet_name: &str, row: &[String]) -> Option<ClickTarget> {
    if let Some(kind) = asset_kind_for_sheet(sheet_name) {
        return row.first().map(|url| ClickTarget::Asset(kind, url.clone()));
    }
    match sheet_name {
        // severity | issue | count | sample urls
        "Issues" => row.get(1).map(|issue| ClickTarget::Issue(issue.clone())),
        // ID | URL | ...
        "Overview" => row.get(1).map(|url| ClickTarget::Page(url.clone())),
        // page | target | ...
        _ => row.first().map(|url| ClickTarget::Page(url.clone())),
    }
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

fn diff_body(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    // Same click-through guarantee as the data tables.
    ui.style_mut().interaction.selectable_labels = false;
    ui.horizontal(|ui| {
        ui.heading("Compare with");
        ui.label(RichText::new("(a previous crawl of the same site)").weak());
    });
    ui.add_space(4.0);

    let mut selection = app.diff_with;
    egui::ComboBox::from_id_salt("diff_session_pick")
        .selected_text(match selection {
            Some(id) => format!("Session #{id}"),
            None => "pick a session…".to_string(),
        })
        .width(360.0)
        .show_ui(ui, |ui| {
            for session in &app.sessions {
                if Some(session.id) == app.selected_session {
                    continue;
                }
                let label = format!(
                    "#{} {} — {} pages",
                    session.id,
                    session
                        .name
                        .clone()
                        .unwrap_or_else(|| short_url(&session.start_url)),
                    session.pages_fetched
                );
                ui.selectable_value(&mut selection, Some(session.id), label);
            }
        });
    if selection != app.diff_with {
        app.diff_with = selection;
        if let Some(id) = selection {
            app.start_diff(id);
        } else {
            app.diff_entries.clear();
            app.diff_stats = None;
        }
    }

    ui.add_space(6.0);
    if app.diff_loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Diffing…");
        });
        return;
    }
    let Some(stats) = &app.diff_stats else {
        ui.label(
            RichText::new("Pick a session above to see what changed since that crawl.").weak(),
        );
        return;
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("+{} added", stats.added)).color(OK_GREEN));
        ui.label(RichText::new(format!("−{} removed", stats.removed)).color(ERR_RED));
        ui.label(RichText::new(format!("~{} changed", stats.changed)).color(WARN_YELLOW));
        ui.label(RichText::new(format!("{} unchanged", stats.unchanged)).weak());
    });
    ui.add_space(4.0);

    if app.diff_entries.is_empty() {
        ui.label("No differences — the crawls match.");
        return;
    }

    let mut clicked: Option<usize> = None;
    egui::ScrollArea::horizontal()
        .id_salt("diff_hscroll")
        .auto_shrink([false, false])
        .scroll_source(egui::containers::scroll_area::ScrollSource {
            drag: egui::containers::scroll_area::DragScroll::Never,
            ..Default::default()
        })
        .show(ui, |ui| {
            let table = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .sense(egui::Sense::click())
                .cell_layout(Layout::left_to_right(Align::Center))
                .min_scrolled_height(0.0)
                .column(Column::initial(90.0).at_least(70.0).clip(true))
                .column(Column::initial(320.0).at_least(140.0).clip(true))
                .column(Column::initial(420.0).at_least(160.0).clip(true));

            table
                .header(24.0, |mut header_row| {
                    for title in ["Type", "URL", "Changes"] {
                        header_row.col(|ui| {
                            ui.strong(title);
                        });
                    }
                })
                .body(|body| {
                    body.rows(18.0, app.diff_entries.len(), |mut row| {
                        let index = row.index();
                        let entry = &app.diff_entries[index];
                        row.col(|ui| {
                            let (label, color) = match entry.kind {
                                DiffKind::Added => ("added", OK_GREEN),
                                DiffKind::Removed => ("removed", ERR_RED),
                                DiffKind::Changed => ("changed", WARN_YELLOW),
                            };
                            ui.label(RichText::new(label).color(color));
                        });
                        row.col(|ui| {
                            ui.label(&entry.url);
                        });
                        row.col(|ui| {
                            let summary = entry
                                .changes
                                .iter()
                                .map(|change| {
                                    format!(
                                        "{}: {} → {}",
                                        change.field, change.old, change.new
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("  •  ");
                            ui.label(summary);
                        });
                        if row.response().clicked()
                            || row.response().is_pointer_button_down_on()
                        {
                            clicked = Some(index);
                        }
                    });
                });
        });
    if let Some(index) = clicked {
        app.detail = Some(DetailView::DiffEntry { index });
    }
}

// ---------------------------------------------------------------------------
// Detail inspector
// ---------------------------------------------------------------------------

fn detail_panel(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    if app.detail.is_none() {
        app.detail_rect = None;
        return;
    }
    let max_width = (ui.available_width() - 320.0).max(320.0);
    let width = app.detail_width.clamp(320.0, max_width);
    let mut close = false;
    let mut next: Option<DetailView> = None;
    {
        let detail = app.detail.clone().expect("checked above");
        let title = match &detail {
            DetailView::Page { .. } => "Page details",
            DetailView::Asset { .. } => "Asset usage",
            DetailView::Issue { .. } => "Issue URLs",
            DetailView::DiffEntry { .. } => "Change details",
        };
        let panel = egui::Panel::right("details")
            .resizable(false)
            .exact_size(width)
            .show(ui, |ui| {
                // Keep the inspector copy-friendly even though global label
                // selection is disabled for table click-through.
                ui.style_mut().interaction.selectable_labels = true;
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.heading(title);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("✕").on_hover_text("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        next = match &detail {
                            DetailView::Page { index } => page_detail(app, ui, *index),
                            DetailView::Asset { kind, url } => asset_detail(app, ui, *kind, url),
                            DetailView::Issue { name } => issue_detail(app, ui, name),
                            DetailView::DiffEntry { index } => diff_entry_detail(app, ui, *index),
                        };
                    });
            });
        app.detail_rect = Some(panel.response.rect);
    }
    if close {
        app.detail = None;
    } else if let Some(view) = next {
        app.detail = Some(view);
    }
}

fn kv(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.label(RichText::new(key).weak());
    ui.label(value);
    ui.end_row();
}

fn page_detail(app: &RustySeoApp, ui: &mut egui::Ui, index: usize) -> Option<DetailView> {
    let Some(page) = app.pages.get(index) else {
        ui.label("Page data not loaded");
        return None;
    };
    let mut next: Option<DetailView> = None;

    ui.hyperlink_to(RichText::new(&page.url).strong(), &page.url);
    if !page.requested_url.is_empty() && page.requested_url != page.url {
        ui.label(
            RichText::new(format!("redirected from {}", page.requested_url))
                .weak()
                .small(),
        );
    }
    ui.add_space(6.0);

    egui::Grid::new("page_meta")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            kv(ui, "Status", &page.status);
            kv(ui, "Title", &page.title);
            kv(ui, "Title length", &page.title_len.to_string());
            kv(ui, "Description", &page.description);
            kv(ui, "Description length", &page.description_len.to_string());
            kv(ui, "H1", &page.h1);
            kv(ui, "Words", &page.word_count.unwrap_or(0).to_string());
            kv(ui, "Size (bytes)", &page.size.to_string());
            kv(ui, "Indexability", &page.indexability);
            kv(ui, "Language", &page.language);
            kv(ui, "Mobile", &page.mobile.to_string());
            kv(
                ui,
                "Canonical target",
                page.canonicals
                    .iter()
                    .find(|(rel, _, _)| rel.eq_ignore_ascii_case("canonical"))
                    .map(|(_, href, _)| href.as_str())
                    .unwrap_or("—"),
            );
            if !page.redirect_chain.is_empty() {
                let chain = page
                    .redirect_chain
                    .iter()
                    .map(|hop| format!("{} {}", hop.status, hop.url))
                    .collect::<Vec<_>>()
                    .join("  →  ");
                kv(ui, "Redirect chain", &chain);
            }
            if let Some(desktop) = &page.cwv_desktop {
                kv(
                    ui,
                    "CWV desktop",
                    &format!(
                        "score {} • LCP {} • CLS {} • TBT {}",
                        desktop.performance_score, desktop.lcp, desktop.cls, desktop.tbt
                    ),
                );
            }
            if let Some(mobile) = &page.cwv_mobile {
                kv(
                    ui,
                    "CWV mobile",
                    &format!(
                        "score {} • LCP {} • CLS {} • TBT {}",
                        mobile.performance_score, mobile.lcp, mobile.cls, mobile.tbt
                    ),
                );
            }
        });

    ui.add_space(8.0);

    egui::CollapsingHeader::new(format!("Headings ({})", page.headings.len()))
        .default_open(false)
        .show(ui, |ui| {
            for (tag, text) in page.headings.iter().take(200) {
                ui.label(format!("{tag}: {text}"));
            }
        });

    if !page.keywords.is_empty() {
        egui::CollapsingHeader::new(format!("Keywords ({})", page.keywords.len()))
            .default_open(false)
            .show(ui, |ui| {
                ui.label(page.keywords.join(", "));
            });
    }

    let internal: Vec<&rustyseo_core::crawler::helpers::html_parser::AnchorLink> =
        page.anchor_links.iter().collect();
    let internal_set: std::collections::HashSet<&str> = page
        .anchor_links
        .iter()
        .map(|link| link.href.as_str())
        .collect();
    let external: Vec<&rustyseo_core::crawler::helpers::html_parser::AnchorLink> = page
        .outlinks
        .iter()
        .filter(|link| link.href.starts_with("http") && !internal_set.contains(link.href.as_str()))
        .collect();

    egui::CollapsingHeader::new(format!("Internal links ({})", internal.len()))
        .default_open(false)
        .show(ui, |ui| {
            for link in internal.iter().take(300) {
                let mark = if link.rel.to_lowercase().contains("nofollow") {
                    " [nofollow]"
                } else {
                    ""
                };
                ui.label(format!("{}{} — {}", link.href, mark, link.text.trim()));
            }
        });

    egui::CollapsingHeader::new(format!("External links ({})", external.len()))
        .default_open(false)
        .show(ui, |ui| {
            for link in external.iter().take(300) {
                ui.label(format!("{} — {}", link.href, link.text.trim()));
            }
        });

    egui::CollapsingHeader::new(format!("Images ({})", page.images.len()))
        .default_open(false)
        .show(ui, |ui| {
            for image in page.images.iter().take(200) {
                let label = if image.alt.trim().is_empty() {
                    format!("{}  (missing alt)", image.src)
                } else {
                    format!("{}  [{}]", image.src, image.alt)
                };
                if ui.link(label).clicked() {
                    next = Some(DetailView::Asset {
                        kind: AssetKind::Images,
                        url: image.src.clone(),
                    });
                }
            }
        });

    if !page.schema.is_empty() {
        egui::CollapsingHeader::new(format!("Structured data ({})", page.schema.len()))
            .default_open(false)
            .show(ui, |ui| {
                for block in page.schema.iter().take(3) {
                    let preview: String = block.chars().take(400).collect();
                    ui.monospace(preview);
                    ui.add_space(4.0);
                }
            });
    }

    if let Some(extraction) = &page.extraction {
        egui::CollapsingHeader::new("Custom extraction")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(format!(
                    "found: {} • matches: {} • search: {}",
                    extraction.found, extraction.count, extraction.search_text
                ));
                for matched in extraction.matches.iter().take(50) {
                    ui.monospace(format!("<{}> {}", matched.element, matched.snippet));
                }
            });
    }

    next
}

fn asset_detail(
    app: &RustySeoApp,
    ui: &mut egui::Ui,
    kind: AssetKind,
    url: &str,
) -> Option<DetailView> {
    let mut next = None;
    ui.label(RichText::new(url).monospace().weak());
    let users = app.asset_users(kind, url);
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!(
            "{} used on {} page(s)",
            kind.label(),
            users.len()
        ))
        .strong(),
    );
    ui.separator();
    for index in users {
        let page_url = &app.pages[index].url;
        if ui.selectable_label(false, page_url).clicked() {
            next = Some(DetailView::Page { index });
        }
    }
    next
}

fn issue_detail(app: &RustySeoApp, ui: &mut egui::Ui, name: &str) -> Option<DetailView> {
    let mut next = None;
    let urls = app.issue_urls(name);
    ui.label(RichText::new(name).strong());
    ui.label(RichText::new(format!("{} affected URL(s)", urls.len())).weak());
    ui.separator();

    for raw in urls {
        // Some analyzers emit annotations like "url (339 chars)" or pairs
        // joined with " [ and ] ".
        for part in raw.split(" [ and ] ") {
            let cleaned = part.trim();
            let url_only = cleaned.split(" (").next().unwrap_or(cleaned).trim();
            match app.resolve_page(url_only) {
                Some(index) => {
                    if ui.selectable_label(false, cleaned).clicked() {
                        next = Some(DetailView::Page { index });
                    }
                }
                None => {
                    ui.label(cleaned);
                }
            }
        }
    }
    next
}

fn diff_entry_detail(app: &RustySeoApp, ui: &mut egui::Ui, index: usize) -> Option<DetailView> {
    let Some(entry) = app.diff_entries.get(index) else {
        return None;
    };
    let mut next = None;
    let (label, color) = match entry.kind {
        DiffKind::Added => ("Added in this crawl", OK_GREEN),
        DiffKind::Removed => ("Removed since the compared crawl", ERR_RED),
        DiffKind::Changed => ("Changed since the compared crawl", WARN_YELLOW),
    };
    ui.label(RichText::new(label).color(color).strong());
    ui.label(RichText::new(&entry.url).monospace());
    ui.add_space(6.0);

    if let Some(page_index) = app.resolve_page(&entry.url) {
        if ui.button("Open page details").clicked() {
            next = Some(DetailView::Page { index: page_index });
        }
    }

    if !entry.changes.is_empty() {
        ui.add_space(4.0);
        egui::Grid::new("diff_changes")
            .num_columns(3)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Field").strong());
                ui.label(RichText::new("Old").strong());
                ui.label(RichText::new("New").strong());
                ui.end_row();
                for change in &entry.changes {
                    ui.label(&change.field);
                    ui.label(RichText::new(&change.old).weak());
                    ui.label(&change.new);
                    ui.end_row();
                }
            });
    }
    next
}

// ---------------------------------------------------------------------------
// Live crawl
// ---------------------------------------------------------------------------

fn live_view(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    let mut stop_clicked = false;
    {
        let Some(crawl) = app.crawl.as_ref() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.spinner();
            ui.heading("Crawling");
            ui.label(RichText::new(&crawl.url).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !crawl.stop_requested
                    && ui
                        .button(RichText::new("■ Stop").color(ERR_RED))
                        .clicked()
                {
                    stop_clicked = true;
                }
            });
        });
        ui.add_space(4.0);

        // Processed / discovered pages — a rough sense of remaining work.
        let total = crawl.pages + crawl.queued + crawl.processing;
        let fraction = if total == 0 {
            0.0
        } else {
            crawl.pages as f32 / total as f32
        };
        ui.add(
            egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                .show_percentage()
                .desired_height(14.0),
        );
        ui.add_space(4.0);
        ui.label(format!(
            "{} pages • {} failed • queued {} • processing {} • {} robots-blocked • {} rendered • {} not modified • {:.0}s elapsed{}",
            crawl.pages,
            crawl.failed,
            crawl.queued,
            crawl.processing,
            crawl.robots_blocked,
            crawl.js_renders,
            crawl.not_modified,
            crawl.started.elapsed().as_secs_f32(),
            if crawl.stop_requested { " • stopping…" } else { "" }
        ));
        ui.separator();
        ui.label(RichText::new("Latest pages").strong());

        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for page in &crawl.recent {
                    let color = if page.failed { ERR_RED } else { OK_GREEN };
                    let suffix = match (page.js_rendered, page.not_modified) {
                        (true, _) => " [js]",
                        (_, true) => " [304]",
                        _ => "",
                    };
                    ui.monospace(
                        RichText::new(format!(
                            "[{}] {} ({} links){}",
                            page.status, page.url, page.links, suffix
                        ))
                        .color(color),
                    );
                }
            });
    }
    if stop_clicked {
        app.stop_crawl();
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

fn settings_window(app: &mut RustySeoApp, ctx: &egui::Context) {
    if !app.settings_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Settings")
        .open(&mut open)
        .resizable(true)
        .default_width(560.0)
        .show(ctx, |ui| {
            let draft = &mut app.settings_draft;

            ui.heading("Crawler defaults");
            ui.add_space(2.0);
            egui::Grid::new("settings_crawler")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Max pages");
                    ui.add(egui::DragValue::new(&mut draft.crawler.max_pages).range(1..=1_000_000));
                    ui.end_row();

                    ui.label("Concurrency");
                    ui.add(egui::DragValue::new(&mut draft.crawler.concurrency).range(1..=64));
                    ui.end_row();

                    ui.label("JavaScript rendering");
                    ui.checkbox(&mut draft.crawler.enable_javascript, "render with Chrome");
                    ui.end_row();

                    ui.label("JS fallback");
                    ui.checkbox(
                        &mut draft.crawler.js_fallback,
                        "render only pages that look client-side",
                    );
                    ui.end_row();

                    ui.label("Stay on domain");
                    ui.checkbox(&mut draft.crawler.stay_on_domain, "");
                    ui.end_row();

                    ui.label("Follow redirects");
                    ui.checkbox(&mut draft.crawler.follow_redirects, "");
                    ui.end_row();

                    ui.label("Respect robots.txt");
                    ui.checkbox(&mut draft.crawler.respect_robots, "skip disallowed URLs");
                    ui.end_row();

                    ui.label("Respect Crawl-delay");
                    ui.checkbox(&mut draft.crawler.respect_crawl_delay, "slower, politer");
                    ui.end_row();

                    ui.label("Respect nofollow");
                    ui.checkbox(
                        &mut draft.crawler.respect_nofollow,
                        "do not follow rel=nofollow links",
                    );
                    ui.end_row();

                    ui.label("Respect noindex");
                    ui.checkbox(
                        &mut draft.crawler.respect_noindex,
                        "do not follow links from noindex pages",
                    );
                    ui.end_row();
                });

            ui.add_space(8.0);
            ui.heading("Connectors");
            ui.add_space(2.0);
            egui::Grid::new("settings_connectors")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    ui.label("PageSpeed Insights key");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.connectors.pagespeed.api_key)
                            .password(true)
                            .desired_width(340.0),
                    );
                    ui.end_row();

                    ui.label("PageSpeed enabled");
                    ui.checkbox(
                        &mut draft.connectors.pagespeed.status,
                        "Collect CWV on crawls (2 API calls/page — slow)",
                    );
                    ui.end_row();

                    ui.label("Gemini API key");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.connectors.gemini.api_key)
                            .password(true)
                            .desired_width(340.0),
                    );
                    ui.end_row();

                    ui.label("Gemini model");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.connectors.gemini.model)
                            .desired_width(340.0),
                    );
                    ui.end_row();

                    ui.label("OpenAI API key");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.connectors.openai.api_key)
                            .password(true)
                            .desired_width(340.0),
                    );
                    ui.end_row();

                    ui.label("OpenAI model");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.connectors.openai.model)
                            .desired_width(340.0),
                    );
                    ui.end_row();
                });

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Save").strong()).clicked() {
                    app.save_settings_draft();
                }
                if ui.button("Reload from disk").clicked() {
                    app.open_settings();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("Paths").strong().small());
            ui.label(
                RichText::new(format!("settings: {}", rustyseo_core::models::AppSettings::path().display()))
                    .weak()
                    .small(),
            );
            ui.label(
                RichText::new(format!("database: {}", rustyseo_core::db::get_db_path().display()))
                    .weak()
                    .small(),
            );
            ui.label(
                RichText::new(format!("log: {}", app.log_path.display()))
                    .weak()
                    .small(),
            );
            if let Ok(exports) = export::exports_dir() {
                ui.label(
                    RichText::new(format!("exports: {}", exports.display()))
                        .weak()
                        .small(),
                );
            }
        });
    app.settings_open = open;
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn short_url(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

fn format_started(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| raw.get(..16).unwrap_or(raw).to_string())
}

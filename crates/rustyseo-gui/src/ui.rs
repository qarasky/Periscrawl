//! Rendering: panels, session list, crawl form, live view and data tables.

use eframe::egui::{self, Align, Color32, Layout, RichText};
use egui_extras::{Column, TableBuilder};

use rustyseo_core::export::Sheet;

use crate::app::{ExportKind, LiveCrawl, RustySeoApp};

const ACCENT: Color32 = Color32::from_rgb(90, 150, 255);

pub fn draw(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    top_panel(app, ui);
    left_panel(app, ui);
    bottom_panel(app, ui);
    central(app, ui);
}

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
                        "{} pages fetched • {} failed",
                        session.pages_fetched, session.pages_failed
                    ));
                } else {
                    ui.label(RichText::new(format!("Session #{id}")).strong());
                }
            } else {
                ui.label(RichText::new("No session selected").weak());
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let exporting_enabled = !app.sheets.is_empty() && app.crawl.is_none();
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
                    let robots = crawl
                        .stats
                        .as_ref()
                        .map(|s| s.robots_blocked())
                        .unwrap_or(0);
                    ui.label(format!(
                        "Crawling • {} pages • {} failed • queued {} • processing {} • {} robots-blocked • {:.0}s",
                        crawl.pages,
                        crawl.failed,
                        crawl.queued,
                        crawl.processing,
                        robots,
                        crawl.started.elapsed().as_secs_f32()
                    ));
                }
                None => {
                    ui.label(&app.status);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
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
            let crawl = app.crawl.as_ref().expect("checked above");
            live_view(ui, crawl);
        } else if app.loading_session {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading session…");
            });
        } else if app.sheets.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Start a crawl or pick a session from the left.")
                        .weak()
                        .size(15.0),
                );
            });
        } else {
            tabs_and_table(app, ui);
        }
    });
}

fn tabs_and_table(app: &mut RustySeoApp, ui: &mut egui::Ui) {
    let mut new_tab: Option<usize> = None;
    ui.horizontal_wrapped(|ui| {
        for (index, sheet) in app.sheets.iter().enumerate() {
            let label = format!("{} ({})", sheet.name, sheet.rows.len());
            let selected = app.active_tab == index;
            if ui.selectable_label(selected, label).clicked() {
                new_tab = Some(index);
            }
        }
    });
    if let Some(index) = new_tab {
        if index != app.active_tab {
            app.active_tab = index;
            app.filter.clear();
        }
    }

    ui.add_space(4.0);
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
    });
    ui.separator();

    if let Some(sheet) = app.sheets.get(app.active_tab) {
        sheet_table(ui, sheet, &app.filter);
    }
}

/// Virtualised table for one sheet, with a case-insensitive text filter.
fn sheet_table(ui: &mut egui::Ui, sheet: &Sheet, filter: &str) {
    let needle = filter.trim().to_lowercase();
    let indices: Vec<usize> = if needle.is_empty() {
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

    let text_height = 18.0;
    let mut table = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(Layout::left_to_right(Align::Center))
        .min_scrolled_height(0.0);

    let column_count = sheet.header.len().max(1);
    for column_index in 0..column_count {
        let column = if column_index + 1 == column_count {
            Column::remainder().at_least(120.0)
        } else {
            Column::auto().at_least(60.0).clip(true)
        };
        table = table.column(column);
    }

    table
        .header(22.0, |mut header| {
            for title in &sheet.header {
                header.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(text_height, indices.len(), |mut row| {
                let source_index = indices[row.index()];
                let data_row = &sheet.rows[source_index];
                for (column_index, cell) in data_row.iter().enumerate() {
                    if column_index >= column_count {
                        break;
                    }
                    row.col(|ui| {
                        ui.label(cell);
                    });
                }
            });
        });
}

fn live_view(ui: &mut egui::Ui, crawl: &LiveCrawl) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.heading("Crawling");
        ui.label(RichText::new(&crawl.url).weak());
    });
    ui.add_space(4.0);

    let fraction = (crawl.pages as f32 / crawl.max_pages.max(1) as f32).clamp(0.0, 1.0);
    ui.add(
        egui::ProgressBar::new(fraction)
            .show_percentage()
            .desired_height(14.0),
    );
    ui.add_space(4.0);

    let robots = crawl
        .stats
        .as_ref()
        .map(|s| s.robots_blocked())
        .unwrap_or(0);
    ui.label(format!(
        "{} pages • {} failed • queued {} • processing {} • robots-blocked {} • {:.0}s elapsed",
        crawl.pages,
        crawl.failed,
        crawl.queued,
        crawl.processing,
        robots,
        crawl.started.elapsed().as_secs_f32()
    ));
    ui.separator();
    ui.label(RichText::new("Latest pages").strong());

    egui::ScrollArea::vertical()
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for page in &crawl.recent {
                let color = if page.failed {
                    Color32::from_rgb(240, 110, 110)
                } else {
                    Color32::from_rgb(120, 215, 140)
                };
                ui.monospace(
                    RichText::new(format!(
                        "[{}] {} ({} links)",
                        page.status, page.url, page.links
                    ))
                    .color(color),
                );
            }
        });
}

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

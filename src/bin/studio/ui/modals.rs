use eframe::egui::{self, Color32, Context, RichText};
use std::path::PathBuf;
use std::sync::mpsc::channel;

use crate::types::{IccProfileEntry, IccProfileFilter, IccProfileSource};
use crate::icc::extract_file_date;
use crate::processing::submit_print_jobs_sync;
use crate::App;

impl App {
    pub(crate) fn show_printer_props(&mut self, ctx: &Context) {
        let Some(caps) = self.state.caps.clone() else { self.state.show_props = false; return };
        let prev_page_size = self.state.selected_page_size_idx;

        let num_extra = caps.extra_options.len();
        let base_height = if num_extra > 0 { 280.0 } else { 180.0 };
        let extra_height = (num_extra as f32 * 28.0).min(350.0);
        let content_height = base_height + extra_height;
        
        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.35).clamp(340.0, 520.0);
        let height = (screen.height() * 0.7).clamp(220.0, content_height.max(400.0));

        egui::Window::new(format!("Properties — {}", caps.name))
            .collapsible(false)
            .resizable(true)
            .default_size([width, height])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let combo_width = ui.available_width() * 0.55;
                
                egui::Grid::new("props_grid")
                    .num_columns(2)
                    .spacing([ui.available_width() * 0.03, 6.0])
                    .striped(true)
                    .show(ui, |ui| {
                        // ── Media Type ───────────────────────────────────
                        ui.label("Media Type:");
                        if caps.media_types.is_empty() {
                            ui.label(RichText::new("—").weak());
                        } else {
                            egui::ComboBox::from_id_salt("props_media")
                                .width(combo_width)
                                .selected_text(
                                    caps.media_types.get(self.state.props_media_idx)
                                        .map(|s| s.as_str()).unwrap_or("—")
                                )
                                .show_ui(ui, |ui| {
                                    for (i, m) in caps.media_types.iter().enumerate() {
                                        ui.selectable_value(&mut self.state.props_media_idx, i, m);
                                    }
                                });
                        }
                        ui.end_row();

                        // ── Paper Size ───────────────────────────────────
                        ui.label("Paper Size:");
                        if caps.page_sizes.is_empty() {
                            ui.label(RichText::new("—").weak());
                        } else {
                            let ps_label = caps.page_sizes
                                .get(self.state.selected_page_size_idx)
                                .map(|p| p.label.as_str()).unwrap_or("—");
                            egui::ComboBox::from_id_salt("props_paper")
                                .width(combo_width)
                                .selected_text(ps_label)
                                .show_ui(ui, |ui| {
                                    for i in 0..caps.page_sizes.len() {
                                        let label = caps.page_sizes[i].label.clone();
                                        ui.selectable_value(
                                            &mut self.state.selected_page_size_idx, i, label
                                        );
                                    }
                                });
                        }
                        ui.end_row();

                        // ── Input Slot / Source Tray ─────────────────────
                        if !caps.input_slots.is_empty() {
                            ui.label("Input Slot:");
                            egui::ComboBox::from_id_salt("props_slot")
                                .width(combo_width)
                                .selected_text(
                                    caps.input_slots.get(self.state.props_slot_idx)
                                        .map(|s| s.as_str()).unwrap_or("—")
                                )
                                .show_ui(ui, |ui| {
                                    for (i, s) in caps.input_slots.iter().enumerate() {
                                        ui.selectable_value(&mut self.state.props_slot_idx, i, s);
                                    }
                                });
                            ui.end_row();
                        }
                    });

                // ── Additional CUPS options ───────────────────────────────────
                if !caps.extra_options.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new("Advanced / Printer-specific").small().weak());
                    ui.add_space(4.0);

                    let remaining = ui.available_height() - 40.0;
                    egui::ScrollArea::vertical()
                        .id_salt("props_extra_scroll")
                        .max_height(remaining.max(100.0))
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            let extra_combo_width = ui.available_width() * 0.50;
                            egui::Grid::new("props_extra_grid")
                                .num_columns(2)
                                .spacing([ui.available_width() * 0.03, 6.0])
                                .striped(true)
                                .show(ui, |ui| {
                                    for opt in &caps.extra_options {
                                        let label_text = if opt.label.len() > 35 {
                                            format!("{:.32}...", opt.label)
                                        } else {
                                            opt.label.clone()
                                        };
                                        ui.label(label_text);
                                        
                                        let idx = self.state.extra_option_indices
                                            .entry(opt.key.clone())
                                            .or_insert(opt.default_idx);
                                        let sel_text = opt.choices.get(*idx)
                                            .map(|(_, l)| {
                                                if l.len() > 30 {
                                                    format!("{:.27}...", l)
                                                } else {
                                                    l.clone()
                                                }
                                            })
                                            .unwrap_or("—".to_string());
                                        
                                        egui::ComboBox::from_id_salt(
                                            format!("props_extra_{}", opt.key)
                                        )
                                        .width(extra_combo_width)
                                        .selected_text(sel_text)
                                        .show_ui(ui, |ui| {
                                            for (i, (_, cl)) in opt.choices.iter().enumerate() {
                                                let display_cl = if cl.len() > 35 {
                                                    format!("{:.32}...", cl)
                                                } else {
                                                    cl.clone()
                                                };
                                                ui.selectable_value(idx, i, display_cl);
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                        });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.state.show_props = false;
                        }
                    });
                });
            });

        if self.state.selected_page_size_idx != prev_page_size {
            self.relayout_queue();
        }
    }

    pub(crate) fn show_print_confirm(&mut self, ctx: &Context) {
        let Some(caps) = self.state.caps.clone() else { self.state.show_print_confirm = false; return };
        let printer_name = self.state.printers.get(self.state.printer_idx).map(|p| p.name.clone());
        let Some(printer_name) = printer_name else { self.state.show_print_confirm = false; return };
        if self.state.pending_print_paths.is_empty() { self.state.show_print_confirm = false; return };
        let temp_paths = self.state.pending_print_paths.clone();

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.30).clamp(320.0, 480.0);

        egui::Window::new(RichText::new("Confirm Print").strong().color(Color32::WHITE))
            .collapsible(false)
            .resizable(false)
            .fixed_size([width, 0.0])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(4.0);

                ui.label(RichText::new("Printer:").small().weak());
                ui.label(&printer_name);
                ui.add_space(4.0);

                let paper = caps.page_sizes.get(self.state.selected_page_size_idx)
                    .map(|p| p.label.as_str())
                    .unwrap_or("—");
                ui.label(RichText::new("Paper:").small().weak());
                ui.label(paper);
                ui.add_space(4.0);
                ui.label(RichText::new("Pages:").small().weak());
                ui.label(format!("{}", temp_paths.len()));
                ui.add_space(4.0);

                if !caps.media_types.is_empty() {
                    let media = caps.media_types.get(self.state.props_media_idx)
                        .map(|m| m.as_str())
                        .unwrap_or("—");
                    ui.label(RichText::new("Media Type:").small().weak());
                    ui.label(media);
                    ui.add_space(4.0);
                }

                if !caps.input_slots.is_empty() {
                    let slot = caps.input_slots.get(self.state.props_slot_idx)
                        .map(|s| s.as_str())
                        .unwrap_or("—");
                    ui.label(RichText::new("Input Slot:").small().weak());
                    ui.label(slot);
                    ui.add_space(4.0);
                }

                if !caps.extra_options.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Additional Options:").small().weak());
                    egui::ScrollArea::vertical()
                        .max_height(80.0)
                        .show(ui, |ui| {
                            for opt in &caps.extra_options {
                                if let Some(&idx) = self.state.extra_option_indices.get(&opt.key) {
                                    if let Some((_, label)) = opt.choices.get(idx) {
                                        ui.label(format!("• {}: {}", opt.label, label));
                                    }
                                }
                            }
                        });
                }

                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_print_confirm = false;
                            for p in &temp_paths {
                                let _ = std::fs::remove_file(p);
                            }
                            self.state.pending_print_paths.clear();
                        }
                        
                        let print_btn = ui.add(egui::Button::new(
                            RichText::new("Print").strong().color(Color32::WHITE)
                        ));
                        if print_btn.clicked() {
                            self.state.show_print_confirm = false;
                            let temp_paths_clone = temp_paths.clone();
                            let (tx, rx) = channel::<Result<(), String>>();
                            let (log_tx, log_rx) = channel::<String>();
                            self.state.print_rx = Some(rx);
                            self.state.print_log_rx = Some(log_rx);
                            self.state.log.push("Submitting print jobs...".into());
                            let caps = self.state.caps.clone();
                            let printer_idx = self.state.printer_idx;
                            let printers = self.state.printers.clone();
                            let selected_page_size_idx = self.state.selected_page_size_idx;
                            let props_media_idx = self.state.props_media_idx;
                            let props_slot_idx = self.state.props_slot_idx;
                            let extra_option_indices = self.state.extra_option_indices.clone();
                            std::thread::spawn(move || {
                                let result = submit_print_jobs_sync(
                                    &temp_paths_clone,
                                    caps,
                                    printer_idx,
                                    &printers,
                                    selected_page_size_idx,
                                    props_media_idx,
                                    props_slot_idx,
                                    &extra_option_indices,
                                    &log_tx,
                                );
                                let _ = tx.send(result);
                            });
                            self.state.pending_print_paths.clear();
                        }
                    });
                });
            });
    }

    pub(crate) fn show_icc_picker(&mut self, ctx: &Context) {
        if self.state.icc_auto_switch_pending {
            self.state.icc_profile_filter = self.state.saved_icc_filter_for_restore;
            self.state.icc_auto_switch_pending = false;
        }

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.50).clamp(600.0, 900.0);
        let height = (screen.height() * 0.70).clamp(500.0, 700.0);

        egui::Window::new("Select ICC Profile")
            .collapsible(false)
            .resizable(false)
            .min_size([width, height])
            .max_size([width, height])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);

                // Profile filter radio buttons
                ui.horizontal(|ui| {
                    ui.label("Show:");
                    let previous_filter = self.state.icc_profile_filter;
                    ui.radio_value(&mut self.state.icc_profile_filter, IccProfileFilter::All, "All profiles");
                    ui.radio_value(&mut self.state.icc_profile_filter, IccProfileFilter::System, "System level profiles");
                    ui.radio_value(&mut self.state.icc_profile_filter, IccProfileFilter::User, "User profiles");
                    if previous_filter != self.state.icc_profile_filter {
                        use crate::app::save_settings;
                        use crate::types::{Engine, Intent, IccProfileFilter};
                        
                        let engine_str = match self.state.engine {
                            Engine::Lanczos3 => "lanczos3",
                            Engine::Iterative => "iterative",
                            Engine::RobidouxEwa => "robidoux",
                            Engine::Mks => "mks",
                        };
                        let intent_str = match self.state.intent {
                            Intent::Perceptual => "perceptual",
                            Intent::Saturation => "saturation",
                            Intent::Relative => "relative",
                        };
                        let icc_filter_str = match self.state.icc_profile_filter {
                            IccProfileFilter::All => "all",
                            IccProfileFilter::System => "system",
                            IccProfileFilter::User => "user",
                        };
                        let printer_name = self.state.printers.get(self.state.printer_idx).map(|p| p.name.clone());
                        save_settings(&crate::types::Settings {
                            current_dir: Some(self.state.current_dir.to_string_lossy().into_owned()),
                            printer_name,
                            page_size_name: self.state.caps.as_ref()
                                .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
                                .map(|ps| ps.name.clone()),
                            engine: Some(engine_str.into()),
                            sharpen: Some(self.state.sharpen),
                            depth16: Some(self.state.depth16),
                            target_dpi: Some(self.state.target_dpi),
                            output_icc: self.state.output_icc.as_ref().map(|e| e.path.to_string_lossy().into_owned()),
                            intent: Some(intent_str.into()),
                            bpc: Some(self.state.bpc),
                            output_dir: Some(self.state.output_dir.to_string_lossy().into_owned()),
                            user_border_in: Some(self.state.user_border_in),
                            icc_filter: Some(icc_filter_str.into()),
                        });
                    }
                });

                ui.add_space(8.0);

                // Search/filter input
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.state.icc_filter_text)
                            .hint_text("Search profiles...")
                            .desired_width(f32::INFINITY)
                    );
                });

                ui.add_space(8.0);

                // Profile list
                let filter_lower = self.state.icc_filter_text.to_lowercase();
                let filtered: Vec<&IccProfileEntry> = self.state.icc_profiles
                    .iter()
                    .filter(|p| {
                        let location_match = match self.state.icc_profile_filter {
                            IccProfileFilter::All => true,
                            IccProfileFilter::System => p.source == IccProfileSource::System,
                            IccProfileFilter::User => p.source == IccProfileSource::User,
                        };

                        let text_match = filter_lower.is_empty()
                            || p.description.to_lowercase().contains(&filter_lower)
                            || p.file_name().to_lowercase().contains(&filter_lower)
                            || p.location().to_lowercase().contains(&filter_lower);

                        location_match && text_match
                    })
                    .collect();

                let mut selected_path: Option<PathBuf> = None;
                let scroll_height = height - 200.0;

                egui::ScrollArea::vertical()
                    .max_height(scroll_height)
                    .auto_shrink([false; 2])
                    .show(ui, |ui| {
                        if filtered.is_empty() {
                            ui.centered_and_justified(|ui| {
                                if self.state.icc_profiles.is_empty() {
                                    ui.label("No ICC profiles found in standard directories.");
                                } else {
                                    ui.label("No profiles match your filter.");
                                }
                            });
                        } else {
                            use egui_extras::{TableBuilder, Column};

                            let table_width = ui.available_width();
                            let desc_width = table_width * 0.50;
                            let loc_width = table_width * 0.35;
                            let file_width = table_width * 0.15;

                            TableBuilder::new(ui)
                                .striped(true)
                                .resizable(true)
                                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                .column(Column::initial(desc_width))
                                .column(Column::initial(loc_width))
                                .column(Column::initial(file_width))
                                .column(Column::remainder())
                                .header(20.0, |mut header| {
                                    header.col(|ui| { ui.strong("Description"); });
                                    header.col(|ui| { ui.strong("Location"); });
                                    header.col(|ui| { ui.strong("Filename"); });
                                    header.col(|ui| { ui.strong("Date"); });
                                })
                                .body(|mut body| {
                                    for profile in filtered {
                                        body.row(22.0, |mut row| {
                                            row.col(|ui| {
                                                if ui.selectable_label(false, &profile.description).clicked() {
                                                    selected_path = Some(profile.path.clone());
                                                }
                                            });
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(
                                                    RichText::new(&profile.location())
                                                        .weak()
                                                ).truncate());
                                            });
                                            row.col(|ui| {
                                                ui.add(egui::Label::new(
                                                    RichText::new(profile.file_name())
                                                        .monospace()
                                                        .weak()
                                                ).truncate());
                                            });
                                            row.col(|ui| {
                                                ui.label(RichText::new(&profile.date).weak());
                                            });
                                        });
                                    }
                                });
                        }
                    });

                if let Some(path) = selected_path {
                    if let Some(entry) = self.state.icc_profiles.iter().find(|e| e.path == path) {
                        self.state.output_icc = Some(entry.clone());
                    } else {
                        let date = extract_file_date(&path);
                        let description = path.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string();
                        self.state.output_icc = Some(IccProfileEntry { path, description, date, source: IccProfileSource::User });
                    }
                    self.state.show_icc_picker = false;
                    self.mark_preview_dirty();
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Browse for File...").clicked() {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("ICC Profile", &["icc", "icm"])
                            .pick_file()
                        {
                            let date = extract_file_date(&p);
                            let description = if let Ok(bytes) = std::fs::read(&p) {
                                if let Ok(profile) = lcms2::Profile::new_icc(&bytes) {
                                    profile.info(lcms2::InfoType::Description, lcms2::Locale::none())
                                        .unwrap_or_else(|| p.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string())
                                } else {
                                    p.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string()
                                }
                            } else {
                                p.file_name().and_then(|n| n.to_str()).unwrap_or("Unknown").to_string()
                            };
                            self.state.output_icc = Some(IccProfileEntry { path: p, description, date, source: IccProfileSource::User });
                            self.state.show_icc_picker = false;
                            self.mark_preview_dirty();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.state.show_icc_picker = false;
                            self.state.icc_scan_rx = None;
                            self.state.icc_scan_pending = false;
                        }
                    });
                });
            });
    }
}

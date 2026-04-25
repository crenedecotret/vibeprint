use eframe::egui::{self, Color32, Context, Pos2, Rect, RichText, Sense, Vec2};
use std::path::PathBuf;
use std::sync::mpsc::channel;

use crate::icc::{apply_preview_transform, extract_file_date};
use crate::processing::submit_print_jobs_sync;
use crate::types::{CustomSizeMode, IccProfileEntry, IccProfileFilter, IccProfileSource};
use crate::App;

impl App {
    pub(crate) fn show_printer_props(&mut self, ctx: &Context) {
        let Some(caps) = self.state.caps.clone() else {
            self.state.show_props = false;
            return;
        };
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
                                    caps.media_types
                                        .get(self.state.props_media_idx)
                                        .map(|s| s.1.as_str())
                                        .unwrap_or("—"),
                                )
                                .show_ui(ui, |ui| {
                                    for (i, (_, label)) in caps.media_types.iter().enumerate() {
                                        ui.selectable_value(&mut self.state.props_media_idx, i, label);
                                    }
                                });
                        }
                        ui.end_row();

                        // ── Paper Size ───────────────────────────────────
                        ui.label("Paper Size:");
                        if caps.page_sizes.is_empty() {
                            ui.label(RichText::new("—").weak());
                        } else {
                            let ps_label = caps
                                .page_sizes
                                .get(self.state.selected_page_size_idx)
                                .map(|p| p.label.as_str())
                                .unwrap_or("—");
                            egui::ComboBox::from_id_salt("props_paper")
                                .width(combo_width)
                                .selected_text(ps_label)
                                .show_ui(ui, |ui| {
                                    for i in 0..caps.page_sizes.len() {
                                        let label = caps.page_sizes[i].label.clone();
                                        ui.selectable_value(
                                            &mut self.state.selected_page_size_idx,
                                            i,
                                            label,
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
                                    caps.input_slots
                                        .get(self.state.props_slot_idx)
                                        .map(|s| s.1.as_str())
                                        .unwrap_or("—"),
                                )
                                .show_ui(ui, |ui| {
                                    for (i, (_, label)) in caps.input_slots.iter().enumerate() {
                                        ui.selectable_value(&mut self.state.props_slot_idx, i, label);
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

                                        let idx = self
                                            .state
                                            .extra_option_indices
                                            .entry(opt.key.clone())
                                            .or_insert(opt.default_idx);
                                        let sel_text = opt
                                            .choices
                                            .get(*idx)
                                            .map(|(_, l)| {
                                                if l.len() > 30 {
                                                    format!("{:.27}...", l)
                                                } else {
                                                    l.clone()
                                                }
                                            })
                                            .unwrap_or("—".to_string());

                                        egui::ComboBox::from_id_salt(format!(
                                            "props_extra_{}",
                                            opt.key
                                        ))
                                        .width(extra_combo_width)
                                        .selected_text(sel_text)
                                        .show_ui(
                                            ui,
                                            |ui| {
                                                for (i, (_, cl)) in opt.choices.iter().enumerate() {
                                                    let display_cl = if cl.len() > 35 {
                                                        format!("{:.32}...", cl)
                                                    } else {
                                                        cl.clone()
                                                    };
                                                    ui.selectable_value(idx, i, display_cl);
                                                }
                                            },
                                        );
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
        let Some(caps) = self.state.caps.clone() else {
            self.state.show_print_confirm = false;
            return;
        };
        let printer_name = self
            .state
            .printers
            .get(self.state.printer_idx)
            .map(|p| p.name.clone());
        let Some(printer_name) = printer_name else {
            self.state.show_print_confirm = false;
            return;
        };
        if self.state.pending_print_paths.is_empty() {
            self.state.show_print_confirm = false;
            return;
        };
        let temp_paths = self.state.pending_print_paths.clone();

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.30).clamp(320.0, 480.0);
        let scale = (screen.height() / 1080.0).clamp(1.0, 1.5);
        let btn_size = [90.0 * scale, 30.0 * scale];

        egui::Window::new(
            RichText::new("Confirm Print")
                .strong()
                .color(Color32::WHITE),
        )
        .collapsible(false)
        .resizable(false)
        .fixed_size([width, 0.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.add_space(4.0);

            ui.label(RichText::new("Printer:").weak());
            ui.label(&printer_name);
            ui.add_space(4.0);

            let paper = caps
                .page_sizes
                .get(self.state.selected_page_size_idx)
                .map(|p| p.label.as_str())
                .unwrap_or("—");
            ui.label(RichText::new("Paper:").weak());
            ui.label(paper);
            ui.add_space(4.0);
            ui.label(RichText::new("Pages:").weak());
            ui.label(format!("{}", temp_paths.len()));
            ui.add_space(4.0);

            if !caps.media_types.is_empty() {
                let media = caps
                    .media_types
                    .get(self.state.props_media_idx)
                    .map(|m| m.1.as_str())
                    .unwrap_or("—");
                ui.label(RichText::new("Media Type:").weak());
                ui.label(media);
                ui.add_space(4.0);
            }

            if !caps.input_slots.is_empty() {
                let slot = caps
                    .input_slots
                    .get(self.state.props_slot_idx)
                    .map(|s| s.1.as_str())
                    .unwrap_or("—");
                ui.label(RichText::new("Input Slot:").weak());
                ui.label(slot);
                ui.add_space(4.0);
            }

            if !caps.extra_options.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new("Additional Options:").weak());
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

            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("lpr command:").weak());
            {
                let mut lpr_parts = vec![
                    format!("lpr -P '{}'", printer_name),
                    "-o print-scaling=none".to_string(),
                ];
                if let Some(ps) = caps.page_sizes.get(self.state.selected_page_size_idx) {
                    lpr_parts.push(format!("-o media={}", ps.name));
                }
                if let Some((key, _)) = caps.media_types.get(self.state.props_media_idx) {
                    lpr_parts.push(format!("-o media-type={}", key));
                }
                if let Some((key, _)) = caps.input_slots.get(self.state.props_slot_idx) {
                    lpr_parts.push(format!("-o media-source={}", key));
                }
                for opt in &caps.extra_options {
                    if let Some(&idx) = self.state.extra_option_indices.get(&opt.key) {
                        if let Some((choice_key, _)) = opt.choices.get(idx) {
                            lpr_parts.push(format!("-o {}={}", opt.key, choice_key));
                        }
                    }
                }
                lpr_parts.push("<file.pdf>".to_string());
                let cmd = lpr_parts.join(" ");
                egui::ScrollArea::horizontal()
                    .id_salt("lpr_cmd_scroll")
                    .show(ui, |ui| {
                        ui.label(RichText::new(&cmd).monospace().small().weak());
                    });
            }

            ui.add_space(12.0);

            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let print_btn = ui.add_sized(
                        btn_size,
                        egui::Button::new(RichText::new("Print").strong().color(Color32::WHITE)),
                    );
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

                    if ui
                        .add_sized(btn_size, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        self.state.show_print_confirm = false;
                        for p in &temp_paths {
                            let _ = std::fs::remove_file(p);
                        }
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
        let scale = (screen.height() / 1080.0).clamp(1.0, 1.5);
        let btn_size = [90.0 * scale, 30.0 * scale];

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
                    ui.radio_value(
                        &mut self.state.icc_profile_filter,
                        IccProfileFilter::All,
                        "All profiles",
                    );
                    ui.radio_value(
                        &mut self.state.icc_profile_filter,
                        IccProfileFilter::System,
                        "System level profiles",
                    );
                    ui.radio_value(
                        &mut self.state.icc_profile_filter,
                        IccProfileFilter::User,
                        "User profiles",
                    );
                    if previous_filter != self.state.icc_profile_filter {
                        use crate::app::save_settings;
                        use crate::types::{Engine, IccProfileFilter, Intent};

                        let engine_str = match self.state.engine {
                            Engine::Lanczos3 => "lanczos3",
                            Engine::Iterative => "iterative",
                            Engine::MitchellEwa => "mitchell",
                            Engine::MitchellEwaSharp => "mitchell-sharp",
                            Engine::Mks => "catmullrom",
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
                        let printer_name = self
                            .state
                            .printers
                            .get(self.state.printer_idx)
                            .map(|p| p.name.clone());
                        save_settings(&crate::types::Settings {
                            current_dir: Some(
                                self.state.current_dir.to_string_lossy().into_owned(),
                            ),
                            printer_name,
                            page_size_name: self
                                .state
                                .caps
                                .as_ref()
                                .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
                                .map(|ps| ps.name.clone()),
                            engine: Some(engine_str.into()),
                            sharpen: Some(self.state.sharpen),
                            depth16: Some(self.state.depth16),
                            target_dpi: Some(self.state.target_dpi),
                            output_icc: self
                                .state
                                .output_icc
                                .as_ref()
                                .map(|e| e.path.to_string_lossy().into_owned()),
                            intent: Some(intent_str.into()),
                            bpc: Some(self.state.bpc),
                            output_dir: Some(self.state.output_dir.to_string_lossy().into_owned()),
                            user_border_in: Some(self.state.user_border_in),
                            icc_filter: Some(icc_filter_str.into()),
                            show_log: Some(self.state.show_log),
                            extra_option_indices: Some(self.state.extra_option_indices.clone()),
                            media_type_key: self.state.caps.as_ref()
                                .and_then(|c| c.media_types.get(self.state.props_media_idx))
                                .map(|(k, _)| k.clone()),
                            input_slot_key: self.state.caps.as_ref()
                                .and_then(|c| c.input_slots.get(self.state.props_slot_idx))
                                .map(|(k, _)| k.clone()),
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
                            .desired_width(f32::INFINITY),
                    );
                });

                ui.add_space(8.0);

                // Profile list
                let filter_lower = self.state.icc_filter_text.to_lowercase();
                let filtered: Vec<&IccProfileEntry> = self
                    .state
                    .icc_profiles
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
                            use egui_extras::{Column, TableBuilder};

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
                                    header.col(|ui| {
                                        ui.strong("Description");
                                    });
                                    header.col(|ui| {
                                        ui.strong("Location");
                                    });
                                    header.col(|ui| {
                                        ui.strong("Filename");
                                    });
                                    header.col(|ui| {
                                        ui.strong("Date");
                                    });
                                })
                                .body(|mut body| {
                                    for profile in filtered {
                                        body.row(22.0, |mut row| {
                                            row.col(|ui| {
                                                if ui
                                                    .selectable_label(false, &profile.description)
                                                    .clicked()
                                                {
                                                    selected_path = Some(profile.path.clone());
                                                }
                                            });
                                            row.col(|ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(&profile.location()).weak(),
                                                    )
                                                    .truncate(),
                                                );
                                            });
                                            row.col(|ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(profile.file_name())
                                                            .monospace()
                                                            .weak(),
                                                    )
                                                    .truncate(),
                                                );
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
                        let description = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Unknown")
                            .to_string();
                        self.state.output_icc = Some(IccProfileEntry {
                            path,
                            description,
                            date,
                            source: IccProfileSource::User,
                        });
                    }
                    self.state.show_icc_picker = false;
                    self.mark_preview_dirty();
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui
                        .add_sized(btn_size, egui::Button::new("Browse for File..."))
                        .clicked()
                    {
                        if let Some(p) = rfd::FileDialog::new()
                            .add_filter("ICC Profile", &["icc", "icm"])
                            .pick_file()
                        {
                            let date = extract_file_date(&p);
                            let description = if let Ok(bytes) = std::fs::read(&p) {
                                if let Ok(profile) = lcms2::Profile::new_icc(&bytes) {
                                    profile
                                        .info(lcms2::InfoType::Description, lcms2::Locale::none())
                                        .unwrap_or_else(|| {
                                            p.file_name()
                                                .and_then(|n| n.to_str())
                                                .unwrap_or("Unknown")
                                                .to_string()
                                        })
                                } else {
                                    p.file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("Unknown")
                                        .to_string()
                                }
                            } else {
                                p.file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("Unknown")
                                    .to_string()
                            };
                            self.state.output_icc = Some(IccProfileEntry {
                                path: p,
                                description,
                                date,
                                source: IccProfileSource::User,
                            });
                            self.state.show_icc_picker = false;
                            self.mark_preview_dirty();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add_sized(btn_size, egui::Button::new("Close")).clicked() {
                            self.state.show_icc_picker = false;
                            self.state.icc_scan_rx = None;
                            self.state.icc_scan_pending = false;
                        }
                    });
                });
            });
    }

    pub(crate) fn show_crop_editor(&mut self, ctx: &Context) {
        let Some(queue_id) = self.state.crop_editor_queue_id else {
            self.state.show_crop_editor = false;
            return;
        };

        let Some(q) = self
            .state
            .queue
            .iter()
            .find(|qi| qi.id == queue_id)
            .cloned()
        else {
            self.state.show_crop_editor = false;
            return;
        };

        let Some(full_image) = self.state.full_images.get(&q.filepath).cloned() else {
            self.state.show_crop_editor = false;
            return;
        };

        // Extract data needed inside closure before borrowing
        let q_filepath_str = q.filepath.to_string_lossy().to_string();
        let q_fit_to_page = q.fit_to_page;
        let q_size = q.size;
        let q_src_size_px = q.src_size_px;
        let q_border_type = q.border_type;
        let q_border_width_pt = q.border_width_pt;

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.90).clamp(800.0, 1400.0);
        let height = (screen.height() * 0.90).clamp(700.0, 1000.0);
        let scale = (screen.height() / 1080.0).clamp(1.0, 1.5);
        let btn_size = [90.0 * scale, 30.0 * scale];

        egui::Window::new("Crop Editor")
            .collapsible(false)
            .resizable(true)
            .default_size([width, height])
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.add_space(8.0);

                // Get target aspect ratio from print size
                // IMPORTANT: Calculate what rotation the layout engine WILL decide
                // based on current source image vs target box, not what it WAS
                let (ia_w_in, ia_h_in) = self.imageable_size_in();
                let (w_in, h_in) = if q_fit_to_page {
                    (ia_w_in, ia_h_in)
                } else {
                    q_size.as_inches()
                };

                // Calculate if layout engine will rotate - same logic as layout_engine.rs
                let src_w = if let Some((sw, _sh)) = q_src_size_px {
                    sw as f32
                } else {
                    full_image.size[0] as f32
                };
                let src_h = if let Some((_sw, sh)) = q_src_size_px {
                    sh as f32
                } else {
                    full_image.size[1] as f32
                };

                // Determine image orientation and orient print size to match
                // (same logic as layout_engine.rs choose_orientation_for_flow_with_state)
                let src_landscape = src_w > src_h;
                let (oriented_w, oriented_h) = if src_landscape {
                    (h_in, w_in) // Swap for landscape images
                } else {
                    (w_in, h_in) // Keep as-is for portrait images
                };

                // Now calculate if rotation is needed within the oriented box
                let fitted_area_no_rotate = {
                    let s = (oriented_w / src_w).min(oriented_h / src_h);
                    let fw = src_w * s;
                    let fh = src_h * s;
                    fw * fh
                };
                let fitted_area_rotate = {
                    let s = (oriented_w / src_h).min(oriented_h / src_w);
                    let fw = src_h * s;
                    let fh = src_w * s;
                    fw * fh
                };
                let will_rotate = fitted_area_rotate > fitted_area_no_rotate;

                let (target_w, target_h) = if will_rotate {
                    (oriented_h, oriented_w)
                } else {
                    (oriented_w, oriented_h)
                };

                // Adjust for inner border: crop should fit in the inner area
                let (target_w, target_h) = if q_border_type
                    == vibeprint::layout_engine::BorderType::Inner
                    && q_border_width_pt > 0.0
                {
                    let border_in = q_border_width_pt / 72.0; // Convert pt to inches
                    (
                        (target_w - border_in * 2.0).max(0.1),
                        (target_h - border_in * 2.0).max(0.1),
                    )
                } else {
                    (target_w, target_h)
                };

                let target_aspect = target_w / target_h;

                // Downsample image for preview if too large
                let max_preview_dim = 1024;
                let (img_w, img_h) = (full_image.size[0] as f32, full_image.size[1] as f32);
                let scale = (max_preview_dim as f32 / img_w.max(img_h)).min(1.0);

                // Load/create preview texture once and cache it
                let tex_name = format!("crop_preview_{}", q_filepath_str);
                let tex_name_path: std::path::PathBuf = tex_name.clone().into();
                let tex = if let Some(tex) = self.state.preview_textures.get(&tex_name_path) {
                    tex.clone()
                } else {
                    // Step 1: Start with full resolution image
                    let mut preview_img = full_image.clone();

                    // Step 2: Apply ICC color management at FULL RESOLUTION (before any resize)
                    // This is critical for wide-gamut profiles like ProPhoto RGB
                    if let Some(ref monitor_profile) = self.state.monitor_icc_profile {
                        let mut pixel_bytes: Vec<u8> = preview_img
                            .pixels
                            .iter()
                            .flat_map(|c| [c.r(), c.g(), c.b()])
                            .collect();

                        let src_icc = self
                            .state
                            .embedded_icc_by_path
                            .get(&q.filepath)
                            .and_then(|v| v.as_deref());

                        if apply_preview_transform(
                            monitor_profile,
                            src_icc,
                            self.state.output_icc.as_ref().map(|e| &e.path),
                            &mut pixel_bytes,
                            self.state.intent.to_lcms(),
                            self.state.bpc,
                            self.state.softproof_enabled,
                        )
                        .is_some()
                        {
                            preview_img.pixels = pixel_bytes
                                .chunks_exact(3)
                                .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                                .collect();
                        }
                    }

                    // Step 3: Downscale AFTER color management (if needed)
                    let preview_img = if scale < 1.0 {
                        let preview_w = (img_w * scale) as u32;
                        let preview_h = (img_h * scale) as u32;

                        // Convert ColorImage to RgbImage for resizing
                        let rgb_img = image::RgbImage::from_raw(
                            preview_img.size[0] as u32,
                            preview_img.size[1] as u32,
                            preview_img
                                .pixels
                                .iter()
                                .flat_map(|p| [p.r(), p.g(), p.b()])
                                .collect(),
                        )
                        .unwrap_or_else(|| {
                            image::RgbImage::new(
                                preview_img.size[0] as u32,
                                preview_img.size[1] as u32,
                            )
                        });

                        let resized = image::imageops::resize(
                            &rgb_img,
                            preview_w,
                            preview_h,
                            image::imageops::FilterType::Lanczos3,
                        );

                        eframe::egui::ColorImage {
                            size: [preview_w as usize, preview_h as usize],
                            pixels: resized
                                .into_raw()
                                .chunks_exact(3)
                                .map(|p| Color32::from_rgb(p[0], p[1], p[2]))
                                .collect(),
                        }
                    } else {
                        preview_img
                    };

                    let tex =
                        ctx.load_texture(&tex_name, preview_img, egui::TextureOptions::LINEAR);
                    self.state
                        .preview_textures
                        .insert(tex_name_path, tex.clone());
                    tex
                };

                let preview_aspect = img_w / img_h;

                // Central panel for image display
                let available_width = ui.available_width() - 16.0; // Small margin
                let available_height = ui.available_height() - 80.0; // Reserve space for buttons
                let image_rect = Rect::from_min_size(
                    ui.cursor().min,
                    Vec2::new(available_width, available_height),
                );

                // Calculate display size to fit image_rect while maintaining aspect ratio
                let rect_aspect = image_rect.width() / image_rect.height();
                let (display_w, display_h) = if preview_aspect > rect_aspect {
                    (image_rect.width(), image_rect.width() / preview_aspect)
                } else {
                    (image_rect.height() * preview_aspect, image_rect.height())
                };

                let image_display_rect =
                    Rect::from_center_size(image_rect.center(), Vec2::new(display_w, display_h));

                // Draw the image
                let painter = ui.painter_at(image_rect);
                painter.image(
                    tex.id(),
                    image_display_rect,
                    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );

                // Calculate crop window rect in display coordinates
                let (u0, v0, u1, v1) = self.state.crop_editor_uv;
                let crop_w = (u1 - u0) * display_w;
                let crop_h = (v1 - v0) * display_h;
                let crop_rect = Rect::from_min_size(
                    image_display_rect.min + Vec2::new(u0 * display_w, v0 * display_h),
                    Vec2::new(crop_w, crop_h),
                );

                // Draw dimmed overlay outside crop (4 rectangles: top, bottom, left, right)
                let overlay_color = Color32::from_rgba_premultiplied(0, 0, 0, 180);
                // Top
                painter.rect_filled(
                    Rect::from_min_max(
                        image_display_rect.min,
                        Pos2::new(image_display_rect.max.x, crop_rect.min.y),
                    ),
                    0.0,
                    overlay_color,
                );
                // Bottom
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(image_display_rect.min.x, crop_rect.max.y),
                        image_display_rect.max,
                    ),
                    0.0,
                    overlay_color,
                );
                // Left
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(image_display_rect.min.x, crop_rect.min.y),
                        Pos2::new(crop_rect.min.x, crop_rect.max.y),
                    ),
                    0.0,
                    overlay_color,
                );
                // Right
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(crop_rect.max.x, crop_rect.min.y),
                        Pos2::new(image_display_rect.max.x, crop_rect.max.y),
                    ),
                    0.0,
                    overlay_color,
                );

                // Draw crop window outline with thicker border
                painter.rect_stroke(crop_rect, 0.0, egui::Stroke::new(3.0, Color32::WHITE));

                // Draw corner resize handle (bottom-right) - larger and more visible
                let handle_size = 16.0;
                let handle_rect = Rect::from_min_size(
                    Pos2::new(crop_rect.max.x - handle_size, crop_rect.max.y - handle_size),
                    Vec2::new(handle_size, handle_size),
                );
                let handle_color = if self.state.crop_editor_resizing {
                    Color32::from_rgb(100, 200, 255)
                } else {
                    Color32::WHITE
                };
                painter.rect_filled(handle_rect, 3.0, handle_color);
                painter.rect_stroke(handle_rect, 3.0, egui::Stroke::new(2.0, Color32::BLACK));
                // Draw diagonal lines to indicate resize direction
                let line_offset = 4.0;
                painter.line_segment(
                    [
                        handle_rect.min + Vec2::new(line_offset, handle_size - line_offset),
                        handle_rect.max - Vec2::new(line_offset, line_offset),
                    ],
                    egui::Stroke::new(2.0, Color32::BLACK),
                );

                // Get pointer position for interactions
                let pointer_pos = ui.input(|i| i.pointer.latest_pos());

                // 1. RESIZE HANDLE INTERACTION
                // Expand interaction area slightly beyond visual handle for easier grabbing
                let handle_interact_rect = handle_rect.expand(4.0);
                let handle_sense = Sense::click_and_drag();
                let handle_response = ui.allocate_rect(handle_interact_rect, handle_sense);

                if handle_response.dragged() {
                    if !self.state.crop_editor_resizing {
                        self.state.crop_editor_resizing = true;
                        self.state.crop_editor_resize_start_pos = pointer_pos;
                        self.state.crop_editor_resize_start_uv = Some(self.state.crop_editor_uv);
                    }

                    if let Some(start_pos) = self.state.crop_editor_resize_start_pos {
                        if let Some(start_uv) = self.state.crop_editor_resize_start_uv {
                            let current_pos = pointer_pos.unwrap_or(start_pos);
                            let delta = current_pos - start_pos;

                            // Calculate resize - using diagonal drag with aspect ratio maintained
                            // Positive delta (drag down-right) = grow, negative = shrink
                            let delta_pixels = (delta.x + delta.y * target_aspect) / 2.0;

                            // Convert pixel delta to UV delta based on current crop display size
                            let (su0, sv0, su1, sv1) = start_uv;
                            let start_crop_w_pixels = (su1 - su0) * display_w;
                            let uv_per_pixel = (su1 - su0) / start_crop_w_pixels.max(1.0);
                            let delta_u = delta_pixels * uv_per_pixel;

                            let current_crop_w = su1 - su0;
                            let _current_crop_h = sv1 - sv0;

                            // Grow/shrink from center while maintaining aspect
                            let new_crop_w = (current_crop_w + delta_u).max(0.05).min(1.0);
                            let new_crop_h = new_crop_w / target_aspect;

                            let center_u = (su0 + su1) / 2.0;
                            let center_v = (sv0 + sv1) / 2.0;

                            // Calculate new bounds centered on same point
                            let mut new_u0 = center_u - new_crop_w / 2.0;
                            let mut new_v0 = center_v - new_crop_h / 2.0;
                            let mut new_u1 = new_u0 + new_crop_w;
                            let mut new_v1 = new_v0 + new_crop_h;

                            // Clamp to image bounds
                            if new_u0 < 0.0 {
                                new_u0 = 0.0;
                                new_u1 = new_crop_w;
                            }
                            if new_u1 > 1.0 {
                                new_u1 = 1.0;
                                new_u0 = 1.0 - new_crop_w;
                            }
                            if new_v0 < 0.0 {
                                new_v0 = 0.0;
                                new_v1 = new_crop_h;
                            }
                            if new_v1 > 1.0 {
                                new_v1 = 1.0;
                                new_v0 = 1.0 - new_crop_h;
                            }

                            self.state.crop_editor_uv = (new_u0, new_v0, new_u1, new_v1);
                            // Update center and zoom for consistent scroll wheel behavior
                            self.state.crop_editor_center =
                                ((new_u0 + new_u1) / 2.0, (new_v0 + new_v1) / 2.0);
                            // Calculate zoom as ratio of current to default dimension
                            let current_w = new_u1 - new_u0;
                            self.state.crop_editor_zoom =
                                current_w / self.state.crop_editor_default_w;
                        }
                    }
                } else {
                    self.state.crop_editor_resizing = false;
                    self.state.crop_editor_resize_start_pos = None;
                    self.state.crop_editor_resize_start_uv = None;
                }

                // 2. CROP WINDOW DRAG INTERACTION
                if !self.state.crop_editor_resizing {
                    let crop_sense = Sense::click_and_drag();
                    let crop_response = ui.allocate_rect(crop_rect, crop_sense);

                    if crop_response.dragged() {
                        if !self.state.crop_editor_dragging {
                            self.state.crop_editor_dragging = true;
                            self.state.crop_editor_drag_start = pointer_pos;
                            self.state.crop_editor_drag_start_uv = Some(self.state.crop_editor_uv);
                        }

                        if let Some(start_pos) = self.state.crop_editor_drag_start {
                            if let Some(start_uv) = self.state.crop_editor_drag_start_uv {
                                let current_pos = pointer_pos.unwrap_or(start_pos);
                                let delta = current_pos - start_pos;
                                let delta_uv = Vec2::new(delta.x / display_w, delta.y / display_h);
                                let (su0, sv0, su1, sv1) = start_uv;

                                let crop_w_uv = su1 - su0;
                                let crop_h_uv = sv1 - sv0;

                                // Calculate new position clamped to image bounds
                                let new_u0 = (su0 + delta_uv.x).max(0.0).min(1.0 - crop_w_uv);
                                let new_v0 = (sv0 + delta_uv.y).max(0.0).min(1.0 - crop_h_uv);
                                let new_u1 = new_u0 + crop_w_uv;
                                let new_v1 = new_v0 + crop_h_uv;

                                self.state.crop_editor_uv = (new_u0, new_v0, new_u1, new_v1);
                                // Update center position for zoom operations
                                self.state.crop_editor_center =
                                    ((new_u0 + new_u1) / 2.0, (new_v0 + new_v1) / 2.0);
                            }
                        }
                    } else {
                        self.state.crop_editor_dragging = false;
                        self.state.crop_editor_drag_start = None;
                        self.state.crop_editor_drag_start_uv = None;
                    }
                }

                // 3. MOUSE WHEEL ZOOM (when hovering over image)
                let image_sense = Sense::hover();
                let image_response = ui.allocate_rect(image_rect, image_sense);

                if image_response.hovered()
                    && !self.state.crop_editor_dragging
                    && !self.state.crop_editor_resizing
                {
                    let scroll_delta = ui.input(|i| i.raw_scroll_delta);
                    if scroll_delta.y != 0.0 {
                        let zoom_delta = 1.0 + (scroll_delta.y * 0.001).clamp(-0.5, 0.5);
                        let (center_u, center_v) = self.state.crop_editor_center;

                        // Get the default dimensions stored at editor open time
                        let default_w = self.state.crop_editor_default_w;
                        let default_h = self.state.crop_editor_default_h;

                        // Update zoom level and calculate new dimensions
                        let new_zoom = (self.state.crop_editor_zoom * zoom_delta).max(0.05);
                        self.state.crop_editor_zoom = new_zoom;

                        // Scale from default dimensions (always maintain aspect ratio)
                        let mut new_w = default_w * new_zoom;
                        let mut new_h = default_h * new_zoom;

                        // Clamp to image bounds while maintaining aspect ratio
                        // If we hit a bound, scale both dimensions proportionally
                        let max_w = center_u.min(1.0 - center_u) * 2.0; // Max width that fits at center
                        let max_h = center_v.min(1.0 - center_v) * 2.0; // Max height that fits at center

                        let scale_w = if new_w > max_w { max_w / new_w } else { 1.0 };
                        let scale_h = if new_h > max_h { max_h / new_h } else { 1.0 };
                        let scale = scale_w.min(scale_h);

                        new_w *= scale;
                        new_h *= scale;

                        // Enforce minimum size
                        if new_w < 0.05 {
                            let min_scale = 0.05 / new_w;
                            new_w = 0.05;
                            new_h *= min_scale;
                        }

                        // Calculate final bounds centered on current center
                        let half_w = new_w / 2.0;
                        let half_h = new_h / 2.0;
                        let new_u0 = (center_u - half_w).max(0.0);
                        let new_v0 = (center_v - half_h).max(0.0);
                        let new_u1 = (new_u0 + new_w).min(1.0);
                        let new_v1 = (new_v0 + new_h).min(1.0);

                        self.state.crop_editor_uv = (new_u0, new_v0, new_u1, new_v1);
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // Buttons
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Apply button (rightmost due to right_to_left layout)
                        let apply_btn = ui.add_sized(
                            btn_size,
                            egui::Button::new(
                                RichText::new("Apply").strong().color(Color32::WHITE),
                            )
                            .fill(Color32::from_rgb(60, 120, 200)),
                        );
                        if apply_btn.clicked() {
                            // Save UVs to queue item
                            if let Some(item) =
                                self.state.queue.iter_mut().find(|qi| qi.id == queue_id)
                            {
                                let (u0, v0, u1, v1) = self.state.crop_editor_uv;
                                item.crop_u0 = Some(u0);
                                item.crop_v0 = Some(v0);
                                item.crop_u1 = Some(u1);
                                item.crop_v1 = Some(v1);
                                self.relayout_queue();
                            }
                            self.state.show_crop_editor = false;
                            // Clean up the temporary texture
                            let tex_name: std::path::PathBuf =
                                format!("crop_preview_{}", q_filepath_str).into();
                            self.state.preview_textures.remove(&tex_name);
                        }

                        // Reset button (middle)
                        if ui.add_sized(btn_size, egui::Button::new("Reset")).clicked() {
                            // Reset to auto-calculated centered crop
                            // For rotated images, swap dimensions so crop is calculated correctly
                            // on the original image (will be rotated to match target aspect)
                            let (calc_w, calc_h) = if will_rotate {
                                (target_h, target_w)
                            } else {
                                (target_w, target_h)
                            };
                            let auto_uv = if let Some((sw, sh)) = q_src_size_px {
                                crate::utils::calc_crop_uv(
                                    calc_w, calc_h, sw, sh, false, true, None,
                                )
                            } else {
                                (0.0, 0.0, 1.0, 1.0)
                            };
                            self.state.crop_editor_uv = auto_uv;
                            // Reset default dimensions, zoom and center to match the auto-calculated crop
                            let (u0, v0, u1, v1) = auto_uv;
                            self.state.crop_editor_default_w = u1 - u0;
                            self.state.crop_editor_default_h = v1 - v0;
                            self.state.crop_editor_zoom = 1.0;
                            self.state.crop_editor_center = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
                        }

                        // Cancel button (leftmost due to right_to_left layout)
                        if ui
                            .add_sized(btn_size, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            self.state.show_crop_editor = false;
                            // Clean up the temporary texture
                            let tex_name: std::path::PathBuf =
                                format!("crop_preview_{}", q_filepath_str).into();
                            self.state.preview_textures.remove(&tex_name);
                        }

                        ui.add_space(20.0);

                        // Instruction text on the left
                        ui.label(
                            egui::RichText::new(
                                "Please use the scroll wheel and mouse to select your crop.",
                            )
                            .color(Color32::from_gray(200))
                            .size(14.0),
                        );
                    });
                });
            });
    }

    pub(crate) fn show_custom_size_modal(&mut self, ctx: &Context) {
        let (ia_w_in, ia_h_in) = self.imageable_size_in();

        // Compute aspect ratio from staged or selected queue item
        let aspect: Option<f32> = if let Some(src) = self.state.staged_source_image.as_ref() {
            let [w, h] = src.size;
            if h > 0 { Some(w as f32 / h as f32) } else { None }
        } else if let Some(qi) = self.selected_queue() {
            qi.src_size_px.map(|(w, h)| if h > 0 { w as f32 / h as f32 } else { 1.0 })
        } else {
            None
        };

        let mut close = false;
        let mut confirmed: Option<(f32, f32)> = None;

        egui::Window::new("Custom Print Size")
            .collapsible(false)
            .resizable(false)
            .min_width(260.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.add_space(4.0);

                // ── Mode selector ────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.radio_value(
                        &mut self.state.custom_size_mode,
                        CustomSizeMode::Specific,
                        "Specific size",
                    );
                    ui.radio_value(
                        &mut self.state.custom_size_mode,
                        CustomSizeMode::LongSide,
                        "Long side",
                    );
                });

                ui.add_space(8.0);

                // ── Input fields ─────────────────────────────────────────────
                let (parsed_w, parsed_h) = match self.state.custom_size_mode {
                    CustomSizeMode::Specific => {
                        egui::Grid::new("custom_size_grid")
                            .num_columns(4)
                            .spacing([4.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("W:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.state.custom_size_w_str)
                                        .desired_width(60.0),
                                );
                                ui.label("H:");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.state.custom_size_h_str)
                                        .desired_width(60.0),
                                );
                                ui.label("in");
                                ui.end_row();
                            });
                        let pw = self.state.custom_size_w_str.parse::<f32>().ok();
                        let ph = self.state.custom_size_h_str.parse::<f32>().ok();
                        (pw, ph)
                    }
                    CustomSizeMode::LongSide => {
                        egui::Grid::new("custom_size_long_grid")
                            .num_columns(3)
                            .spacing([4.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Long side:");
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut self.state.custom_size_long_str,
                                    )
                                    .desired_width(60.0),
                                );
                                ui.label("in");
                                ui.end_row();
                            });
                        if let Some(asp) = aspect {
                            if let Ok(long) =
                                self.state.custom_size_long_str.parse::<f32>()
                            {
                                let (w, h) = if asp >= 1.0 {
                                    // landscape: long = w
                                    let h = long / asp;
                                    (long, h)
                                } else {
                                    // portrait: long = h
                                    let w = long * asp;
                                    (w, long)
                                };
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format!(
                                        "→  {:.3}\" × {:.3}\"",
                                        w, h
                                    ))
                                    .weak()
                                    .size(11.0),
                                );
                                (Some(w), Some(h))
                            } else {
                                (None, None)
                            }
                        } else {
                            ui.label(
                                RichText::new("No image aspect ratio available")
                                    .weak()
                                    .size(11.0),
                            );
                            (None, None)
                        }
                    }
                };

                // ── Validation ───────────────────────────────────────────────
                let size_ok = match (parsed_w, parsed_h) {
                    (Some(w), Some(h)) => w > 0.0 && h > 0.0,
                    _ => false,
                };
                let fits = match (parsed_w, parsed_h) {
                    (Some(w), Some(h)) => {
                        let (fits, _) = crate::utils::check_size_fit(w, h, ia_w_in, ia_h_in);
                        fits
                    }
                    _ => false,
                };

                if size_ok && !fits {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("⚠ Exceeds printable area")
                            .color(Color32::from_rgb(220, 120, 40))
                            .size(11.0),
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            size_ok && fits,
                            egui::Button::new("Confirm"),
                        )
                        .clicked()
                    {
                        if let (Some(w), Some(h)) = (parsed_w, parsed_h) {
                            confirmed = Some((w, h));
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });

        if close {
            self.state.show_custom_size_modal = false;
        }

        if let Some((w, h)) = confirmed {
            self.state.show_custom_size_modal = false;
            if self.state.staged.is_some() {
                let _ = self.enqueue_staged_with_size(w, h);
            } else {
                self.update_selected_queue_size(w, h);
            }
        }
    }
}

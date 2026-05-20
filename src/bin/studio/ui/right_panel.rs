use eframe::egui::{self, Color32, RichText, Vec2};

use crate::types::{
    Engine, IccPickerContext, Intent, ProcState, RightTab, FIT_PAGE_IDX, print_sizes,
};
use crate::utils::check_size_fit;
use crate::App;

impl App {
    pub(crate) fn draw_right(&mut self, ui: &mut egui::Ui) {
        // ── Tab bar ───────────────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.state.right_tab,
                RightTab::PrinterSettings,
                "Printer Settings",
            );
            ui.selectable_value(
                &mut self.state.right_tab,
                RightTab::ImageProperties,
                "Image Properties",
            );
            ui.selectable_value(
                &mut self.state.right_tab,
                RightTab::ImageQueue,
                "Image Queue",
            );
        });
        ui.separator();

        match self.state.right_tab {
            RightTab::PrinterSettings => {
                // ── Settings Section (Top - Scrollable) ─────────────────────────────
                let available_height = ui.available_height();
                egui::ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .max_height(available_height * 0.6)
                    .show(ui, |ui| {
                        self.draw_tab_printer(ui);
                    });

                // ── Print Section (Bottom - Fixed) ─────────────────────────────────────
                ui.separator();
                self.draw_print_controls(ui);
            }
            RightTab::ImageProperties => {
                self.draw_tab_image(ui);
            }
            RightTab::ImageQueue => {
                self.draw_tab_queue(ui);
            }
        }
    }

    fn draw_tab_printer(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("tab_printer_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);
                let mut preview_dirty = false;

                // ── Block A: Hardware ─────────────────────────────────────────────
                ui.label(RichText::new("Hardware & Properties").strong().size(12.0));
                ui.separator();

                let prev_idx = self.state.printer_idx;
                let prev_page_size_idx = self.state.selected_page_size_idx;
                let prev_dpi = self.state.target_dpi;
                ui.horizontal(|ui| {
                    let selected_name = self
                        .state
                        .printers
                        .get(self.state.printer_idx)
                        .map(|p| p.name.as_str())
                        .unwrap_or("No printer found");
                    egui::ComboBox::from_id_salt("printer_cb")
                        .width(ui.available_width() - 36.0)
                        .selected_text(selected_name)
                        .show_ui(ui, |ui| {
                            for (i, p) in self.state.printers.iter().enumerate() {
                                let label = if p.is_default {
                                    format!("★ {}", p.name)
                                } else {
                                    p.name.clone()
                                };
                                ui.selectable_value(&mut self.state.printer_idx, i, label);
                            }
                        });
                    if ui
                        .small_button("⚙")
                        .on_hover_text("Printer properties")
                        .clicked()
                    {
                        self.state.show_props = true;
                    }
                });
                if self.state.printer_idx != prev_idx {
                    self.sync_caps_to_selection();
                }

                // ── Paper Size ────────────────────────────────────────────
                if let Some(caps) = &self.state.caps {
                    let ps_label = caps
                        .page_sizes
                        .get(self.state.selected_page_size_idx)
                        .map(|p| p.label.as_str())
                        .unwrap_or("—");
                    ui.horizontal(|ui| {
                        ui.label("Paper Size:");
                        egui::ComboBox::from_id_salt("paper_size_cb")
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
                    });
                }

                // ── Border ─
                let (paper_w_in, paper_h_in) = self
                    .state
                    .caps
                    .as_ref()
                    .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
                    .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
                    .unwrap_or((8.5, 11.0));

                ui.vertical(|ui| {
                    let unit_label = if self.state.use_metric { "Border: (mm)" } else { "Border: (in)" };
                    ui.label(unit_label);
                    // Row 1: Left + Right
                    ui.horizontal(|ui| {
                        ui.label("L:");
                        let resp_l = ui.add(
                            egui::TextEdit::singleline(&mut self.state.border_edit_l)
                                .desired_width(50.0)
                                .font(egui::FontId::proportional(12.0)),
                        );
                        if resp_l.gained_focus() {
                            self.state.border_edit_l = crate::app::format_border_edit(
                                self.state.user_border.left,
                                self.state.use_metric,
                            );
                        }
                        if resp_l.lost_focus() {
                            if let Ok(v) = self.state.border_edit_l.parse::<f32>() {
                                let input_in = if self.state.use_metric {
                                    vibeprint::layout_engine::mm_to_inches(v)
                                } else {
                                    v
                                };
                                let max_left = (0.5 * paper_w_in - self.state.user_border.right)
                                    .max(self.state.reported_border.left);
                                let new_left =
                                    input_in.clamp(self.state.reported_border.left, max_left);
                                if (new_left - self.state.user_border.left).abs() > 0.0001 {
                                    if self.state.user_border.right + new_left > 0.5 * paper_w_in {
                                        let new_right = (0.5 * paper_w_in - new_left)
                                            .max(self.state.reported_border.right);
                                        if (new_right - self.state.user_border.right).abs() > 0.0001 {
                                            self.state.user_border.right = new_right;
                                            self.state.border_edit_r = crate::app::format_border_edit(
                                                new_right, self.state.use_metric,
                                            );
                                            self.state.log.push(format!(
                                                "Right border reduced to {:.3}in (sum cap)",
                                                new_right
                                            ));
                                        }
                                    }
                                    self.state.user_border.left = new_left;
                                    self.state.border_edit_l = crate::app::format_border_edit(
                                        new_left, self.state.use_metric,
                                    );
                                    self.relayout_queue();
                                } else if (input_in - self.state.user_border.left).abs() > 0.0001 {
                                    self.state.border_edit_l = crate::app::format_border_edit(
                                        self.state.user_border.left, self.state.use_metric,
                                    );
                                }
                            } else {
                                self.state.border_edit_l = crate::app::format_border_edit(
                                    self.state.user_border.left, self.state.use_metric,
                                );
                            }
                        }

                        ui.add_space(8.0);

                        ui.label("R:");
                        let resp_r = ui.add(
                            egui::TextEdit::singleline(&mut self.state.border_edit_r)
                                .desired_width(50.0)
                                .font(egui::FontId::proportional(12.0)),
                        );
                        if resp_r.gained_focus() {
                            self.state.border_edit_r = crate::app::format_border_edit(
                                self.state.user_border.right,
                                self.state.use_metric,
                            );
                        }
                        if resp_r.lost_focus() {
                            if let Ok(v) = self.state.border_edit_r.parse::<f32>() {
                                let input_in = if self.state.use_metric {
                                    vibeprint::layout_engine::mm_to_inches(v)
                                } else {
                                    v
                                };
                                let max_right = (0.5 * paper_w_in - self.state.user_border.left)
                                    .max(self.state.reported_border.right);
                                let new_right =
                                    input_in.clamp(self.state.reported_border.right, max_right);
                                if (new_right - self.state.user_border.right).abs() > 0.0001 {
                                    if self.state.user_border.left + new_right > 0.5 * paper_w_in {
                                        let new_left = (0.5 * paper_w_in - new_right)
                                            .max(self.state.reported_border.left);
                                        if (new_left - self.state.user_border.left).abs() > 0.0001 {
                                            self.state.user_border.left = new_left;
                                            self.state.border_edit_l = crate::app::format_border_edit(
                                                new_left, self.state.use_metric,
                                            );
                                            self.state.log.push(format!(
                                                "Left border reduced to {:.3}in (sum cap)",
                                                new_left
                                            ));
                                        }
                                    }
                                    self.state.user_border.right = new_right;
                                    self.state.border_edit_r = crate::app::format_border_edit(
                                        new_right, self.state.use_metric,
                                    );
                                    self.relayout_queue();
                                } else if (input_in - self.state.user_border.right).abs() > 0.0001 {
                                    self.state.border_edit_r = crate::app::format_border_edit(
                                        self.state.user_border.right, self.state.use_metric,
                                    );
                                }
                            } else {
                                self.state.border_edit_r = crate::app::format_border_edit(
                                    self.state.user_border.right, self.state.use_metric,
                                );
                            }
                        }

                        ui.add_space(8.0);

                        if ui
                            .small_button("✖")
                            .on_hover_text("Reset to printer default")
                            .clicked()
                        {
                            self.state.user_border = self.state.reported_border;
                            self.state.border_edit_l = crate::app::format_border_edit(
                                self.state.user_border.left, self.state.use_metric,
                            );
                            self.state.border_edit_r = crate::app::format_border_edit(
                                self.state.user_border.right, self.state.use_metric,
                            );
                            self.state.border_edit_t = crate::app::format_border_edit(
                                self.state.user_border.top, self.state.use_metric,
                            );
                            self.state.border_edit_b = crate::app::format_border_edit(
                                self.state.user_border.bottom, self.state.use_metric,
                            );
                            self.relayout_queue();
                        }
                    });

                    // Row 2: Top + Bottom
                    ui.horizontal(|ui| {
                        ui.label("T:");
                        let resp_t = ui.add(
                            egui::TextEdit::singleline(&mut self.state.border_edit_t)
                                .desired_width(50.0)
                                .font(egui::FontId::proportional(12.0)),
                        );
                        if resp_t.gained_focus() {
                            self.state.border_edit_t = crate::app::format_border_edit(
                                self.state.user_border.top,
                                self.state.use_metric,
                            );
                        }
                        if resp_t.lost_focus() {
                            if let Ok(v) = self.state.border_edit_t.parse::<f32>() {
                                let input_in = if self.state.use_metric {
                                    vibeprint::layout_engine::mm_to_inches(v)
                                } else {
                                    v
                                };
                                let max_top = (0.5 * paper_h_in - self.state.user_border.bottom)
                                    .max(self.state.reported_border.top);
                                let new_top =
                                    input_in.clamp(self.state.reported_border.top, max_top);
                                if (new_top - self.state.user_border.top).abs() > 0.0001 {
                                    if self.state.user_border.bottom + new_top > 0.5 * paper_h_in {
                                        let new_bottom = (0.5 * paper_h_in - new_top)
                                            .max(self.state.reported_border.bottom);
                                        if (new_bottom - self.state.user_border.bottom).abs()
                                            > 0.0001
                                        {
                                            self.state.user_border.bottom = new_bottom;
                                            self.state.border_edit_b =
                                                crate::app::format_border_edit(
                                                    new_bottom, self.state.use_metric,
                                                );
                                            self.state.log.push(format!(
                                                "Bottom border reduced to {:.3}in (sum cap)",
                                                new_bottom
                                            ));
                                        }
                                    }
                                    self.state.user_border.top = new_top;
                                    self.state.border_edit_t = crate::app::format_border_edit(
                                        new_top, self.state.use_metric,
                                    );
                                    self.relayout_queue();
                                } else if (input_in - self.state.user_border.top).abs() > 0.0001 {
                                    self.state.border_edit_t = crate::app::format_border_edit(
                                        self.state.user_border.top, self.state.use_metric,
                                    );
                                }
                            } else {
                                self.state.border_edit_t = crate::app::format_border_edit(
                                    self.state.user_border.top, self.state.use_metric,
                                );
                            }
                        }

                        ui.add_space(8.0);

                        ui.label("B:");
                        let resp_b = ui.add(
                            egui::TextEdit::singleline(&mut self.state.border_edit_b)
                                .desired_width(50.0)
                                .font(egui::FontId::proportional(12.0)),
                        );
                        if resp_b.gained_focus() {
                            self.state.border_edit_b = crate::app::format_border_edit(
                                self.state.user_border.bottom,
                                self.state.use_metric,
                            );
                        }
                        if resp_b.lost_focus() {
                            if let Ok(v) = self.state.border_edit_b.parse::<f32>() {
                                let input_in = if self.state.use_metric {
                                    vibeprint::layout_engine::mm_to_inches(v)
                                } else {
                                    v
                                };
                                let max_bottom =
                                    (0.5 * paper_h_in - self.state.user_border.top)
                                        .max(self.state.reported_border.bottom);
                                let new_bottom =
                                    input_in.clamp(self.state.reported_border.bottom, max_bottom);
                                if (new_bottom - self.state.user_border.bottom).abs() > 0.0001 {
                                    if self.state.user_border.top + new_bottom > 0.5 * paper_h_in {
                                        let new_top = (0.5 * paper_h_in - new_bottom)
                                            .max(self.state.reported_border.top);
                                        if (new_top - self.state.user_border.top).abs() > 0.0001 {
                                            self.state.user_border.top = new_top;
                                            self.state.border_edit_t =
                                                crate::app::format_border_edit(
                                                    new_top, self.state.use_metric,
                                                );
                                            self.state.log.push(format!(
                                                "Top border reduced to {:.3}in (sum cap)",
                                                new_top
                                            ));
                                        }
                                    }
                                    self.state.user_border.bottom = new_bottom;
                                    self.state.border_edit_b = crate::app::format_border_edit(
                                        new_bottom, self.state.use_metric,
                                    );
                                    self.relayout_queue();
                                } else if (input_in - self.state.user_border.bottom).abs() > 0.0001 {
                                    self.state.border_edit_b = crate::app::format_border_edit(
                                        self.state.user_border.bottom, self.state.use_metric,
                                    );
                                }
                            } else {
                                self.state.border_edit_b = crate::app::format_border_edit(
                                    self.state.user_border.bottom, self.state.use_metric,
                                );
                            }
                        }
                    });
                });

                // ── Print to file ──
                ui.checkbox(&mut self.state.print_to_file, "Print to file");

                ui.add_space(10.0);

                // ── Block B: Processing Engine ────────────────────────────────────
                ui.label(RichText::new("Processing Engine").strong().size(12.0));
                ui.separator();

                // Interpolate
                ui.horizontal(|ui| {
                    ui.label("Interpolate:");
                    egui::ComboBox::from_id_salt("engine_cb")
                        .selected_text(self.state.engine.label())
                        .show_ui(ui, |ui| {
                            for e in Engine::ALL {
                                ui.selectable_value(&mut self.state.engine, e.clone(), e.label());
                            }
                        });
                });

                if self.state.selected_page_size_idx != prev_page_size_idx {
                    let new_reported = self.calc_reported_border();
                    self.state.reported_border = new_reported;
                    self.state.user_border.left =
                        self.state.user_border.left.max(new_reported.left);
                    self.state.user_border.right =
                        self.state.user_border.right.max(new_reported.right);
                    self.state.user_border.top =
                        self.state.user_border.top.max(new_reported.top);
                    self.state.user_border.bottom =
                        self.state.user_border.bottom.max(new_reported.bottom);
                    // Re-clamp sum caps for new paper dimensions
                    let (pw, ph) = self
                        .state
                        .caps
                        .as_ref()
                        .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
                        .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
                        .unwrap_or((8.5, 11.0));
                    if self.state.user_border.left + self.state.user_border.right > 0.5 * pw {
                        self.state.user_border.right = (0.5 * pw - self.state.user_border.left)
                            .max(self.state.reported_border.right);
                    }
                    if self.state.user_border.top + self.state.user_border.bottom > 0.5 * ph {
                        self.state.user_border.bottom = (0.5 * ph - self.state.user_border.top)
                            .max(self.state.reported_border.bottom);
                    }
                    self.state.border_edit_l =
                        crate::app::format_border_edit(
                            self.state.user_border.left, self.state.use_metric);
                    self.state.border_edit_r =
                        crate::app::format_border_edit(
                            self.state.user_border.right, self.state.use_metric);
                    self.state.border_edit_t =
                        crate::app::format_border_edit(
                            self.state.user_border.top, self.state.use_metric);
                    self.state.border_edit_b =
                        crate::app::format_border_edit(
                            self.state.user_border.bottom, self.state.use_metric);
                    self.relayout_queue();
                }

                // Sharpen
                ui.horizontal(|ui| {
                    ui.label("Sharpen:");
                    ui.add(egui::Slider::new(&mut self.state.sharpen, 0..=20).show_value(true));
                    if ui.small_button("✖").on_hover_text("Reset to 5").clicked() {
                        self.state.sharpen = 5;
                    }
                });

                // Output DPI
                ui.horizontal(|ui| {
                    ui.label("Output DPI:");
                    egui::ComboBox::from_id_salt("dpi_cb")
                        .selected_text(format!("{}", self.state.target_dpi))
                        .show_ui(ui, |ui| {
                            for &dpi in &[300u32, 360, 600, 720] {
                                ui.selectable_value(
                                    &mut self.state.target_dpi,
                                    dpi,
                                    format!("{dpi}"),
                                );
                            }
                        });
                });

                if self.state.target_dpi != prev_dpi {
                    self.relayout_queue();
                }

                ui.add_space(10.0);

                // ── Block C: Color Management ─────────────────────────────────────
                ui.label(RichText::new("Color Management").strong().size(12.0));

                // Output ICC
                ui.horizontal(|ui| {
                    ui.label("Output ICC:");
                    let icc_label = self
                        .state
                        .output_icc
                        .as_ref()
                        .map(|e| e.description.clone())
                        .unwrap_or_else(|| "sRGB".into());
                    ui.add(
                        egui::Label::new(RichText::new(&icc_label).small().monospace()).truncate(),
                    );
                    if self.state.icc_scan_pending {
                        ui.label("Scanning...");
                    } else if ui.small_button("…").clicked() {
                        // Set picker context to Output ICC selection
                        self.state.icc_picker_context = IccPickerContext::Output;
                        use crate::icc::scan_icc_directories;
                        use std::sync::mpsc::channel;
                        let (tx, rx) = channel::<Vec<crate::types::IccProfileEntry>>();
                        self.state.icc_scan_rx = Some(rx);
                        self.state.icc_scan_pending = true;
                        self.state.icc_profiles.clear();
                        self.state.icc_filter_text.clear();
                        std::thread::spawn(move || scan_icc_directories(tx));
                    }
                    if self.state.output_icc.is_some() && ui.small_button("✖").clicked() {
                        self.state.output_icc = None;
                        preview_dirty = true;
                    }
                });

                // Intent
                let prev_intent = self.state.intent;
                ui.horizontal(|ui| {
                    ui.label("Intent:");
                    egui::ComboBox::from_id_salt("intent_cb")
                        .selected_text(self.state.intent.label())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.state.intent,
                                Intent::Relative,
                                Intent::Relative.label(),
                            );
                            ui.selectable_value(
                                &mut self.state.intent,
                                Intent::Perceptual,
                                Intent::Perceptual.label(),
                            );
                            ui.selectable_value(
                                &mut self.state.intent,
                                Intent::Saturation,
                                Intent::Saturation.label(),
                            );
                        });
                });
                if self.state.intent != prev_intent {
                    preview_dirty = true;
                }

                if ui
                    .checkbox(&mut self.state.bpc, "Black Point Compensation")
                    .changed()
                {
                    preview_dirty = true;
                }

                if preview_dirty {
                    self.mark_preview_dirty();
                }

                ui.add_space(10.0);

                let is_running = matches!(self.state.proc_state, ProcState::Running);
                let is_printing = self.state.print_rx.is_some();
                let has_image = !self.state.queue.is_empty();

                let btn_text = if self.state.print_to_file {
                    "Print to File"
                } else {
                    "Print"
                };
                let print_btn = egui::Button::new(RichText::new(btn_text).size(14.0).strong())
                    .min_size(Vec2::new(ui.available_width(), 36.0))
                    .fill(Color32::from_rgb(60, 120, 200));

                if ui
                    .add_enabled(has_image && !is_running && !is_printing, print_btn)
                    .clicked()
                {
                    if self.state.print_to_file {
                        self.start_process_export();
                    } else {
                        self.start_process_print();
                    }
                }

                ui.add_space(4.0);

                if is_running {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Processing…");
                    });
                } else if is_printing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Printing…");
                    });
                } else if !has_image {
                    ui.label(RichText::new("Add queued images first").small().weak());
                } else if let ProcState::Done(ref paths) = self.state.proc_state {
                    let msg = if let Some(first) = paths.first() {
                        format!(
                            "✓ {} page(s): {}",
                            paths.len(),
                            first.file_name().unwrap_or_default().to_string_lossy()
                        )
                    } else {
                        "✓ Done".to_string()
                    };
                    ui.label(RichText::new(msg).small().color(Color32::GREEN));
                } else if let ProcState::Failed(ref e) = self.state.proc_state {
                    ui.label(RichText::new(format!("✗ {e}")).small().color(Color32::RED));
                }
            });
    }

    fn draw_print_controls(&mut self, ui: &mut egui::Ui) {
        if self.state.print_to_file {
            ui.label(RichText::new("Output Folder").strong().size(12.0));
        } else {
            ui.label(
                RichText::new("Output Folder")
                    .strong()
                    .size(12.0)
                    .color(Color32::TRANSPARENT),
            );
        }
        if self.state.print_to_file {
            ui.separator();
        } else {
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            let label = self.state.output_dir.to_string_lossy();
            if self.state.print_to_file {
                ui.add(
                    egui::Label::new(RichText::new(label.as_ref()).small().monospace()).truncate(),
                );
                if ui.small_button("…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.state.output_dir = p;
                    }
                }
            } else {
                ui.add(
                    egui::Label::new(
                        RichText::new(label.as_ref())
                            .small()
                            .monospace()
                            .color(Color32::TRANSPARENT),
                    )
                    .truncate(),
                );
                let _ = ui.label(RichText::new("…").color(Color32::TRANSPARENT));
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if self.state.print_to_file {
                ui.label("Output depth:");
            } else {
                ui.label(RichText::new("Output depth:").color(Color32::TRANSPARENT));
            }
            if self.state.print_to_file {
                ui.selectable_value(&mut self.state.depth16, true, "16-bit");
                ui.selectable_value(&mut self.state.depth16, false, "8-bit Dithered");
            } else {
                let _ = ui.label(RichText::new("16-bit").color(Color32::TRANSPARENT));
                let _ = ui.label(RichText::new("8-bit Dithered").color(Color32::TRANSPARENT));
            }
        });

        ui.add_space(4.0);

        // ── Log (at the bottom) ───────────────────────────────────────────────
        ui.add_space(12.0);
        ui.checkbox(&mut self.state.show_log, RichText::new("Show Log").strong().size(12.0));
        ui.separator();
        if self.state.show_log {
            egui::ScrollArea::vertical()
                .id_salt("log_scroll")
                .max_height(80.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for entry in &self.state.log {
                        ui.label(RichText::new(entry).small().monospace());
                    }
                });
        }
    }

    fn draw_tab_image(&mut self, ui: &mut egui::Ui) {
        let (ia_w_in, ia_h_in) = self.imageable_size_in();

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Print Size").strong().size(12.0));
            ui.label(
                RichText::new(format!(
                    "  Printable area: {}",
                    if self.state.use_metric {
                        let (w_mm, h_mm) = (
                            vibeprint::layout_engine::inches_to_mm(ia_w_in),
                            vibeprint::layout_engine::inches_to_mm(ia_h_in),
                        );
                        format!("{:.1} × {:.1} mm", w_mm, h_mm)
                    } else {
                        format!("{:.2}\" × {:.2}\"", ia_w_in, ia_h_in)
                    }
                ))
                .size(10.0)
                .color(egui::Color32::from_gray(180)),
            );
        });

        let has_target = self.state.staged.is_some() || self.state.selected_queue_id.is_some();
        if !has_target {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Stage an image or select one from queue")
                    .weak()
                    .italics()
                    .size(11.0),
            );
            return;
        }

        ui.add_space(4.0);

        let sizes = print_sizes(self.state.use_metric);

        // Determine the currently selected size index for queued images
        let selected_size_idx = if self.state.staged.is_some() {
            None // No highlighting for staged images
        } else if let Some(qi) = self.selected_queue() {
            if qi.fit_to_page {
                Some(FIT_PAGE_IDX)
            } else {
                let (qw, qh) = qi.size.as_inches();
                sizes.iter().enumerate().find_map(|(i, &(w, h, _))| {
                    let (sw, sh) = if self.state.use_metric {
                        (vibeprint::layout_engine::mm_to_inches(w), vibeprint::layout_engine::mm_to_inches(h))
                    } else {
                        (w, h)
                    };
                    if (qw - sw).abs() < 0.001 && (qh - sh).abs() < 0.001 {
                        Some(i)
                    } else {
                        None
                    }
                })
            }
        } else {
            None
        };

        egui::ScrollArea::vertical()
            .id_salt("print_sizes")
            .show(ui, |ui| {
                for (idx, &(w, h, label)) in sizes.iter().enumerate() {
                    let (w_in, h_in) = if self.state.use_metric {
                        (vibeprint::layout_engine::mm_to_inches(w), vibeprint::layout_engine::mm_to_inches(h))
                    } else {
                        (w, h)
                    };
                    let (fits, _) = check_size_fit(w_in, h_in, ia_w_in, ia_h_in);
                    let is_selected = selected_size_idx == Some(idx);
                    let row_text = RichText::new(label).size(13.0).color(if is_selected {
                        Color32::from_rgb(60, 120, 200)
                    } else if fits {
                        Color32::from_gray(210)
                    } else {
                        Color32::from_gray(150)
                    });
                    let resp = ui.add_enabled(fits, egui::SelectableLabel::new(false, row_text));
                    if !fits {
                        resp.clone()
                            .on_disabled_hover_text("Too large for the printable area");
                    }
                    if resp.clicked() {
                        if self.state.staged.is_some() {
                            let _ = self.enqueue_staged_with_idx(idx);
                        } else {
                            self.update_selected_queue_size_idx(idx);
                        }
                    }
                }

                // Fit to Page option (in same section as print sizes)
                let is_fit_to_page_selected = selected_size_idx == Some(FIT_PAGE_IDX);
                let fit_text =
                    RichText::new("Fit to Page")
                        .size(13.0)
                        .color(if is_fit_to_page_selected {
                            Color32::from_rgb(60, 120, 200)
                        } else {
                            Color32::from_gray(210)
                        });
                if ui.selectable_label(false, fit_text).clicked() {
                    if self.state.staged.is_some() {
                        let _ = self.enqueue_staged_with_idx(FIT_PAGE_IDX);
                    } else {
                        self.update_selected_queue_size_idx(FIT_PAGE_IDX);
                    }
                }

                // Custom Size option
                let is_custom_selected = selected_size_idx.is_none()
                    && self.selected_queue().map(|q| !q.fit_to_page).unwrap_or(false);
                let custom_text = RichText::new("Custom Size")
                    .size(13.0)
                    .color(if is_custom_selected {
                        Color32::from_rgb(60, 120, 200)
                    } else {
                        Color32::from_gray(210)
                    });
                if ui.selectable_label(false, custom_text).clicked() {
                    let (w_str, h_str, long_str) =
                        if let Some(qi) = self.selected_queue() {
                            let (w, h) = qi.size.as_inches();
                            let long = w.max(h);
                            if self.state.use_metric {
                                (
                                    format!("{:.3}", vibeprint::layout_engine::inches_to_mm(w)),
                                    format!("{:.3}", vibeprint::layout_engine::inches_to_mm(h)),
                                    format!("{:.3}", vibeprint::layout_engine::inches_to_mm(long)),
                                )
                            } else {
                                (format!("{:.3}", w), format!("{:.3}", h), format!("{:.3}", long))
                            }
                        } else if self.state.staged.is_some() {
                            (String::new(), String::new(), String::new())
                        } else {
                            (String::new(), String::new(), String::new())
                        };
                    self.state.custom_size_w_str = w_str;
                    self.state.custom_size_h_str = h_str;
                    self.state.custom_size_long_str = long_str;
                    self.state.custom_size_input_is_metric = self.state.use_metric;
                    self.state.show_custom_size_modal = true;
                }

                ui.separator();

                ui.add_space(8.0);

                let mut crop_enabled = self
                    .selected_queue()
                    .map(|q| q.crop_enabled)
                    .unwrap_or(false);

                ui.horizontal(|ui| {
                    let crop_response =
                        ui.add(egui::Checkbox::new(&mut crop_enabled, "Crop Image"));

                    if crop_response.changed() {
                        // Get imageable size before mutable borrow
                        let (ia_w_in, ia_h_in) = self.imageable_size_in();
if let Some(item) = self.selected_queue_mut() {
                                    item.crop_enabled = crop_enabled;
                                    if crop_enabled {
                                        // Calculate and store auto-crop UVs for the target cell
                                        let (w_in, h_in) = if item.fit_to_page {
                                            (ia_w_in, ia_h_in)
                                        } else {
                                            item.size.as_inches()
                                        };

                                        // Calculate oriented box and rotation
                                        let (sw, sh) = item.src_size_px.unwrap_or((1, 1));
                                        let src_landscape = (sw as f32) > (sh as f32);
                                        let (oriented_w, oriented_h) = if src_landscape {
                                            (h_in, w_in)
                                        } else {
                                            (w_in, h_in)
                                        };

                                        // Calculate if rotation is needed
                                        let fitted_area_no_rotate = {
                                            let s = (oriented_w / sw as f32).min(oriented_h / sh as f32);
                                            (sw as f32 * s) * (sh as f32 * s)
                                        };
                                        let fitted_area_rotate = {
                                            let s = (oriented_w / sh as f32).min(oriented_h / sw as f32);
                                            (sh as f32 * s) * (sw as f32 * s)
                                        };
                                        let will_rotate = fitted_area_rotate > fitted_area_no_rotate;

                                        // For crop calculation, swap dimensions if rotation is needed
                                        let (calc_w, calc_h) = if will_rotate {
                                            (oriented_h, oriented_w)
                                        } else {
                                            (oriented_w, oriented_h)
                                        };

                                        // Adjust cell dimensions for border type:
                                        // - Outer: expand by 2×border (border adds outside the cell)
                                        // - Inner: shrink by 2×border (border eats inside the cell)
                                        // This ensures crop UVs match the visible area aspect ratio.
                                        let (calc_w, calc_h) = if item.border_type
                                            == vibeprint::layout_engine::BorderType::Outer
                                            && item.border_width_pt > 0.0
                                        {
                                            let border_in = item.border_width_pt / 72.0;
                                            let (expand_w, expand_h) = if item.crop_inverted {
                                                (calc_h, calc_w)
                                            } else {
                                                (calc_w, calc_h)
                                            };
                                            let (expanded_w, expanded_h) = (
                                                expand_w + border_in * 2.0,
                                                expand_h + border_in * 2.0,
                                            );
                                            if item.crop_inverted {
                                                (expanded_h, expanded_w)
                                            } else {
                                                (expanded_w, expanded_h)
                                            }
                                        } else if item.border_type
                                            == vibeprint::layout_engine::BorderType::Inner
                                            && item.border_width_pt > 0.0
                                        {
                                            let border_in = item.border_width_pt / 72.0;
                                            let (shrink_w, shrink_h) = if item.crop_inverted {
                                                (calc_h, calc_w)
                                            } else {
                                                (calc_w, calc_h)
                                            };
                                            let (shrunk_w, shrunk_h) = (
                                                (shrink_w - border_in * 2.0).max(0.1),
                                                (shrink_h - border_in * 2.0).max(0.1),
                                            );
                                            if item.crop_inverted {
                                                (shrunk_h, shrunk_w)
                                            } else {
                                                (shrunk_w, shrunk_h)
                                            }
                                        } else {
                                            (calc_w, calc_h)
                                        };

                                        // Calculate auto-crop UVs
                                        // When inverted, flip the rotation decision to match processor logic
                                        let will_rotate_for_uv = if item.crop_inverted {
                                            !will_rotate
                                        } else {
                                            will_rotate
                                        };
                                        let (u0, v0, u1, v1) = crate::utils::calc_crop_uv(
                                            calc_w,
                                            calc_h,
                                            sw,
                                            sh,
                                            will_rotate_for_uv,
                                            true,
                                            None,
                                        );
                                        item.crop_u0 = Some(u0);
                                        item.crop_v0 = Some(v0);
                                        item.crop_u1 = Some(u1);
                                        item.crop_v1 = Some(v1);
                                        item.crop_inverted = false;
                            } else {
                                // When disabling crop, clear custom UVs to restore full image
                                item.crop_u0 = None;
                                item.crop_v0 = None;
                                item.crop_u1 = None;
                                item.crop_v1 = None;
                                item.crop_inverted = false;
                            }
                            self.mark_preview_dirty();
                        }
                    }

                    // Edit button - enabled when crop is enabled and a queue item is selected
                    let has_custom_crop = self
                        .selected_queue()
                        .map(|q| {
                            q.crop_u0.is_some()
                                && q.crop_v0.is_some()
                                && q.crop_u1.is_some()
                                && q.crop_v1.is_some()
                        })
                        .unwrap_or(false);
                    let edit_enabled = crop_enabled && self.selected_queue().is_some();
                    let edit_text = if has_custom_crop { "Edit*" } else { "Edit" };
                    if ui
                        .add_enabled(edit_enabled, egui::Button::new(edit_text))
                        .clicked()
                    {
                        if let Some(q) = self.selected_queue() {
                            // Initialize crop editor with current UVs or auto-calculated
                            // Extract all data from q before modifying state
                            let crop_inverted = q.crop_inverted;
                            let stored_uv = match (q.crop_u0, q.crop_v0, q.crop_u1, q.crop_v1) {
                                (Some(u0), Some(v0), Some(u1), Some(v1)) => Some((u0, v0, u1, v1)),
                                _ => None,
                            };
                            let (w_in, h_in) = if q.fit_to_page {
                                self.imageable_size_in()
                            } else {
                                q.size.as_inches()
                            };
                            let (sw, sh) = q.src_size_px.unwrap_or((1, 1));
                            let src_w = sw as f32;
                            let src_h = sh as f32;
                            let src_landscape = src_w > src_h;
                            let border_type = q.border_type;
                            let border_width_pt = q.border_width_pt;
                            
                            // Now safe to modify state
                            self.state.crop_editor_inverted = crop_inverted;

                            // Orient print size to match image aspect ratio
                            let (oriented_w, oriented_h) = if src_landscape {
                                (h_in, w_in)
                            } else {
                                (w_in, h_in)
                            };

                            // Calculate if rotation is needed within oriented box
                            let fitted_area_no_rotate = {
                                let s = (oriented_w / src_w).min(oriented_h / src_h);
                                (src_w * s) * (src_h * s)
                            };
                            let fitted_area_rotate = {
                                let s = (oriented_w / src_h).min(oriented_h / src_w);
                                (src_h * s) * (src_w * s)
                            };
                            let will_rotate = fitted_area_rotate > fitted_area_no_rotate;

                            // For crop calculation, swap dimensions if rotation is needed
                            // so calc_crop_uv returns UVs in original image space
                            let (calc_w, calc_h) = if will_rotate {
                                (oriented_h, oriented_w)
                            } else {
                                (oriented_w, oriented_h)
                            };

                            // Adjust cell dimensions for border type:
                            // - Outer: expand by 2×border (border adds outside the cell)
                            // - Inner: shrink by 2×border (border eats inside the cell)
                            // This ensures crop UVs match the visible area aspect ratio.
                            let (final_w, final_h) = if border_type
                                == vibeprint::layout_engine::BorderType::Outer
                                && border_width_pt > 0.0
                            {
                                let border_in = border_width_pt / 72.0;
                                let (expand_w, expand_h) = if crop_inverted {
                                    (calc_h, calc_w)
                                } else {
                                    (calc_w, calc_h)
                                };
                                let (expanded_w, expanded_h) = (
                                    expand_w + border_in * 2.0,
                                    expand_h + border_in * 2.0,
                                );
                                if crop_inverted {
                                    (expanded_h, expanded_w)
                                } else {
                                    (expanded_w, expanded_h)
                                }
                            } else if border_type
                                == vibeprint::layout_engine::BorderType::Inner
                                && border_width_pt > 0.0
                            {
                                let border_in = border_width_pt / 72.0;
                                let (shrink_w, shrink_h) = if crop_inverted {
                                    (calc_h, calc_w)
                                } else {
                                    (calc_w, calc_h)
                                };
                                let (shrunk_w, shrunk_h) = (
                                    (shrink_w - border_in * 2.0).max(0.1),
                                    (shrink_h - border_in * 2.0).max(0.1),
                                );
                                if crop_inverted {
                                    (shrunk_h, shrunk_w)
                                } else {
                                    (shrunk_w, shrunk_h)
                                }
                            } else {
                                (calc_w, calc_h)
                            };

let auto_uv = if sw != 0 && sh != 0 {
                                // When inverted, flip the rotation decision to match processor logic.
                                let will_rotate_for_uv = if crop_inverted { !will_rotate } else { will_rotate };
                                let uv = crate::utils::calc_crop_uv(
                                    final_w,
                                    final_h,
                                    sw,
                                    sh,
                                    will_rotate_for_uv,
                                    true,
                                    None,
                                );
                                Some(uv)
                            } else {
                                Some((0.0, 0.0, 1.0, 1.0))
                            };
                            let initial_uv =
                                stored_uv.unwrap_or(auto_uv.unwrap_or((0.0, 0.0, 1.0, 1.0)));
                            self.state.crop_editor_uv = initial_uv;
                            // Store initial dimensions as the "default" for zoom = 1.0
                            let (u0, v0, u1, v1) = initial_uv;
                            self.state.crop_editor_default_w = u1 - u0;
                            self.state.crop_editor_default_h = v1 - v0;
                            self.state.crop_editor_zoom = 1.0; // Start at zoom = 1.0 (default size)
                            self.state.crop_editor_center = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
                            self.state.crop_editor_queue_id = self.state.selected_queue_id;
                            self.state.show_crop_editor = true;
                        }
                    }
                });

                // ── Center to Page ─────────────────────────────────────────
                ui.add_space(8.0);
                let mut center_to_page = self
                    .selected_queue()
                    .map(|q| q.center_to_page)
                    .unwrap_or(false);
                let center_disabled = self
                    .selected_queue()
                    .map(|q| q.fit_to_page)
                    .unwrap_or(false);
                ui.add_enabled_ui(!center_disabled, |ui| {
                    if ui
                        .checkbox(&mut center_to_page, "Center to page")
                        .changed()
                    {
                        if let Some(item) = self.selected_queue_mut() {
                            item.center_to_page = center_to_page;
                            self.relayout_queue();
                            // Jump to the item's page after relayout
                            if let Some(id) = self.state.selected_queue_id {
                                if let Some(item) = self.state.queue.iter().find(|q| q.id == id) {
                                    self.state.current_page = item.page;
                                }
                            }
                        }
                    }
                });

                // ── Border ────────────────────────────────────────────────
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(RichText::new("Border").strong().size(12.0));

                // Check if fit_to_page is enabled - Outer border is impossible in this case
                let is_fit_to_page = self
                    .selected_queue()
                    .map(|q| q.fit_to_page)
                    .unwrap_or(false);

                let mut border_type = self
                    .selected_queue()
                    .map(|q| q.border_type)
                    .unwrap_or(vibeprint::layout_engine::BorderType::None);

                // Calculate aesthetic default: 15% of longest cell side in points
                let default_border_pt = if let Some(item) = self.selected_queue() {
                    let (cell_w_in, cell_h_in) = if item.fit_to_page {
                        let (ia_w_in, ia_h_in) = self.imageable_size_in();
                        (ia_w_in, ia_h_in)
                    } else {
                        item.size.as_inches()
                    };
                    let longest_side_in = cell_w_in.max(cell_h_in);
                    longest_side_in * 0.15 // 15% of longest side (result in points)
                } else {
                    0.15 // Default: 15% of 1 inch (0.15 pt)
                };

                let mut border_width_pt = self
                    .selected_queue()
                    .map(|q| q.border_width_pt)
                    .unwrap_or(default_border_pt);

                // Auto-switch from Outer to Inner if fit_to_page is enabled
                if is_fit_to_page && border_type == vibeprint::layout_engine::BorderType::Outer {
                    border_type = vibeprint::layout_engine::BorderType::Inner;
                }

                ui.horizontal(|ui| {
                    ui.radio_value(
                        &mut border_type,
                        vibeprint::layout_engine::BorderType::None,
                        "None",
                    );
                    ui.radio_value(
                        &mut border_type,
                        vibeprint::layout_engine::BorderType::Inner,
                        "Inner",
                    );
                    if is_fit_to_page {
                        ui.add_enabled_ui(false, |ui| {
                            ui.radio_value(
                                &mut border_type,
                                vibeprint::layout_engine::BorderType::Outer,
                                "Outer",
                            )
                            .on_disabled_hover_text("Outer border not available with Fit to Page");
                        });
                    } else {
                        ui.radio_value(
                            &mut border_type,
                            vibeprint::layout_engine::BorderType::Outer,
                            "Outer",
                        );
                    }
                });

                // Width field - visible when Inner or Outer selected
                let has_queue_selection = self.selected_queue().is_some();
                let show_width = border_type != vibeprint::layout_engine::BorderType::None;

                // Calculate max border: 20% of longest cell side, but also ensure outer border fits within imageable area
                let max_border_pt = if let Some(item) = self.selected_queue() {
                    let (cell_w_in, cell_h_in) = if item.fit_to_page {
                        let (ia_w_in, ia_h_in) = self.imageable_size_in();
                        (ia_w_in, ia_h_in)
                    } else {
                        item.size.as_inches()
                    };
                    let longest_side_in = cell_w_in.max(cell_h_in);
                    // 20% of longest side in inches, convert to points (1 inch = 72 pt)
                    let percentage_max = longest_side_in * 0.2 * 72.0;

                    // For outer borders, also constrain by imageable area.
                    // Use orientation-safe axis pairing: smallest cell dim vs smallest page dim,
                    // largest cell dim vs largest page dim — safe regardless of rotation.
                    if border_type == vibeprint::layout_engine::BorderType::Outer && !item.fit_to_page {
                        let (ia_w_in, ia_h_in) = self.imageable_size_in();
                        let min_cell = cell_w_in.min(cell_h_in);
                        let max_cell = cell_w_in.max(cell_h_in);
                        let min_page = ia_w_in.min(ia_h_in);
                        let max_page = ia_w_in.max(ia_h_in);
                        let max_from_short = ((min_page - min_cell) / 2.0).max(0.0) * 72.0;
                        let max_from_long = ((max_page - max_cell) / 2.0).max(0.0) * 72.0;
                        let space_constrained_max = max_from_short.min(max_from_long);
                        percentage_max.min(space_constrained_max)
                    } else {
                        percentage_max
                    }
                } else {
                    20.16 // Default max if no selection (20% of 1.4" at 72pt/inch)
                };

                if show_width {
                    ui.horizontal(|ui| {
                        ui.label("Width:");
let current_pt = border_width_pt.min(max_border_pt);
                        if self.state.border_width_edit_focus {
                            if self.state.use_metric {
                                if self.state.border_width_edit_string.is_empty() {
                                    self.state.border_width_edit_string = format!("{}", (vibeprint::layout_engine::inches_to_mm(current_pt / 72.0)).round() as u32);
                                }
                            } else {
                                if self.state.border_width_edit_string.is_empty() {
                                    self.state.border_width_edit_string = format!("{:.3}", current_pt);
                                }
                            }
                        } else {
                            if self.state.use_metric {
                                self.state.border_width_edit_string = format!("{}", (vibeprint::layout_engine::inches_to_mm(current_pt / 72.0)).round() as u32);
                            } else {
                                self.state.border_width_edit_string = format!("{:.3}", current_pt);
                            }
                        }
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.state.border_width_edit_string)
                                .desired_width(50.0)
                                .font(egui::FontId::proportional(12.0)),
                        );
                        self.state.border_width_edit_focus = resp.has_focus();
                        if resp.gained_focus() {
                            if self.state.use_metric {
                                self.state.border_width_edit_string = format!("{}", (vibeprint::layout_engine::inches_to_mm(current_pt / 72.0)).round() as u32);
                            } else {
                                self.state.border_width_edit_string = format!("{:.3}", current_pt);
                            }
                        }
                        if resp.lost_focus() {
                            if self.state.use_metric {
                                if let Ok(mm_val) = self.state.border_width_edit_string.parse::<u32>() {
                                    let pt = vibeprint::layout_engine::mm_to_inches(mm_val as f32) * 72.0;
                                    border_width_pt = pt.max(0.0).min(max_border_pt);
                                }
                                self.state.border_width_edit_string = format!("{}", (vibeprint::layout_engine::inches_to_mm(border_width_pt / 72.0)).round() as u32);
                            } else {
                                if let Ok(v) = self.state.border_width_edit_string.parse::<f32>() {
                                    border_width_pt = v.max(0.0).min(max_border_pt);
                                }
                                self.state.border_width_edit_string = format!("{:.3}", border_width_pt.min(max_border_pt));
                            }
                        }
                        // Also apply on-the-fly for immediate visual feedback while typing
                        if resp.changed() && resp.has_focus() {
                            if self.state.use_metric {
                                if let Ok(mm_val) = self.state.border_width_edit_string.parse::<u32>() {
                                    let pt = vibeprint::layout_engine::mm_to_inches(mm_val as f32) * 72.0;
                                    border_width_pt = pt.max(0.0).min(max_border_pt);
                                }
                            } else {
                                if let Ok(v) = self.state.border_width_edit_string.parse::<f32>() {
                                    border_width_pt = v.max(0.0).min(max_border_pt);
                                }
                            }
                        }
                        ui.label(if self.state.use_metric { "mm" } else { "pt" });
                        let max_label = if self.state.use_metric {
                            format!("max {}", (vibeprint::layout_engine::inches_to_mm(max_border_pt / 72.0)).round() as u32)
                        } else {
                            format!("max {:.3}", max_border_pt)
                        };
                        ui.label(
                            RichText::new(max_label)
                                .weak()
                                .size(10.0),
                        );
                    });
                }

                // Apply changes if they differ
                if has_queue_selection {
                    let old_border_type = self.selected_queue().map(|q| q.border_type);
                    let type_changed = old_border_type != Some(border_type);
                    let width_changed =
                        self.selected_queue().map(|q| q.border_width_pt) != Some(border_width_pt);

                    // If enabling border for first time (None -> Inner/Outer), use small default
                    let border_enabled = old_border_type
                        == Some(vibeprint::layout_engine::BorderType::None)
                        && border_type != vibeprint::layout_engine::BorderType::None;

                    // Pre-compute values needed for crop UV recalculation before mutable borrow
                    let use_metric = self.state.use_metric;

                    // IMPORTANT: Set default border width BEFORE crop calculation
                    // so UV recalculation uses the correct border size
                    if border_enabled {
                        border_width_pt = if use_metric { 2.835 } else { 1.0 };
                    }

                    let fit_to_page = self.selected_queue().map(|q| q.fit_to_page).unwrap_or(false);
                    let (cell_w_in, cell_h_in) = if fit_to_page {
                        self.imageable_size_in()
                    } else {
                        self.selected_queue().map(|q| q.size.as_inches()).unwrap_or((5.0, 7.0))
                    };
                    let src_size_px = self.selected_queue().and_then(|q| q.src_size_px).unwrap_or((1, 1));
                    let crop_inverted = self.selected_queue().map(|q| q.crop_inverted).unwrap_or(false);

                    if type_changed || width_changed {
                        // Compute oriented dimensions and rotation to determine
                        // visible area aspect in the same coordinate space as crop UVs
                        let (sw, sh) = src_size_px;
                        let sw_f = sw as f32;
                        let sh_f = sh as f32;
                        let src_landscape = sw_f > sh_f;
                        let (oriented_w, oriented_h) = if src_landscape {
                            (cell_h_in, cell_w_in)
                        } else {
                            (cell_w_in, cell_h_in)
                        };
                        let fitted_area_no_rotate = {
                            let s = (oriented_w / sw_f).min(oriented_h / sh_f);
                            (sw_f * s) * (sh_f * s)
                        };
                        let fitted_area_rotate = {
                            let s = (oriented_w / sh_f).min(oriented_h / sw_f);
                            (sh_f * s) * (sw_f * s)
                        };
                        let will_rotate = fitted_area_rotate > fitted_area_no_rotate;
                        let effective_will_rotate = if crop_inverted { !will_rotate } else { will_rotate };
                        let (full_w, full_h) = if effective_will_rotate {
                            (oriented_h, oriented_w)
                        } else {
                            (oriented_w, oriented_h)
                        };

                        // Compute old and new visible areas in oriented coordinate space
                        let new_border_in = border_width_pt.min(max_border_pt) / 72.0;

                        let new_is_inner = border_type
                            == vibeprint::layout_engine::BorderType::Inner;
                        let new_is_outer = border_type
                            == vibeprint::layout_engine::BorderType::Outer;
                        let (new_vis_w, new_vis_h) = if new_is_inner && new_border_in > 0.0 {
                            (
                                (full_w - new_border_in * 2.0).max(0.1),
                                (full_h - new_border_in * 2.0).max(0.1),
                            )
                        } else if new_is_outer && new_border_in > 0.0 {
                            (
                                full_w + new_border_in * 2.0,
                                full_h + new_border_in * 2.0,
                            )
                        } else {
                            (full_w, full_h)
                        };

                        // Always recalculate crop when border type or width changes,
                        // since even small aspect changes affect cropped images.
                        let crop_adjustment = {
                            let has_crop = self.selected_queue().map(|q| q.crop_enabled && q.crop_u0.is_some()).unwrap_or(false);
                            if has_crop {
                                let src_aspect = sw_f / sh_f;
                                let box_aspect = new_vis_w / new_vis_h;
                                let target_aspect = box_aspect / src_aspect;
                                Some(target_aspect)
                            } else {
                                None
                            }
                        };

                        if let Some(item) = self.selected_queue_mut() {
                            // Apply pre-computed crop UV adjustments
                            if let Some(target_aspect) = crop_adjustment {
                                if let (Some(u0), Some(v0), Some(u1), Some(v1)) =
                                    (item.crop_u0, item.crop_v0, item.crop_u1, item.crop_v1)
                                {
                                    let old_center_u = (u0 + u1) / 2.0;
                                    let old_center_v = (v0 + v1) / 2.0;
                                    let old_crop_area = (u1 - u0) * (v1 - v0);
                                    let new_crop_w = (old_crop_area * target_aspect).sqrt();
                                    let new_crop_h = (old_crop_area / target_aspect).sqrt();
                                    let half_w = new_crop_w / 2.0;
                                    let half_h = new_crop_h / 2.0;
                                    item.crop_u0 = Some((old_center_u - half_w).max(0.0));
                                    item.crop_v0 = Some((old_center_v - half_h).max(0.0));
                                    item.crop_u1 = Some((old_center_u + half_w).min(1.0));
                                    item.crop_v1 = Some((old_center_v + half_h).min(1.0));
                                }
                            }

                            item.border_type = border_type;
                            item.border_width_pt = border_width_pt.min(max_border_pt); // Clamp to max for this cell size
                                                                                       // Trigger relayout for outer border (affects cell size)
                            if border_type == vibeprint::layout_engine::BorderType::Outer
                                || (old_border_type
                                    == Some(vibeprint::layout_engine::BorderType::Outer))
                            {
                                self.relayout_queue();
                            } else {
                                self.mark_preview_dirty();
                            }
                        }
                    }
                }
            });
    }

    fn draw_tab_queue(&mut self, ui: &mut egui::Ui) {
        use uuid::Uuid;

        ui.add_space(4.0);
        ui.label(RichText::new("Queued Images").strong().size(12.0));
        ui.separator();

        if self.state.queue.is_empty() {
            ui.label(RichText::new("Queue is empty").weak().italics().size(11.0));
            return;
        }

        let mut delete_id: Option<Uuid> = None;
        let rows: Vec<(Uuid, std::path::PathBuf, usize)> = self
            .state
            .queue
            .iter()
            .map(|q| (q.id, q.filepath.clone(), q.page))
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("queue_list")
            .show(ui, |ui| {
                for (id, path, page) in &rows {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());

                    ui.horizontal(|ui| {
                        let sel = self.state.selected_queue_id == Some(*id);
                        let lbl = format!("{}  (P{})", name, *page + 1);
                        if ui.selectable_label(sel, lbl).clicked() {
                            self.state.selected_queue_id = Some(*id);
                            self.state.current_page = *page;
                            self.state.right_tab = RightTab::ImageProperties;
                        }
                        if ui.small_button("✖").clicked() {
                            delete_id = Some(*id);
                        }
                    });
                }
            });

        if let Some(id) = delete_id {
            self.state.queue.retain(|q| q.id != id);
            if self.state.selected_queue_id == Some(id) {
                self.state.selected_queue_id = None;
            }
            self.relayout_queue();
        }
    }
}

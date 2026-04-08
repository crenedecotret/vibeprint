use eframe::egui::{self, Color32, RichText, Vec2};

use crate::types::{Engine, Intent, ProcState, RightTab, FIT_PAGE_IDX, PRINT_SIZES};
use crate::utils::check_size_fit;
use crate::App;

impl App {
    pub(crate) fn draw_right(&mut self, ui: &mut egui::Ui) {
        // ── Tab bar ───────────────────────────────────────────────────────────
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.state.right_tab, RightTab::PrinterSettings, "Printer Settings",
            );
            ui.selectable_value(
                &mut self.state.right_tab, RightTab::ImageProperties, "Image Properties",
            );
            ui.selectable_value(
                &mut self.state.right_tab, RightTab::ImageQueue, "Image Queue",
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
        egui::ScrollArea::vertical().id_salt("tab_printer_scroll").show(ui, |ui| {
            ui.add_space(6.0);
            let mut preview_dirty = false;

            // ── Block A: Hardware ─────────────────────────────────────────────
            ui.label(RichText::new("Hardware & Properties").strong().size(12.0));
            ui.separator();

            let prev_idx = self.state.printer_idx;
            let prev_page_size_idx = self.state.selected_page_size_idx;
            let prev_dpi = self.state.target_dpi;
            ui.horizontal(|ui| {
                let selected_name = self.state.printers.get(self.state.printer_idx)
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
                if ui.small_button("⚙").on_hover_text("Printer properties").clicked() {
                    self.state.show_props = true;
                }
            });
            if self.state.printer_idx != prev_idx {
                self.sync_caps_to_selection();
            }

            // ── Paper Size ────────────────────────────────────────────
            if let Some(caps) = &self.state.caps {
                let ps_label = caps.page_sizes
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
                                ui.selectable_value(&mut self.state.selected_page_size_idx, i, label);
                            }
                        });
                });
            }

            // ── Border ──
            ui.horizontal(|ui| {
                ui.label("Border:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.state.border_edit_string)
                        .desired_width(60.0)
                        .font(egui::FontId::proportional(12.0)),
                );
                
                // Update edit string when gaining focus to show current value
                if resp.gained_focus() {
                    self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
                }
                
                // Apply changes when losing focus
                if resp.lost_focus() {
                    if let Ok(v) = self.state.border_edit_string.parse::<f32>() {
                        let (paper_w_in, paper_h_in) = self.state.caps
                            .as_ref()
                            .and_then(|c| c.page_sizes.get(self.state.selected_page_size_idx))
                            .map(|ps| (ps.paper_size.0 / 72.0, ps.paper_size.1 / 72.0))
                            .unwrap_or((8.5, 11.0));
                        let max_border = (paper_w_in.min(paper_h_in) * 0.25).max(self.state.reported_border_in);
                        
                        let new_border = v.clamp(self.state.reported_border_in, max_border);
                        if (new_border - self.state.user_border_in).abs() > 0.0001 {
                            self.state.user_border_in = new_border;
                            self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
                            self.relayout_queue();
                        } else if (v - self.state.user_border_in).abs() > 0.0001 {
                            self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
                        }
                    } else {
                        self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
                    }
                }
                
                if ui.small_button("✖").on_hover_text("Reset to printer default").clicked() {
                    self.state.user_border_in = self.state.reported_border_in;
                    self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
                    self.relayout_queue();
                }
                ui.label("in");
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
                self.state.reported_border_in = new_reported;
                if self.state.user_border_in < new_reported {
                    self.state.user_border_in = new_reported;
                }
                self.state.border_edit_string = format!("{:.3}", self.state.user_border_in);
                self.relayout_queue();
            }

            // Sharpen
            ui.horizontal(|ui| {
                ui.label("Sharpen:");
                ui.add(egui::Slider::new(&mut self.state.sharpen, 0..=20).show_value(true));
            });

            // Output DPI
            ui.horizontal(|ui| {
                ui.label("Output DPI:");
                egui::ComboBox::from_id_salt("dpi_cb")
                    .selected_text(format!("{}", self.state.target_dpi))
                    .show_ui(ui, |ui| {
                        for &dpi in &[300u32, 360, 600, 720] {
                            ui.selectable_value(&mut self.state.target_dpi, dpi, format!("{dpi}"));
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
                let icc_label = self.state.output_icc.as_ref()
                    .map(|e| e.description.clone())
                    .unwrap_or_else(|| "sRGB".into());
                ui.add(egui::Label::new(
                    RichText::new(&icc_label).small().monospace()
                ).truncate());
                if self.state.icc_scan_pending {
                    ui.label("Scanning...");
                } else if ui.small_button("…").clicked() {
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
                        ui.selectable_value(&mut self.state.intent, Intent::Relative, Intent::Relative.label());
                        ui.selectable_value(&mut self.state.intent, Intent::Perceptual, Intent::Perceptual.label());
                        ui.selectable_value(&mut self.state.intent, Intent::Saturation, Intent::Saturation.label());
                    });
            });
            if self.state.intent != prev_intent {
                preview_dirty = true;
            }

            if ui.checkbox(&mut self.state.bpc, "Black Point Compensation").changed() {
                preview_dirty = true;
            }

            if preview_dirty {
                self.mark_preview_dirty();
            }

            ui.add_space(10.0);

            let is_running = matches!(self.state.proc_state, ProcState::Running);
            let is_printing = self.state.print_rx.is_some();
            let has_image = !self.state.queue.is_empty();

            let btn_text = if self.state.print_to_file { "Print to File" } else { "Print" };
            let print_btn = egui::Button::new(
                RichText::new(btn_text).size(14.0).strong(),
            )
            .min_size(Vec2::new(ui.available_width(), 36.0))
            .fill(Color32::from_rgb(60, 120, 200));

            if ui.add_enabled(has_image && !is_running && !is_printing, print_btn).clicked() {
                if self.state.print_to_file {
                    self.start_process_export();
                } else {
                    self.start_process_print();
                }
            }

            ui.add_space(4.0);

            if is_running {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Processing…"); });
            } else if is_printing {
                ui.horizontal(|ui| { ui.spinner(); ui.label("Printing…"); });
            } else if !has_image {
                ui.label(RichText::new("Add queued images first").small().weak());
            } else if let ProcState::Done(ref paths) = self.state.proc_state {
                let msg = if let Some(first) = paths.first() {
                    format!("✓ {} page(s): {}", paths.len(), first.file_name().unwrap_or_default().to_string_lossy())
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
            ui.label(RichText::new("Output Folder").strong().size(12.0).color(Color32::TRANSPARENT));
        }
        if self.state.print_to_file {
            ui.separator();
        } else {
            ui.add_space(6.0);
        }
        ui.horizontal(|ui| {
            let label = self.state.output_dir.to_string_lossy();
            if self.state.print_to_file {
                ui.add(egui::Label::new(
                    RichText::new(label.as_ref()).small().monospace()
                ).truncate());
                if ui.small_button("…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.state.output_dir = p;
                    }
                }
            } else {
                ui.add(egui::Label::new(
                    RichText::new(label.as_ref()).small().monospace().color(Color32::TRANSPARENT)
                ).truncate());
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
                ui.selectable_value(&mut self.state.depth16, true,  "16-bit");
                ui.selectable_value(&mut self.state.depth16, false, "8-bit Dithered");
            } else {
                let _ = ui.label(RichText::new("16-bit").color(Color32::TRANSPARENT));
                let _ = ui.label(RichText::new("8-bit Dithered").color(Color32::TRANSPARENT));
            }
        });

        ui.add_space(4.0);

        // ── Log (at the bottom) ───────────────────────────────────────────────
        ui.add_space(12.0);
        ui.label(RichText::new("Log").strong().size(12.0));
        ui.separator();
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

    fn draw_tab_image(&mut self, ui: &mut egui::Ui) {
        let (ia_w_in, ia_h_in) = self.imageable_size_in();

        ui.add_space(4.0);
        ui.label(RichText::new("Print Size").strong().size(12.0));
        ui.separator();

        let has_target = self.state.staged.is_some() || self.state.selected_queue_id.is_some();
        if !has_target {
            ui.add_space(8.0);
            ui.label(RichText::new("Stage an image or select one from queue").weak().italics().size(11.0));
            return;
        }

        ui.label(
            RichText::new(format!("Printable area: {:.2}\" × {:.2}\"", ia_w_in, ia_h_in))
                .size(10.0)
                .weak(),
        );
        ui.add_space(4.0);

        // Determine the currently selected size index for queued images
        let selected_size_idx = if self.state.staged.is_some() {
            None // No highlighting for staged images
        } else if let Some(qi) = self.selected_queue() {
            if qi.fit_to_page {
                Some(FIT_PAGE_IDX)
            } else {
                let (qw, qh) = qi.size.as_inches();
                PRINT_SIZES.iter().enumerate().find_map(|(i, &(w, h, _))| {
                    if (qw - w).abs() < 0.001 && (qh - h).abs() < 0.001 {
                        Some(i)
                    } else {
                        None
                    }
                })
            }
        } else {
            None
        };

        egui::ScrollArea::vertical().id_salt("print_sizes").show(ui, |ui| {
            for (idx, &(w, h, label)) in PRINT_SIZES.iter().enumerate() {
                let (fits, _) = check_size_fit(w, h, ia_w_in, ia_h_in);
                let is_selected = selected_size_idx == Some(idx);
                let row_text = RichText::new(label).size(13.0).color(if is_selected {
                    Color32::from_rgb(100, 200, 100)
                } else if fits {
                    Color32::from_gray(210)
                } else {
                    Color32::from_rgb(200, 60, 60)
                });
                let resp = ui.add_enabled(fits, egui::SelectableLabel::new(false, row_text));
                if !fits {
                    resp.clone().on_disabled_hover_text("Too large for the printable area");
                }
                if resp.clicked() {
                    if self.state.staged.is_some() {
                        let _ = self.enqueue_staged_with_idx(idx);
                    } else {
                        self.update_selected_queue_size_idx(idx);
                    }
                }
            }

            ui.separator();
            let is_fit_to_page_selected = selected_size_idx == Some(FIT_PAGE_IDX);
            let fit_text = RichText::new("Fit to Page").size(13.0).color(if is_fit_to_page_selected {
                Color32::from_rgb(100, 200, 100)
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

            // Crop Image checkbox - disabled when Fit to Page is selected
            ui.add_space(8.0);
            let is_fit_to_page = selected_size_idx == Some(FIT_PAGE_IDX);

            let mut crop_enabled = self.selected_queue()
                .map(|q| q.crop_enabled)
                .unwrap_or(false);

            // Disable crop when Fit to Page is selected
            let crop_response = ui.add_enabled(
                !is_fit_to_page,
                egui::Checkbox::new(&mut crop_enabled, "Crop Image")
            );

            if !is_fit_to_page && crop_response.changed() {
                if let Some(item) = self.selected_queue_mut() {
                    item.crop_enabled = crop_enabled;
                    self.mark_preview_dirty();
                }
            }

            if is_fit_to_page && crop_enabled {
                // Auto-disable crop when fit to page is selected
                if let Some(item) = self.selected_queue_mut() {
                    item.crop_enabled = false;
                    self.mark_preview_dirty();
                }
            }

            if is_fit_to_page {
                ui.label(RichText::new("Crop disabled for Fit to Page").weak().size(10.0));
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
            .state.queue
            .iter()
            .map(|q| (q.id, q.filepath.clone(), q.page))
            .collect();
        egui::ScrollArea::vertical().id_salt("queue_list").show(ui, |ui| {
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

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icc;
mod processing;
mod types;
mod ui;
mod utils;

use std::path::PathBuf;

use app::{save_settings, App};
use types::{Engine, IccProfileFilter, Intent, LEFT_W, RIGHT_W};

use eframe::egui;

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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
        save_settings(&types::Settings {
            current_dir: Some(self.state.current_dir.to_string_lossy().into_owned()),
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
        });
    }

    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        self.pump(ctx);

        // INSERT key stages the highlighted image in Image Properties
        if ctx.input(|i| i.key_pressed(eframe::egui::Key::Insert)) {
            if let Some(path) = self.state.highlighted.clone() {
                self.stage_image(path);
            }
        }

        // DELETE key removes the selected queue item and returns to Printer Settings
        // But only if no text field is currently focused (e.g., border width input)
        if !ctx.wants_keyboard_input() && ctx.input(|i| i.key_pressed(eframe::egui::Key::Delete)) {
            if let Some(id) = self.state.selected_queue_id {
                self.state.queue.retain(|q| q.id != id);
                self.state.selected_queue_id = None;
                self.state.right_tab = types::RightTab::PrinterSettings;
                self.relayout_queue();
            }
        }

        if self.state.show_props {
            self.show_printer_props(ctx);
        }
        if self.state.show_print_confirm {
            self.show_print_confirm(ctx);
        }
        if self.state.show_icc_picker {
            self.show_icc_picker(ctx);
        }
        if self.state.show_crop_editor {
            self.show_crop_editor(ctx);
        }

        // Show splash screen during printer discovery
        if !self.state.discovery_complete {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);

                    ui.label(
                        eframe::egui::RichText::new("VibePrint Studio")
                            .size(32.0)
                            .strong(),
                    );
                    ui.add_space(24.0);

                    ui.add_space(16.0);
                    ui.horizontal(|ui| {
                        let space_each_side = (ui.available_width() - 200.0) / 2.0;
                        ui.add_space(space_each_side.max(0.0));
                        ui.spinner();
                        ui.label(eframe::egui::RichText::new("Discovering printers...").size(16.0));
                    });

                    ui.add_space(ui.available_height() * 0.3);

                    if !self.state.log.is_empty() {
                        ui.separator();
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(120.0)
                            .show(ui, |ui| {
                                for entry in self.state.log.iter().take(5) {
                                    ui.label(
                                        eframe::egui::RichText::new(entry).small().monospace(),
                                    );
                                }
                            });
                    }
                });
            });
            return;
        }

        // Normal UI after discovery complete
        egui::SidePanel::left("assets")
            .default_width(LEFT_W)
            .min_width(140.0)
            .max_width(520.0)
            .resizable(true)
            .show(ctx, |ui| self.draw_left(ui));

        egui::SidePanel::right("sidebar")
            .default_width(RIGHT_W)
            .min_width(220.0)
            .max_width(600.0)
            .resizable(true)
            .show(ctx, |ui| self.draw_right(ui));

        egui::CentralPanel::default().show(ctx, |ui| {
            let toolbar_h = 42.0_f32;
            let canvas_h = (ui.available_height() - toolbar_h).max(0.0);
            ui.allocate_ui(
                eframe::egui::Vec2::new(ui.available_width(), canvas_h),
                |ui| {
                    self.draw_canvas(ui);
                },
            );
            self.draw_canvas_toolbar(ui);
        });
    }
}

fn main() -> eframe::Result<()> {
    // Parse CLI arguments for optional image path
    let args: Vec<String> = std::env::args().collect();
    let auto_image_path = if args.len() > 1 {
        let arg = &args[1];
        if arg == "--help" || arg == "-h" {
            println!("VibePrint Studio - Image Printing Application");
            println!("Usage: studio --image IMAGE_PATH");
            println!();
            println!("Options:");
            println!("  --image PATH      Path to image file to auto-load with 'Fit to Page'");
            println!("  --help, -h        Show this help message");
            std::process::exit(0);
        } else if arg == "--image" {
            if args.len() > 2 {
                Some(PathBuf::from(&args[2]))
            } else {
                eprintln!("Error: --image requires a path argument");
                std::process::exit(1);
            }
        } else {
            eprintln!("Error: Unknown argument '{}'", arg);
            eprintln!("Use --help for usage information");
            std::process::exit(1);
        }
    } else {
        None
    };

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("VibePrint Studio")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "VibePrint Studio",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc, auto_image_path)) as Box<dyn eframe::App>)),
    )
}

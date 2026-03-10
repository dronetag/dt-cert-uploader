use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use dt_cert_uploader_core::{list_serial_ports, upload_certificates, UploadParams, UploadProgress};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dronetag Certificate Upload")
            .with_inner_size([560.0, 420.0])
            .with_min_inner_size([560.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "dt-cert-uploader",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

// --- Upload state shared between UI thread and worker thread ---

#[derive(Clone, PartialEq)]
enum UploadState {
    Idle,
    Running {
        file_index: usize,
        file_label: String,
        remote_path: String,
        transferred: u64,
        total: u64,
    },
    Done,
    Error(String),
}

struct App {
    // Connection
    port: String,
    baud_rate: String,
    available_ports: Vec<String>,

    // Cert files
    ca_path: String,
    client_cert_path: String,
    client_key_path: String,

    // Security tag
    sec_tag: u8,

    // Upload state (shared with worker thread)
    upload_state: Arc<Mutex<UploadState>>,
}

impl App {
    fn new() -> Self {
        let available_ports = list_serial_ports();
        let port = available_ports.first().cloned().unwrap_or_default();

        Self {
            port,
            baud_rate: "500000".to_string(),
            available_ports,
            ca_path: String::new(),
            client_cert_path: String::new(),
            client_key_path: String::new(),
            sec_tag: 1,
            upload_state: Arc::new(Mutex::new(UploadState::Idle)),
        }
    }

    fn is_uploading(&self) -> bool {
        matches!(*self.upload_state.lock().unwrap(), UploadState::Running { .. })
    }

    fn start_upload(&self) {
        let params = UploadParams {
            port: self.port.clone(),
            baud_rate: self.baud_rate.parse().unwrap_or(500000),
            sec_tag: self.sec_tag,
            ca_path: self.ca_path.clone(),
            client_cert_path: self.client_cert_path.clone(),
            client_key_path: self.client_key_path.clone(),
        };

        let state = Arc::clone(&self.upload_state);

        thread::spawn(move || {
            let result = upload_certificates(&params, |progress: UploadProgress| {
                let mut s = state.lock().unwrap();
                *s = UploadState::Running {
                    file_index: progress.file_index,
                    file_label: progress.file_label,
                    remote_path: progress.remote_path,
                    transferred: progress.transferred,
                    total: progress.total,
                };
                true
            });

            let mut s = state.lock().unwrap();
            *s = match result {
                Ok(()) => UploadState::Done,
                Err(e) => UploadState::Error(e),
            };
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep repainting while uploading so progress updates show
        if self.is_uploading() {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Dronetag Certificate Upload");
            ui.add_space(12.0);

            let uploading = self.is_uploading();

            // --- Connection ---
            egui::Grid::new("connection_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .min_col_width(100.0)
                .show(ui, |ui| {
                    ui.strong("Serial port:");
                    egui::ComboBox::from_id_salt("port_combo")
                        .selected_text(&self.port)
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for p in &self.available_ports {
                                ui.selectable_value(&mut self.port, p.clone(), p);
                            }
                        });
                    ui.end_row();

                    ui.strong("Baud rate:");
                    ui.add_enabled(
                        !uploading,
                        egui::TextEdit::singleline(&mut self.baud_rate).desired_width(100.0),
                    );
                    ui.end_row();

                    ui.strong("Security tag:");
                    ui.horizontal(|ui| {
                        for tag in 1u8..=9 {
                            ui.radio_value(&mut self.sec_tag, tag, tag.to_string());
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // --- File pickers ---
            egui::Grid::new("files_grid")
                .num_columns(3)
                .spacing([8.0, 6.0])
                .min_col_width(150.0)
                .show(ui, |ui| {
                    file_row(ui, "CA Certificate:", &mut self.ca_path, "*.pem *.crt *.cer", uploading);
                    ui.end_row();
                    file_row(ui, "Client Certificate:", &mut self.client_cert_path, "*.pem *.crt *.cer", uploading);
                    ui.end_row();
                    file_row(ui, "Client Private Key:", &mut self.client_key_path, "*.pem *.key", uploading);
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);

            // --- Upload button + progress ---
            let ready = !self.port.is_empty()
                && !self.ca_path.is_empty()
                && !self.client_cert_path.is_empty()
                && !self.client_key_path.is_empty()
                && !uploading;

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("⬆  Upload Certificates").min_size(egui::vec2(180.0, 32.0)))
                    .clicked()
                {
                    *self.upload_state.lock().unwrap() = UploadState::Idle;
                    self.start_upload();
                }

                // Refresh port list
                if ui.button("🔄 Refresh ports").clicked() {
                    self.available_ports = list_serial_ports();
                    if !self.available_ports.contains(&self.port) {
                        self.port = self.available_ports.first().cloned().unwrap_or_default();
                    }
                }
            });

            ui.add_space(8.0);

            // --- Status area ---
            let state = self.upload_state.lock().unwrap().clone();
            match state {
                UploadState::Idle => {}

                UploadState::Running {
                    file_index,
                    file_label,
                    remote_path,
                    transferred,
                    total,
                } => {
                    let overall_pct = (file_index as f32) / 3.0
                        + (transferred as f32 / total as f32) / 3.0;

                    ui.label(format!(
                        "[{}/3] {} → {}",
                        file_index + 1,
                        file_label,
                        remote_path
                    ));
                    ui.add(
                        egui::ProgressBar::new(overall_pct)
                            .show_percentage()
                            .animate(true),
                    );
                    ui.label(format!(
                        "{} / {} bytes",
                        transferred, total
                    ));
                }

                UploadState::Done => {
                    ui.add(egui::ProgressBar::new(1.0).show_percentage());
                    ui.colored_label(
                        egui::Color32::from_rgb(80, 200, 80),
                        format!(
                            "✔  All certificates uploaded successfully (sec_tag: {})",
                            self.sec_tag
                        ),
                    );
                    ui.label(format!("  /storage/ca_{}.crt", self.sec_tag));
                    ui.label(format!("  /storage/client_{}.crt", self.sec_tag));
                    ui.label(format!("  /storage/client_{}.key", self.sec_tag));
                }

                UploadState::Error(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("✖  Error: {}", msg));
                }
            }
        });
    }
}

fn file_row(
    ui: &mut egui::Ui,
    label: &str,
    path: &mut String,
    filter: &str,
    disabled: bool,
) {
    ui.strong(label);
    ui.add_enabled(
        !disabled,
        egui::TextEdit::singleline(path)
            .desired_width(240.0)
            .hint_text("No file selected"),
    );
    let extensions: Vec<&str> = filter
        .split_whitespace()
        .map(|e| e.trim_start_matches("*."))
        .collect();
    if ui.add_enabled(!disabled, egui::Button::new("Browse…")).clicked() {
        if let Some(file) = rfd::FileDialog::new()
            .add_filter("Certificate files", &extensions)
            .pick_file()
        {
            *path = file.to_string_lossy().to_string();
        }
    }
}

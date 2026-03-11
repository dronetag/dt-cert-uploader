use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use dt_cert_uploader_core::{
    list_serial_ports, upload_certificates, validate_cert_files, DeviceType, UploadParams,
    UploadProgress,
};
use std::time::Instant;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dronetag Certificate Upload")
            .with_inner_size([540.0, 420.0])
            .with_min_inner_size([540.0, 420.0]),
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
    Connecting,
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
    device_type: DeviceType,
    available_ports: Vec<String>,

    // Cert files
    ca_path: String,
    client_cert_path: String,
    client_key_path: String,

    // Security tag
    sec_tag: u8,

    // Upload state (shared with worker thread)
    upload_state: Arc<Mutex<UploadState>>,

    last_port_refresh: Instant,
}

impl App {
    fn new() -> Self {
        let available_ports = list_serial_ports();
        let port = available_ports.first().cloned().unwrap_or_default();

        Self {
            port,
            device_type: DeviceType::DronetagTransmitter,
            available_ports,
            ca_path: String::new(),
            client_cert_path: String::new(),
            client_key_path: String::new(),
            sec_tag: 1,
            upload_state: Arc::new(Mutex::new(UploadState::Idle)),
            last_port_refresh: Instant::now(),
        }
    }

    fn is_uploading(&self) -> bool {
        matches!(*self.upload_state.lock().unwrap(), UploadState::Connecting | UploadState::Running { .. })
    }

    fn start_upload(&self) {
        *self.upload_state.lock().unwrap() = UploadState::Connecting;

        let params = UploadParams {
            port: self.port.clone(),
            device_type: self.device_type,
            sec_tag: self.sec_tag,
            ca_path: self.ca_path.clone(),
            client_cert_path: self.client_cert_path.clone(),
            client_key_path: self.client_key_path.clone(),
        };

        eprintln!(
            "[DEBUG] port='{}' device='{}' mux=0x{:02X} baud={}",
            params.port,
            params.device_type.display_name(),
            params.device_type.mux_addr(),
            params.device_type.baud_rate(),
        );

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
        // Auto-refresh port list every 500ms
        if self.last_port_refresh.elapsed() > std::time::Duration::from_millis(500) {
            self.available_ports = list_serial_ports();
            if !self.available_ports.contains(&self.port) {
                self.port = self.available_ports.first().cloned().unwrap_or_default();
            }
            self.last_port_refresh = Instant::now();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

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

                    ui.strong("Device:");
                    egui::ComboBox::from_id_salt("device_combo")
                        .selected_text(format!(
                            "{} (0x{:02X}, {} baud)",
                            self.device_type.display_name(),
                            self.device_type.mux_addr(),
                            self.device_type.baud_rate()
                        ))
                        .width(260.0)
                        .show_ui(ui, |ui| {
                            for d in DeviceType::all() {
                                ui.selectable_value(
                                    &mut self.device_type,
                                    *d,
                                    format!(
                                        "{} (0x{:02X}, {} baud)",
                                        d.display_name(),
                                        d.mux_addr(),
                                        d.baud_rate()
                                    ),
                                );
                            }
                        });
                    ui.end_row();

                    ui.strong("Security tag:");
                    ui.horizontal(|ui| {
                        for tag in 1u8..=9 {
                            if ui.radio(self.sec_tag == tag, tag.to_string()).clicked() {
                                self.sec_tag = tag;
                                // Reset state so stale success message clears
                                let mut s = self.upload_state.lock().unwrap();
                                if matches!(*s, UploadState::Done | UploadState::Error(_)) {
                                    *s = UploadState::Idle;
                                }
                            }
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

            // --- Upload button ---
            let ready = !self.port.is_empty()
                && !self.ca_path.is_empty()
                && !self.client_cert_path.is_empty()
                && !self.client_key_path.is_empty()
                && !uploading;

            if ui
                .add_enabled(
                    ready,
                    egui::Button::new("⬆  Upload Certificates")
                        .min_size(egui::vec2(180.0, 32.0)),
                )
                .clicked()
            {
                // Validate files first — no serial port opened yet
                match validate_cert_files(&UploadParams {
                    port: self.port.clone(),
                    device_type: self.device_type,
                    sec_tag: self.sec_tag,
                    ca_path: self.ca_path.clone(),
                    client_cert_path: self.client_cert_path.clone(),
                    client_key_path: self.client_key_path.clone(),
                }) {
                    Err(e) => {
                        *self.upload_state.lock().unwrap() = UploadState::Error(e);
                    }
                    Ok(()) => {
                        *self.upload_state.lock().unwrap() = UploadState::Idle;
                        self.start_upload();
                    }
                }
            }

            ui.add_space(8.0);

            // --- Status area ---
            let state = self.upload_state.lock().unwrap().clone();
            match state {
                UploadState::Idle => {}

                UploadState::Connecting => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting to device...");
                    });
                }

                UploadState::Running {
                    file_index,
                    file_label,
                    remote_path,
                    transferred,
                    total,
                } => {
                    if file_index == usize::MAX {
                        // Still initializing, show spinner
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Initializing...");
                        });
                    } else {
                        let overall_pct =
                            (file_index as f32) / 3.0 + (transferred as f32 / total as f32) / 3.0;
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
                        ui.label(format!("{} / {} bytes", transferred, total));
                    }
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
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 80, 80),
                        format!("✖  Error: {}", msg),
                    );
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
    if ui
        .add_enabled(!disabled, egui::Button::new("Browse…"))
        .clicked()
    {
        if let Some(file) = rfd::FileDialog::new()
            .add_filter("Certificate files", &extensions)
            .pick_file()
        {
            *path = file.to_string_lossy().to_string();
        }
    }
}
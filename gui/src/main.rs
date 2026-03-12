use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;
use dt_cert_uploader_core::{
    list_serial_ports, upload_certificates, validate_cert_files, read_settings, write_settings,
    DeviceType, UploadParams, UploadProgress,
};
use std::time::Instant;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Dronetag Certificate Upload")
            .with_inner_size([520.0, 520.0])
            .with_min_inner_size([520.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "dt-cert-uploader",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    )
}

// --- Upload state ---

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

// --- Settings state ---
#[derive(Clone, PartialEq)]
enum SettingsState {
    Idle,
    Busy,
    Done,
    Error(String),
}

// --- MQTT settings fields ---

#[derive(Clone)]
struct MqttSettings {
    dns_addr: String,
    ipaddr: String,
    port: String,
    sec_tag: String,
    user_name: String,
    password: String,
    telemetry_topic: String,
    status_topic: String,
    f_start_topic: String,
    f_end_topic: String,
    r_start_topic: String,
    r_end_topic: String,
}

impl Default for MqttSettings {
    fn default() -> Self {
        Self {
            dns_addr: "dt-hub-2.azure-devices.net".to_string(),
            ipaddr: String::new(),
            port: "8883".to_string(),
            sec_tag: "1".to_string(),
            user_name: String::new(),
            password: String::new(),
            telemetry_topic: "device/%s/telem".to_string(),
            status_topic: "device/%s/status".to_string(),
            f_start_topic: "device/%s/f_start".to_string(),
            f_end_topic: "device/%s/f_end".to_string(),
            r_start_topic: "device/%s/r_start".to_string(),
            r_end_topic: "device/%s/r_end".to_string(),
        }
    }
}

impl MqttSettings {
    fn from_json(val: &serde_json::Value) -> Self {
        let get_str = |key: &str, default: &str| -> String {
            val.get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(default)
                .to_string()
        };
        let get_num = |key: &str, default: &str| -> String {
            val.get(key)
                .map(|v| v.to_string())
                .unwrap_or(default.to_string())
        };

        Self {
            dns_addr:        get_str("dns_addr",        "dt-hub-2.azure-devices.net"),
            ipaddr:          get_str("ipaddr",          ""),
            port:            get_num("port",            "8883"),
            sec_tag:         get_num("sec_tag",         "1"),
            user_name:       get_str("user_name",       ""),
            password:        get_str("password",        ""),
            telemetry_topic: get_str("telemetry_topic", "device/%s/telem"),
            status_topic:    get_str("status_topic",    "device/%s/status"),
            f_start_topic:   get_str("f_start_topic",   "device/%s/f_start"),
            f_end_topic:     get_str("f_end_topic",     "device/%s/f_end"),
            r_start_topic:   get_str("r_start_topic",   "device/%s/r_start"),
            r_end_topic:     get_str("r_end_topic",     "device/%s/r_end"),
        }
    }

    fn to_json_string(&self) -> Result<String, String> {
        let port: u16 = self.port.trim().parse()
            .map_err(|_| format!("Invalid port number: '{}'", self.port))?;
        let sec_tag: u32 = self.sec_tag.trim().parse()
            .map_err(|_| format!("Invalid sec_tag: '{}'", self.sec_tag))?;

        let json = serde_json::json!({
            "nested": true,
            "save": true,
            "dt_cloud": {
                "cloud_client": "DT_TRANS_MQTT_CLIENT"
            },
            "dt_trans_mqtt": {
                "dns_addr":        self.dns_addr.trim(),
                "ipaddr":          self.ipaddr.trim(),
                "port":            port,
                "sec_tag":         sec_tag,
                "user_name":       self.user_name.trim(),
                "password":        self.password.trim(),
                "telemetry_topic": self.telemetry_topic.trim(),
                "status_topic":    self.status_topic.trim(),
                "f_start_topic":   self.f_start_topic.trim(),
                "f_end_topic":     self.f_end_topic.trim(),
                "r_start_topic":   self.r_start_topic.trim(),
                "r_end_topic":     self.r_end_topic.trim(),
            }
        });

        serde_json::to_string(&json).map_err(|e| e.to_string())
    }
}

// --- App ---

struct App {
    // Connection (shared between tabs)
    port: String,
    device_type: DeviceType,
    available_ports: Vec<String>,
    last_port_refresh: Instant,

    // Active tab
    active_tab: Tab,

    // --- Certificates tab ---
    ca_path: String,
    client_cert_path: String,
    client_key_path: String,
    sec_tag: u8,
    upload_state: Arc<Mutex<UploadState>>,

    // --- MQTT settings tab ---
    mqtt: MqttSettings,
    settings_state: Arc<Mutex<SettingsState>>,
}

#[derive(PartialEq)]
enum Tab {
    Certificates,
    MqttSettings,
}

impl App {
    fn new() -> Self {
        let available_ports = list_serial_ports();
        let port = available_ports.first().cloned().unwrap_or_default();

        Self {
            port,
            device_type: DeviceType::DronetagTransmitter,
            available_ports,
            last_port_refresh: Instant::now(),
            active_tab: Tab::Certificates,
            ca_path: String::new(),
            client_cert_path: String::new(),
            client_key_path: String::new(),
            sec_tag: 1,
            upload_state: Arc::new(Mutex::new(UploadState::Idle)),
            mqtt: MqttSettings::default(),
            settings_state: Arc::new(Mutex::new(SettingsState::Idle)),
        }
    }

    fn is_uploading(&self) -> bool {
        matches!(
            *self.upload_state.lock().unwrap(),
            UploadState::Connecting | UploadState::Running { .. }
        )
    }

    fn is_settings_busy(&self) -> bool {
        matches!(*self.settings_state.lock().unwrap(), SettingsState::Busy)
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

    fn start_write_settings(&self, json: String) {
        *self.settings_state.lock().unwrap() = SettingsState::Busy;
        let port = self.port.clone();
        let device_type = self.device_type;
        let state = Arc::clone(&self.settings_state);

        thread::spawn(move || {
            let result = write_settings(&port, &device_type, &json);
            let mut s = state.lock().unwrap();
            *s = match result {
                Ok(()) => SettingsState::Done,
                Err(e) => SettingsState::Error(e),
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

        if self.is_uploading() || self.is_settings_busy() {
            ctx.request_repaint();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Dronetag Certificate Upload");
            ui.add_space(8.0);

            let busy = self.is_uploading() || self.is_settings_busy();

            // --- Shared connection bar ---
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
                });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            // --- Tabs ---
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.active_tab, Tab::Certificates, "🔒 TLS Certificates");
                ui.selectable_value(&mut self.active_tab, Tab::MqttSettings, "⚙  MQTT Settings");
            });
            ui.separator();
            ui.add_space(8.0);

            match self.active_tab {
                Tab::Certificates => self.show_certificates_tab(ui, busy),
                Tab::MqttSettings => self.show_mqtt_tab(ui, busy, ctx),
            }
        });
    }
}

impl App {
    fn show_certificates_tab(&mut self, ui: &mut egui::Ui, busy: bool) {
        let uploading = self.is_uploading();

        // Security tag
        ui.horizontal(|ui| {
            ui.strong("Security tag:");
            for tag in 1u8..=9 {
                if ui.radio(self.sec_tag == tag, tag.to_string()).clicked() {
                    self.sec_tag = tag;
                    let mut s = self.upload_state.lock().unwrap();
                    if matches!(*s, UploadState::Done | UploadState::Error(_)) {
                        *s = UploadState::Idle;
                    }
                }
            }
        });
        ui.add_space(8.0);

        // File pickers
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

        let ready = !self.port.is_empty()
            && !self.ca_path.is_empty()
            && !self.client_cert_path.is_empty()
            && !self.client_key_path.is_empty()
            && !busy;

        if ui
            .add_enabled(
                ready,
                egui::Button::new("⬆  Upload Certificates").min_size(egui::vec2(180.0, 32.0)),
            )
            .clicked()
        {
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

        // Status area
        let state = self.upload_state.lock().unwrap().clone();
        match state {
            UploadState::Idle => {}
            UploadState::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Connecting to device...");
                });
            }
            UploadState::Running { file_index, file_label, remote_path, transferred, total } => {
                if file_index == usize::MAX {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Initializing...");
                    });
                } else {
                    let overall_pct =
                        (file_index as f32) / 3.0 + (transferred as f32 / total as f32) / 3.0;
                    ui.label(format!("[{}/3] {} → {}", file_index + 1, file_label, remote_path));
                    ui.add(egui::ProgressBar::new(overall_pct).show_percentage().animate(true));
                    ui.label(format!("{} / {} bytes", transferred, total));
                }
            }
            UploadState::Done => {
                ui.add(egui::ProgressBar::new(1.0).show_percentage());
                ui.colored_label(
                    egui::Color32::from_rgb(80, 200, 80),
                    format!("✔  All certificates uploaded successfully (sec_tag: {})", self.sec_tag),
                );
                ui.label(format!("  /storage/ca_{}.crt", self.sec_tag));
                ui.label(format!("  /storage/client_{}.crt", self.sec_tag));
                ui.label(format!("  /storage/client_{}.key", self.sec_tag));
            }
            UploadState::Error(msg) => {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("✖  Error: {}", msg));
            }
        }
    }

    fn show_mqtt_tab(&mut self, ui: &mut egui::Ui, busy: bool, ctx: &egui::Context) {
        let disabled = busy;

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("mqtt_grid")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .min_col_width(140.0)
                .show(ui, |ui| {
                    mqtt_row(ui, "DNS Address:",       &mut self.mqtt.dns_addr,        disabled);
                    mqtt_row(ui, "IP Address:",        &mut self.mqtt.ipaddr,          disabled);
                    mqtt_row(ui, "Port:",              &mut self.mqtt.port,            disabled);
                    mqtt_row(ui, "Security Tag:",      &mut self.mqtt.sec_tag,         disabled);
                    mqtt_row(ui, "Username:",          &mut self.mqtt.user_name,       disabled);
                    mqtt_row(ui, "Password:",          &mut self.mqtt.password,        disabled);
                    mqtt_row(ui, "Telemetry Topic:",   &mut self.mqtt.telemetry_topic, disabled);
                    mqtt_row(ui, "Status Topic:",      &mut self.mqtt.status_topic,    disabled);
                    mqtt_row(ui, "F-Start Topic:",     &mut self.mqtt.f_start_topic,   disabled);
                    mqtt_row(ui, "F-End Topic:",       &mut self.mqtt.f_end_topic,     disabled);
                    mqtt_row(ui, "R-Start Topic:",     &mut self.mqtt.r_start_topic,   disabled);
                    mqtt_row(ui, "R-End Topic:",       &mut self.mqtt.r_end_topic,     disabled);
                });

            ui.add_space(12.0);

            ui.horizontal(|ui| {
                // Read settings button
                if ui
                    .add_enabled(
                        !busy && !self.port.is_empty(),
                        egui::Button::new("📥  Read Settings").min_size(egui::vec2(140.0, 32.0)),
                    )
                    .clicked()
                {
                    *self.settings_state.lock().unwrap() = SettingsState::Busy;
                    let port = self.port.clone();
                    let device_type = self.device_type;
                    let state = Arc::clone(&self.settings_state);
                    let mqtt_result: Arc<Mutex<Option<Result<String, String>>>> =
                        Arc::new(Mutex::new(None));
                    let mqtt_result_clone = Arc::clone(&mqtt_result);
                    let ctx_clone = ctx.clone();

                    // Store result handle in app data so update() can pick it up
                    ctx.data_mut(|d| {
                        d.insert_temp(egui::Id::new("mqtt_read_result"), mqtt_result.clone());
                    });

                    thread::spawn(move || {
                        let result = read_settings(&port, &device_type);
                        *mqtt_result_clone.lock().unwrap() = Some(result.clone());
                        *state.lock().unwrap() = match result {
                            Ok(_) => SettingsState::Done,
                            Err(ref e) => SettingsState::Error(e.clone()),
                        };
                        ctx_clone.request_repaint();
                    });
                }

                // Write settings button
                if ui
                    .add_enabled(
                        !busy && !self.port.is_empty(),
                        egui::Button::new("📤  Write Settings").min_size(egui::vec2(140.0, 32.0)),
                    )
                    .clicked()
                {
                    match self.mqtt.to_json_string() {
                        Err(e) => {
                            *self.settings_state.lock().unwrap() = SettingsState::Error(e);
                        }
                        Ok(json) => {
                            self.start_write_settings(json);
                        }
                    }
                }
            });

            ui.add_space(8.0);

            // Poll for read result and apply to fields
            let maybe_result: Option<Arc<Mutex<Option<Result<String, String>>>>> =
                ctx.data(|d| d.get_temp(egui::Id::new("mqtt_read_result")));

            if let Some(result_arc) = maybe_result {
                let mut guard = result_arc.lock().unwrap();
                if let Some(result) = guard.take() {
                    // Clear the handle
                    ctx.data_mut(|d| {
                        d.remove::<Arc<Mutex<Option<Result<String, String>>>>>(egui::Id::new("mqtt_read_result"));
                    });
                    match result {
                        Ok(json) => {
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) {
                                if let Some(mqtt_val) = parsed.get("dt_trans_mqtt") {
                                    self.mqtt = MqttSettings::from_json(mqtt_val);
                                }
                            }
                        }
                        Err(_) => {} // error already in settings_state
                    }
                }
            }

            // Status
            let state = self.settings_state.lock().unwrap().clone();
            match state {
                SettingsState::Idle => {}
                SettingsState::Busy => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Communicating with device...");
                    });
                }
                SettingsState::Done => {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "✔  Done.");
                }
                SettingsState::Error(msg) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("✖  Error: {}", msg));
                }
            }
        });
    }
}

fn file_row(ui: &mut egui::Ui, label: &str, path: &mut String, filter: &str, disabled: bool) {
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

fn mqtt_row(ui: &mut egui::Ui, label: &str, value: &mut String, disabled: bool) {
    ui.strong(label);
    ui.add_enabled(
        !disabled,
        egui::TextEdit::singleline(value).desired_width(300.0),
    );
    ui.end_row();
}
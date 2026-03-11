use std::{
    io::{self, Read, Write},
    time::Duration,
};

use mcumgr_toolkit::MCUmgrClient;
use serialport::SerialPort;

// --- SLIP constants ---
const SLIP_END: u8 = 0x0A;
const SLIP_ESC: u8 = 0xDB;
const SLIP_ESC_END: u8 = 0xDC;
const SLIP_ESC_ESC: u8 = 0xDD;

/// Supported device types with their mux address and baud rate
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeviceType {
    DronetagTransmitter,
    DronetagRider,
}

impl DeviceType {
    pub fn mux_addr(&self) -> u8 {
        match self {
            DeviceType::DronetagTransmitter => 0x11,
            DeviceType::DronetagRider => 0x23,
        }
    }

    pub fn settings_mux_addr(&self) -> u8 {
        match self {
            DeviceType::DronetagTransmitter => 0x13,
            DeviceType::DronetagRider => 0x25,
        }
    }

    pub fn baud_rate(&self) -> u32 {
        match self {
            DeviceType::DronetagTransmitter => 500_000,
            DeviceType::DronetagRider => 115_200,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            DeviceType::DronetagTransmitter => "Dronetag Transmitter",
            DeviceType::DronetagRider => "Dronetag RIDER",
        }
    }

    pub fn all() -> &'static [DeviceType] {
        &[DeviceType::DronetagTransmitter, DeviceType::DronetagRider]
    }
}

// --- Mux+SLIP serial wrapper ---
pub struct MuxSlipSerial {
    port: Box<dyn SerialPort>,
    read_buf: Vec<u8>,
    raw_buf: Vec<u8>,
    mux_addr: u8,
}

impl MuxSlipSerial {
    pub fn new(port: Box<dyn SerialPort>, mux_addr: u8) -> Self {
        Self {
            port,
            read_buf: Vec::new(),
            raw_buf: Vec::new(),
            mux_addr,
        }
    }

    fn slip_encode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 2);
        for &byte in data {
            match byte {
                SLIP_ESC => {
                    out.push(SLIP_ESC);
                    out.push(SLIP_ESC_ESC);
                }
                SLIP_END => {
                    out.push(SLIP_ESC);
                    out.push(SLIP_ESC_END);
                }
                b => out.push(b),
            }
        }
        out.push(SLIP_END);
        out
    }

    fn slip_decode(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            match data[i] {
                SLIP_ESC if i + 1 < data.len() => {
                    match data[i + 1] {
                        SLIP_ESC_END => out.push(SLIP_END),
                        SLIP_ESC_ESC => out.push(SLIP_ESC),
                        b => out.push(b),
                    }
                    i += 2;
                }
                SLIP_END => {
                    i += 1;
                }
                b => {
                    out.push(b);
                    i += 1;
                }
            }
        }
        out
    }

    fn fill_read_buf(&mut self) -> io::Result<()> {
        loop {
            let mut byte = [0u8; 1];
            self.port.read_exact(&mut byte)?;
            self.raw_buf.push(byte[0]);

            if byte[0] == SLIP_END && !self.raw_buf.is_empty() {
                let decoded = Self::slip_decode(&self.raw_buf);
                self.raw_buf.clear();
                if decoded.len() > 1 {
                    self.read_buf.extend_from_slice(&decoded[1..]);
                }
                return Ok(());
            }
        }
    }

    /// Read one complete SLIP frame and return (mux_addr, payload).
    /// Unlike `read()`, this does not filter by address — caller decides.
    pub fn read_raw_frame(&mut self) -> io::Result<(u8, Vec<u8>)> {
        loop {
            let mut byte = [0u8; 1];
            self.port.read_exact(&mut byte)?;
            self.raw_buf.push(byte[0]);

            if byte[0] == SLIP_END && !self.raw_buf.is_empty() {
                let decoded = Self::slip_decode(&self.raw_buf);
                self.raw_buf.clear();

                if decoded.len() > 1 {
                    let addr = decoded[0];
                    let payload = decoded[1..].to_vec();
                    return Ok((addr, payload));
                }
                // Empty or malformed frame — try next one
            }
        }
    }
}

impl Read for MuxSlipSerial {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.read_buf.is_empty() {
            self.fill_read_buf()?;
        }
        let n = buf.len().min(self.read_buf.len());
        buf[..n].copy_from_slice(&self.read_buf[..n]);
        self.read_buf.drain(..n);
        Ok(n)
    }
}

impl Write for MuxSlipSerial {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut payload = Vec::with_capacity(1 + buf.len());
        payload.push(self.mux_addr);
        payload.extend_from_slice(buf);
        let encoded = Self::slip_encode(&payload);
        self.port.write_all(&encoded)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

impl mcumgr_toolkit::transport::serial::ConfigurableTimeout for MuxSlipSerial {
    fn set_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.port
            .set_timeout(timeout)
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
    }
}

// --- Public types ---

/// The three certificate files to upload, plus connection parameters.
pub struct UploadParams {
    pub port: String,
    pub device_type: DeviceType,
    pub sec_tag: u8,
    pub ca_path: String,
    pub client_cert_path: String,
    pub client_key_path: String,
}

/// Progress update sent back to the caller during upload.
pub struct UploadProgress {
    /// Which file is currently being uploaded (0-based index out of 3)
    pub file_index: usize,
    /// Human-readable label for current file
    pub file_label: String,
    /// Remote path being written to
    pub remote_path: String,
    /// Bytes transferred for the current file
    pub transferred: u64,
    /// Total bytes for the current file
    pub total: u64,
}

/// A single file to upload: (local path, remote path, label)
fn build_file_list(params: &UploadParams) -> Vec<(String, String, String)> {
    vec![
        (
            params.ca_path.clone(),
            format!("/storage/ca_{}.crt", params.sec_tag),
            "CA Certificate".to_string(),
        ),
        (
            params.client_cert_path.clone(),
            format!("/storage/client_{}.crt", params.sec_tag),
            "Client Certificate".to_string(),
        ),
        (
            params.client_key_path.clone(),
            format!("/storage/client_{}.key", params.sec_tag),
            "Client Private Key".to_string(),
        ),
    ]
}

/// Maximum allowed certificate file size (5 KB — 4 KB typical + 25% reserve)
pub const MAX_CERT_FILE_SIZE: u64 = 5 * 1024;

/// Validate all certificate file paths and sizes before attempting upload.
pub fn validate_cert_files(params: &UploadParams) -> Result<(), String> {
    let files = [
        (&params.ca_path, "CA Certificate"),
        (&params.client_cert_path, "Client Certificate"),
        (&params.client_key_path, "Client Private Key"),
    ];

    for (path, label) in &files {
        let metadata = std::fs::metadata(path)
            .map_err(|e| format!("{}: cannot read file '{}': {}", label, path, e))?;

        let size = metadata.len();
        if size == 0 {
            return Err(format!("{}: file '{}' is empty", label, path));
        }
        if size > MAX_CERT_FILE_SIZE {
            return Err(format!(
                "{}: file '{}' is too large ({} bytes, maximum is {} bytes / {} KB)",
                label, path, size, MAX_CERT_FILE_SIZE, MAX_CERT_FILE_SIZE / 1024
            ));
        }
    }

    Ok(())
}

/// Connect to the device and upload all three certificate files.
///
/// `progress_cb` is called with progress updates during upload.
/// Return `false` from it to abort the upload.
pub fn upload_certificates(
    params: &UploadParams,
    mut progress_cb: impl FnMut(UploadProgress) -> bool,
) -> Result<(), String> {
    validate_cert_files(params)?;

    // Use a short open timeout so we fail fast on wrong/busy ports
    let port = serialport::new(&params.port, params.device_type.baud_rate())
        .timeout(Duration::from_secs(2))
        .open()
        .map_err(|e| format!("Failed to open serial port '{}': {}", params.port, e))?;

    let client = MCUmgrClient::new_from_serial(
        MuxSlipSerial::new(port, params.device_type.mux_addr())
    );

    // Signal to UI that we are connected and starting
    progress_cb(UploadProgress {
        file_index: usize::MAX,
        file_label: "Initializing...".to_string(),
        remote_path: String::new(),
        transferred: 0,
        total: 1,
    });

    // Fail fast — no retries, short timeout to detect incorrect device connected
    client.set_retries(0);
    client.set_timeout(Duration::from_secs(2))
    .unwrap_or_else(|e| eprintln!("Warning: could not set timeout: {}", e));

    client.use_auto_frame_size().map_err(|e| {
        format!("Device did not respond (wrong port, device type, or mux address?): {}", e)
    })?;

    let files = build_file_list(params);

    for (file_index, (local_path, remote_path, label)) in files.iter().enumerate() {
        let data = std::fs::read(local_path)
            .map_err(|e| format!("Failed to read '{}': {}", local_path, e))?;

        let size = data.len() as u64;
        let reader = std::io::Cursor::new(data);
        let remote_path_clone = remote_path.clone();
        let label_clone = label.clone();

        client
            .fs_file_upload(
                remote_path,
                reader,
                size,
                Some(&mut |transferred, total| {
                    progress_cb(UploadProgress {
                        file_index,
                        file_label: label_clone.clone(),
                        remote_path: remote_path_clone.clone(),
                        transferred,
                        total,
                    })
                }),
            )
            .map_err(|e| format!("Failed to upload '{}': {}", local_path, e))?;
    }

    Ok(())
}

/// Returns a list of available serial port names on the current system.
pub fn list_serial_ports() -> Vec<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.port_name)
        .collect()
}

/// Read device settings by sending an empty JSON `{}` to the settings mux address
/// and reassembling the JSON response (which may span multiple SLIP frames).
pub fn read_settings(port_name: &str, device_type: &DeviceType) -> Result<String, String> {
    let port = serialport::new(port_name, device_type.baud_rate())
        .timeout(Duration::from_secs(4))
        .open()
        .map_err(|e| format!("Failed to open serial port '{}': {}", port_name, e))?;

    let settings_mux_addr = device_type.settings_mux_addr();
    let mut slip = MuxSlipSerial::new(port, settings_mux_addr);

    // Send empty JSON request
    slip.write_all(b"{\"nested\": true}")
        .map_err(|e| format!("Failed to send settings request: {}", e))?;
    slip.flush()
        .map_err(|e| format!("Failed to flush: {}", e))?;

    // Reassemble JSON response — filter to settings channel only
    let mut json_buf = String::new();
    let mut brace_count: i32 = 0;
    let mut in_json = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(4);

    loop {
        if std::time::Instant::now() > deadline {
            return Err("Timeout waiting for settings response from device".to_string());
        }

        let (addr, payload) = slip.read_raw_frame()
            .map_err(|e| format!("Failed to read frame: {}", e))?;

        // Ignore frames not addressed to the settings channel
        if addr != settings_mux_addr {
            continue;
        }

        let chunk = std::str::from_utf8(&payload)
            .unwrap_or("")
            .to_string();

        for ch in chunk.chars() {
            match ch {
                '{' => { brace_count += 1; in_json = true; }
                '}' => { brace_count -= 1; }
                _ => {}
            }
        }
        json_buf.push_str(&chunk);

        if in_json && brace_count == 0 {
            return Ok(json_buf);
        }
    }
}
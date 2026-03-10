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

const MCUMGR_MUX_ADDR: u8 = 0x11;

// --- Mux+SLIP serial wrapper ---

pub struct MuxSlipSerial {
    port: Box<dyn SerialPort>,
    read_buf: Vec<u8>,
    raw_buf: Vec<u8>,
}

impl MuxSlipSerial {
    pub fn new(port: Box<dyn SerialPort>) -> Self {
        Self {
            port,
            read_buf: Vec::new(),
            raw_buf: Vec::new(),
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
        payload.push(MCUMGR_MUX_ADDR);
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
    pub baud_rate: u32,
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

/// Connect to the device and upload all three certificate files.
///
/// `progress_cb` is called with progress updates during upload.
/// Return `false` from it to abort the upload.
pub fn upload_certificates(
    params: &UploadParams,
    mut progress_cb: impl FnMut(UploadProgress) -> bool,
) -> Result<(), String> {
    let port = serialport::new(&params.port, params.baud_rate)
        .timeout(Duration::from_secs(5))
        .open()
        .map_err(|e| format!("Failed to open serial port '{}': {}", params.port, e))?;

    let client = MCUmgrClient::new_from_serial(MuxSlipSerial::new(port));

    client.use_auto_frame_size().unwrap_or_else(|e| {
        eprintln!("Warning: could not read auto frame size, using default. ({})", e);
    });

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

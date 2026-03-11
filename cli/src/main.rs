use clap::Parser;
use dt_cert_uploader_core::{upload_certificates, validate_cert_files, UploadParams, UploadProgress, DeviceType};

/// Upload TLS certificates to a Zephyr device via MCUmgr over serial.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Serial port to use (e.g. /dev/ttyACM0 on Linux, COM3 on Windows, /dev/tty.usbmodem... on macOS)
    #[arg(short, long)]
    port: String,

    /// Device type: transmitter (mux 0x11, 500000 baud), rider (mux 0x23, 115200 baud)
    #[arg(short, long, default_value = "transmitter", value_parser = parse_device_type)]
    device: DeviceType,

    /// Security tag (1-9)
    #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=9))]
    sec_tag: u8,

    /// Path to the CA certificate file
    #[arg(long)]
    ca: String,

    /// Path to the client certificate file
    #[arg(long)]
    client_cert: String,

    /// Path to the client private key file
    #[arg(long)]
    client_key: String,
}

fn parse_device_type(s: &str) -> Result<DeviceType, String> {
    match s.to_lowercase().as_str() {
        "transmitter" => Ok(DeviceType::DronetagTransmitter),
        "rider"       => Ok(DeviceType::DronetagRider),
        _ => Err(format!("Unknown device type '{}'. Valid options: transmitter, rider", s)),
    }
}

fn main() {
    let cli = Cli::parse();

    let params = UploadParams {
        port: cli.port.clone(),
        device_type: cli.device,
        sec_tag: cli.sec_tag,
        ca_path: cli.ca,
        client_cert_path: cli.client_cert,
        client_key_path: cli.client_key,
    };

    if let Err(e) = validate_cert_files(&params) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    println!("Device:       {} (mux 0x{:02X}, {} baud)",
        params.device_type.display_name(),
        params.device_type.mux_addr(),
        params.device_type.baud_rate()
    );
    println!("Security tag: {}\n", params.sec_tag);
    println!("Uploading...");

    let result = upload_certificates(&params, |_progress: UploadProgress| {
        true
    });

    match result {
        Ok(()) => {
            println!("Done.\n");
            println!("All certificates uploaded successfully.");
            println!("  CA cert:     /storage/ca_{}.crt", params.sec_tag);
            println!("  Client cert: /storage/client_{}.crt", params.sec_tag);
            println!("  Client key:  /storage/client_{}.key", params.sec_tag);
        }
        Err(e) => {
            eprintln!("\nError: {}", e);
            std::process::exit(1);
        }
    }
}
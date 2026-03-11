use clap::Parser;
use dt_cert_uploader_core::{upload_certificates, validate_cert_files, read_settings, UploadParams, UploadProgress, DeviceType};
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
    sec_tag: Option<u8>,

    /// Path to the CA certificate file
    #[arg(long)]
    ca: Option<String>,

    /// Path to the client certificate file
    #[arg(long)]
    client_cert: Option<String>,

    /// Path to the client private key file
    #[arg(long)]
    client_key: Option<String>,

    /// Read and print current device settings as JSON
    #[arg(long)]
    read_settings: bool,
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

    // --- Read settings mode ---
    if cli.read_settings {
        println!("Reading settings from {} on '{}'...\n",
            cli.device.display_name(), cli.port);

        match read_settings(&cli.port, &cli.device) {
            Ok(json) => {
                match serde_json::from_str::<serde_json::Value>(&json) {
                    Ok(parsed) => {
                        let mut output = serde_json::Map::new();

                        // Filter out only the relevant settings
                        if let Some(dt_cloud) = parsed.get("dt_cloud") {
                            let mut filtered_cloud = serde_json::Map::new();
                            if let Some(cloud_client) = dt_cloud.get("cloud_client") {
                                filtered_cloud.insert("cloud_client".to_string(), cloud_client.clone());
                            }
                            output.insert("dt_cloud".to_string(), serde_json::Value::Object(filtered_cloud));
                        }
                        if let Some(dt_trans_mqtt) = parsed.get("dt_trans_mqtt") {
                            output.insert("dt_trans_mqtt".to_string(), dt_trans_mqtt.clone());
                        }

                        println!("{}", serde_json::to_string_pretty(&serde_json::Value::Object(output)).unwrap());
                    }
                    Err(e) => {
                        eprintln!("Failed to parse JSON response: {}", e);
                        eprintln!("Raw response:\n{}", json);
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    // --- Upload mode ---
    let sec_tag = cli.sec_tag.unwrap_or_else(|| {
        eprintln!("Error: --sec-tag is required for certificate upload");
        std::process::exit(1);
    });
    let ca = cli.ca.unwrap_or_else(|| {
        eprintln!("Error: --ca is required for certificate upload");
        std::process::exit(1);
    });
    let client_cert = cli.client_cert.unwrap_or_else(|| {
        eprintln!("Error: --client-cert is required for certificate upload");
        std::process::exit(1);
    });
    let client_key = cli.client_key.unwrap_or_else(|| {
        eprintln!("Error: --client-key is required for certificate upload");
        std::process::exit(1);
    });

    let params = UploadParams {
        port: cli.port.clone(),
        device_type: cli.device,
        sec_tag: sec_tag,
        ca_path: ca,
        client_cert_path: client_cert,
        client_key_path: client_key,
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

    let result = upload_certificates(&params, |_progress: UploadProgress| true);

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
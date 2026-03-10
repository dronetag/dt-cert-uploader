use std::io::Write;

use clap::Parser;
use dt_cert_uploader_core::{upload_certificates, UploadParams, UploadProgress};

/// Upload TLS certificates to a Zephyr device via MCUmgr over serial.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Serial port to use (e.g. /dev/ttyACM0 on Linux, COM3 on Windows, /dev/tty.usbmodem... on macOS)
    #[arg(short, long)]
    port: String,

    /// Baud rate
    #[arg(short, long, default_value_t = 500000)]
    baud_rate: u32,

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

fn main() {
    let cli = Cli::parse();

    let params = UploadParams {
        port: cli.port.clone(),
        baud_rate: cli.baud_rate,
        sec_tag: cli.sec_tag,
        ca_path: cli.ca,
        client_cert_path: cli.client_cert,
        client_key_path: cli.client_key,
    };

    println!("Connecting to '{}' at {} baud...", params.port, params.baud_rate);
    println!("Security tag: {}\n", params.sec_tag);

    let mut last_file_index = usize::MAX;

    let result = upload_certificates(&params, |progress: UploadProgress| {
        // Print header when we move to a new file
        if progress.file_index != last_file_index {
            if last_file_index != usize::MAX {
                println!("\r  Done.                              ");
            }
            println!(
                "[{}/3] {} -> {}",
                progress.file_index + 1,
                progress.file_label,
                progress.remote_path
            );
            last_file_index = progress.file_index;
        }

        let pct = progress.transferred * 100 / progress.total;
        print!(
            "\r  {}% ({}/{} bytes)  ",
            pct, progress.transferred, progress.total
        );
        let _ = std::io::stdout().flush();

        true // return false to abort
    });

    match result {
        Ok(()) => {
            println!("\r  Done.                              ");
            println!("\nAll certificates uploaded successfully.");
            println!("  CA cert:     /storage/ca_{}.crt", params.sec_tag);
            println!("  Client cert: /storage/client_{}.crt", params.sec_tag);
            println!("  Client key:  /storage/client_{}.key", params.sec_tag);
        }
        Err(e) => {
            eprintln!("\nError: {}", e);
            eprintln!("Hint: on Linux, ensure you are in the 'dialout' group:");
            eprintln!("  sudo usermod -a -G dialout $USER");
            std::process::exit(1);
        }
    }
}

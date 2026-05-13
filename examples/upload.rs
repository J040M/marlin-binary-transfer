//! End-to-end CLI upload tool, mirroring the Python reference's
//! `transfer.py`.
//!
//! Run with the `blocking,serial,heatshrink` features:
//!
//! ```sh
//! cargo run --example upload \
//!     --features blocking,serial,heatshrink -- \
//!     --port /dev/ttyUSB0 --baud 250000 \
//!     --src ./model.gco --dest model.gco --compression auto
//! ```
//!
//! Args are positional-key-value style for simplicity; no clap dependency.

#[cfg(all(feature = "blocking", feature = "serial"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use marlin_binary_transfer::adapters::blocking::{upload, UploadOptions};
    use marlin_binary_transfer::adapters::serialport;
    use marlin_binary_transfer::file_transfer::Compression;

    let mut args = std::env::args().skip(1);
    let mut port = String::from("/dev/ttyUSB0");
    let mut baud: u32 = 250_000;
    let mut src = String::from("./test_file");
    let mut dest = String::from("test_file.gco");
    let mut compression = "none".to_string();

    while let Some(arg) = args.next() {
        let mut val = || args.next().unwrap_or_default();
        match arg.as_str() {
            "--port" => port = val(),
            "--baud" => baud = val().parse()?,
            "--src" => src = val(),
            "--dest" => dest = val(),
            "--compression" => compression = val(),
            "-h" | "--help" => {
                eprintln!(
                    "usage: upload --port <PATH> --baud <RATE> \\
                            --src <FILE> --dest <NAME> --compression <none|auto>"
                );
                return Ok(());
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let compression = match compression.as_str() {
        "none" => Compression::None,
        "auto" => Compression::Auto,
        other => {
            eprintln!("unsupported --compression value: {other}");
            std::process::exit(2);
        }
    };

    let mut transport = serialport::open(&port, baud)?;
    let file = std::fs::File::open(&src)?;
    let opts = UploadOptions {
        dest_filename: dest,
        compression,
        dummy: false,
        chunk_size: 0,
        progress: Some(Box::new(|p| {
            eprintln!(
                "  progress: {} chunks, {} bytes sent",
                p.chunks_sent, p.bytes_sent
            );
        })),
    };
    let stats = upload(&mut *transport, file, opts)?;
    eprintln!(
        "uploaded {} source bytes / {} sent bytes in {} chunks (compression: {:?})",
        stats.source_bytes, stats.bytes_sent, stats.chunks_sent, stats.compression
    );
    Ok(())
}

#[cfg(not(all(feature = "blocking", feature = "serial")))]
fn main() {
    eprintln!(
        "this example requires the `blocking` and `serial` features:\n\
         cargo run --example upload --features blocking,serial -- ..."
    );
    std::process::exit(2);
}

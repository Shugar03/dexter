//! Linux-capable twin of `dexter --driver browser eval harvest`.
//!
//! Run with:
//! `cargo run -p dexter-eval --example harvest -- <manifest> -o <out> [--browser-url http://localhost:9515]`

use dexter_browser::BrowserDriver;
use dexter_eval::{harvest::harvest_file, to_jsonl};
use std::error::Error;
use std::io;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut manifest = None;
    let mut out = None;
    let mut browser_url = "http://localhost:9515".to_string();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-o" => {
                out = Some(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "-o requires an output path")
                })?);
            }
            "--browser-url" => {
                browser_url = args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--browser-url requires a URL")
                })?;
            }
            _ if arg.starts_with('-') => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown argument '{arg}'"),
                )
                .into());
            }
            _ if manifest.is_none() => manifest = Some(arg),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "only one manifest path is allowed",
                )
                .into());
            }
        }
    }

    let manifest = manifest.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: harvest <manifest> -o <out> [--browser-url <url>]",
        )
    })?;
    let out = out.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: harvest <manifest> -o <out> [--browser-url <url>]",
        )
    })?;
    let driver = BrowserDriver::connect(&browser_url, "chrome")?;
    let items = harvest_file(&driver, Path::new(&manifest))?;
    std::fs::write(&out, to_jsonl(&items)?)?;
    println!("wrote {} items to {out}", items.len());
    Ok(())
}

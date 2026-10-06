//! Amber is a desktop resin slicer. The first machine it knows how to drive
//! is the Anycubic Photon M3 Max.

pub const VERSION: &str = "0.1.0";

pub mod app;
pub mod catalog;
pub mod cli;
pub mod community;
pub mod mesh;
pub mod plate;
pub mod pm3m;
pub mod printer;
pub mod resins;
pub mod scene;
pub mod sl1;
pub mod slice;
pub mod supports;
pub mod viewport;

pub fn start() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("slice") {
        return cli::run(&args[1..]);
    }
    if args.first().map(String::as_str) == Some("--help")
        || args.first().map(String::as_str) == Some("--version")
    {
        println!("Amber {VERSION}");
        println!("  amber                 open the window");
        println!("  amber slice <mesh> -o <file.pm3m> [--printer anycubic-photon-m3-max]");
        return Ok(());
    }
    app::run().map_err(|err| anyhow::anyhow!("{err}"))
}

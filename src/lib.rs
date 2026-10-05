//! Amber is a desktop resin slicer. The first machine it knows how to drive
//! is the Anycubic Photon M3 Max.

pub mod app;
pub mod catalog;
pub mod cli;
pub mod community;
pub mod mesh;
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
    if args.first().map(String::as_str) == Some("--help") {
        println!("Amber resin slicer\n  amber                 open the window\n  amber slice <mesh> -o <file.pm3m> [--printer anycubic-photon-m3-max]");
        return Ok(());
    }
    app::run().map_err(|err| anyhow::anyhow!("{err}"))
}

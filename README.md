# Amber

Amber is a desktop resin slicer for the **Anycubic Photon M3 Max**. It opens STL and OBJ models, lays them out on the 298.08 × 165.6 × 300 mm plate, and writes a Photon Workshop **v516** `.pm3m` file the printer can read from USB.

The window is a native app (Rust, egui, OpenGL). The same program builds on Windows and on Linux. It is not a web app.

## Run the Windows app

`amber.exe` in this folder is a 64-bit Windows program. Copy it to your PC and double-click it. It does not need Rust or an installer. Windows may warn that the file is unrecognized; that is the usual prompt for an app that is not signed.

From a terminal in the same folder you can also slice without opening the window:

```text
amber.exe slice model.stl -o model.pm3m --layer 0.05 --supports medium
```

Amber is not affiliated with Anycubic. Resin times below are Anycubic's published starting points, not a tuned profile for your bottle, temperature, or screen.

## First print on the M3 Max

1. Add the **20 mm cube** (File menu) and export it as a short name such as `cube.pm3m`. The printer skips very long filenames.
2. Leave **Rotate 180°** on. That matches the orientation Photonic Etcher uses for this machine. If the cube comes out mirrored or spun, flip Rotate 180°, Mirror X, or Mirror Y and print the cube again before a real part.
3. Run a **RERF** (resin exposure finder) with the resin you actually have. The presets are the Anycubic store guide from November 2023, all at 0.05 mm layers, 2.5 s light-off, and 6 bottom layers:

   | Resin | Exposure | Bottom | Lift | Lift speed | Retract |
   | --- | ---: | ---: | ---: | ---: | ---: |
   | Colored UV | 3 s | 50 s | 10 mm | 2 mm/s | 3 mm/s |
   | Plant-Based | 3 s | 50 s | 10 mm | 2 mm/s | 3 mm/s |
   | DLP Craftsman | 2 s | 35 s | 8 mm | 3 mm/s | 4 mm/s |
   | UV Tough | 3 s | 50 s | 10 mm | 2 mm/s | 3 mm/s |
   | Water-Wash+ | 3 s | 50 s | 10 mm | 2 mm/s | 3 mm/s |
   | ABS-Like+ | 3 s | 35 s | 10 mm | 2 mm/s | 2 mm/s |
   | ABS-Like Pro | 3 s | 35 s | 10 mm | 2 mm/s | 3 mm/s |
   | High Clear | 4.5 s | 30 s | 10 mm | 2 mm/s | 3 mm/s |

4. The overhang bridge is there to see whether supports hold a span. Do not start with a hollow model until the cube sticks.

## What the window does

- Import STL (binary or ASCII) and OBJ. Drop files on the plate.
- Several models, with move, rotate, scale, mirror, duplicate, delete, and undo.
- Drop to the bed, center, put the largest face down, auto-orient one model or all of them, shelf-pack the plate, and fill the bed with copies. Repair flips an inside-out shell and welds duplicate corners. Models that hang off the plate are marked.
- Hollow at slice time: wall thickness, top and bottom caps, and a lattice. Drain holes are cylinders you click onto the surface.
- The prepare window follows the classic Chitubox layout: a top menu, a left tool rail (Select, Move, Rotate, Scale, Mirror, Hollow, Hole, Support), and a right panel that changes with the tool. Print settings stay in the panel under that name.
- Automatic tree supports (Light, Medium, Heavy). Nearby tips share a trunk. Each tip has a point or ball contact, contact diameter and depth, upper and lower diameter, connection length, trunk diameter, branch angle, and a foot. Cross-braces and a raft are optional. You can also click an underside, send supports only to the platform, or drop them on islands from the last slice.
- Layer preview with a vertical bar on the right: step up or down one layer, drag the bar, or type a layer number. A new slice opens on the last layer. Islands are tinted, and a layer that seals a cavity is called out.
- The plate draws every triangle and grows faces that would be smaller than a pixel, so a dense sculpt stays solid instead of looking full of holes.
- Volume, weight, and a time estimate. Export `.pm3m` or one layer as PNG.
- Settings persist between launches.

The slicer keeps empty layers under a floating part. The printer stacks exposures from the bed, so dropping those layers would print the part on the plate.

## Command line

```text
amber slice model.stl -o model.pm3m --layer 0.05 --exposure 3 --supports medium --hollow 2.0
```

`--supports` is `none`, `light`, `medium`, or `heavy`. Layer height is clamped to 0.01–0.20 mm. The CLI uses the Colored UV starting point unless you pass `--exposure`.

## Build

Install a recent stable Rust (1.85 or newer).

Windows, from this folder:

```text
cargo build --release
```

The executable is `target\release\amber.exe`. A console is hidden in release builds. `amber slice ...` still prints if you run it from a terminal.

Linux (used to check the same window here):

```text
sudo apt install build-essential pkg-config libgtk-3-dev libx11-dev libxcb1-dev libxkbcommon-dev libgl1-mesa-dev
cargo build --release
```

Cross-compile from Linux (mingw):

```text
rustup target add x86_64-pc-windows-gnu
sudo apt install gcc-mingw-w64
cargo build --release --target x86_64-pc-windows-gnu
```

## File format

Output is Photon Workshop v516 with `pw0Img` run-length layers, 6480 × 3600 pixels at 46 µm, and a 224 × 168 preview. Each layer record stores that layer's thickness, not an absolute Z. Motion in the file is single-stage. Anti-aliasing is 1, 2, 4, or 8 levels.

## Not in this version

- 3MF, and mesh-boolean union. Overlapping solids are unioned in the raster.
- Variable layer height, two-stage lift as its own mode, and printers other than the Photon M3 Max.
- Live re-slice while you drag a model.

## Credits

Machine numbers for the M3 Max match Anycubic's 7K panel and the published SoulCrafted printer profile. The v516 layout follows the Photon Workshop file as documented by UVtools and Photonic Etcher. Amber's slicer, support generator, and file writer are original. SoulCrafted and UVtools source was not copied.

## License

MIT. See [LICENSE](LICENSE).

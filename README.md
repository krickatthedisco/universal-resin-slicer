# Amber

Amber is a desktop resin slicer. The machine it was built around is the **Anycubic Photon M3 Max** (298.08 × 165.6 × 300 mm, 6480×3600, 46 µm). It also knows the build volume and pixel grid of the other printers in the UVtools printer list, and it can write a Photon Workshop **v516** file for the machines whose profile is version 516. Every printer can be saved as an open `.sl1` zip. Encrypted CTB, GOO, and similar files are not written.

The window is a native app (Rust, egui, OpenGL), version 0.1.0. The same program builds on Windows and on Linux. It is not a web app.

The window opens in **Simple**. That path is: open a model, pick the printer and resin, hollow and punch a hole if you need to, add supports, then **Slice and save**. **Workshop**, in the top bar, is every control. Help → How to print (F1) is the same five steps. Ctrl+O opens a file, Ctrl+Enter slices, and Ctrl+S saves.

## Run the Windows app

`amber.exe` in this folder is a 64-bit Windows program. Copy it to your PC and double-click it. It does not need Rust or an installer. Windows may warn that the file is unrecognized; that is the usual prompt for an app that is not signed.

From a terminal in the same folder you can also slice without opening the window:

```text
amber.exe slice model.stl -o model.pm3m --layer 0.05 --supports medium --printer anycubic-photon-m3-max
```

Amber is not affiliated with Anycubic or Elegoo. A resin row is copied from a manufacturer table, with the source shown in the slice panel. If a printer and resin have no row, the times stay where you left them and the panel says to run a RERF. A range in a table is stored as its middle. The Elegoo sheet does not list a bottom-layer count, so those rows use 5.

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

- Import STL (binary or ASCII), OBJ, and 3MF. Drop files on the plate. File → Save plate writes an `.amber` file with the models, supports, holes, printer, and resin settings. Open plate, or drop that file, puts them back. Ctrl+Shift+S saves it again. Ctrl+S still writes the sliced file the printer reads. The status line says the next step, from opening a model through saving the file. Ctrl+Z undoes. Ctrl+Y redo puts that edit back.
- Printer list with search. Picking a printer sets the plate, the pixel size, and that machine's lift defaults. It does not invent an exposure.
- Resin list with search. It starts filtered to resins that have a published profile for the printer you picked; uncheck that to see the whole library. A dot means this printer has a published starting point. Elegoo rows are the 2023-12-11 official sheet (Mars, Saturn, and Jupiter, including color). Anycubic rows are the Photon M3 Max store guide from November 2023. Other bottles are named so you can find them, without a guessed cure time.
- A Measure tool: click two points and read the distance. Recent files stay in the File menu. Models can be renamed in the list.
- Several models, with move, rotate, scale to a size in millimetres, quarter turns, mirror, duplicate, a counted row of copies, delete, and undo. Cut a model on Z and keep both pieces. The Select tool moves the selected model in X and Y with a left drag. Right-drag still orbits, and Shift-drag or middle-drag pans. Right-click a part for the same edits. Overlapping models are marked, and View → Show overhangs paints faces that need support. The checkbox beside a model hides it in the view. It still prints.
- View → Cut the view hides the model above a height, drawn as an orange line, so you can click the surface that is left and place a support there. The same control is on the Support tool. View can also show only the contact points, or hide necks, trunks, feet, branches, braces, and the raft one piece at a time. That does not change the slice.
- Drop to the bed, center, put the largest face down, auto-orient one model or all of them, shelf-pack the plate, and fill the bed with copies. Repair flips an inside-out shell and welds duplicate corners. Models that hang off the plate are marked.
- Hollow at slice time: wall thickness and top and bottom caps. The inside stays empty. Drain holes are cylinders you click onto the surface, with a diameter and depth, or a hole punched through the bottom.
- XY offset, an elephant-foot inset on the bottom layers, XY/Z shrink compensation, and an option to fill enclosed voids. A price per litre shows a cost after the slice.
- The prepare window follows the classic Chitubox layout: a top menu, a left tool rail (Select, Move, Rotate, Scale, Mirror, Hollow, Hole, Support), and a right panel that changes with the tool. Print settings stay in that panel and open with the printer and resin you are using.
- Automatic tree supports (Light, Medium, Heavy, Hairpin). Nearby tips share a trunk. Each tip has a point or ball contact, contact diameter and depth, upper and lower diameter, connection length, trunk diameter, branch angle, foot height, and foot diameter. Adding supports lifts the part so its lowest point sits 5 mm above the bed (adjustable, and a second pass does not stack another lift). Diagonal braces join nearby trunks at 45° to the bed, and that angle is adjustable. A skate raft is off until you turn it on, and it only appears under supports that reach the bed: a square pad on each pillar, joined where the pillars are close, with thickness, oversize, and a wall angle. You can also click an underside, drag a tip to a new spot, erase tips inside a radius, send supports only to the platform, or drop them on islands from the last slice. Tips only leaves the contact points on screen while you place more.
- Fill enclosed voids heals speckled gaps and cures accidental pockets. A model you hollowed stays empty. Drains are cut after the fill. Rest before the cure and rest after the lift are added to the light-off the Photon file stores, and counted once in the time estimate. Image blur is off until you set it.
- Layer preview with a vertical bar on the right: step up or down one layer, drag the bar, or type a layer number. A new slice opens on the last layer. Islands are tinted, and a layer that seals a cavity is called out. The same islands are red marks on the plate. Click a mark with Support to plant a tip there. Marks stay until a model moves.
- The plate uses a 24-bit depth buffer and smooth shading, the same way PrusaSlicer and OrcaSlicer draw a solid STL, so the front of the shell hides the inside. Dragging right turns the build plate to the right. Moving a model sends a new matrix; the sculpt and the support forest stay on the GPU until the mesh or the supports change.
- Volume, weight, and a time estimate. Export the native v516 file when the printer has one, or `.sl1` for every machine, or one layer as PNG.
- Large meshes stay on the CPU path that only clips triangles crossing the current layer, and each scanline only tests the edges that cross it. Layers are rasterized in parallel, several bands of height per core so a wide base does not stall the top. Islands and sealed pockets are finished in order afterwards, because those depend on the previous layer. The slice bar estimates the time left. The layer view splits the print time into light and lifting.
- Settings persist between launches.

The slicer keeps empty layers under a floating part. The printer stacks exposures from the bed, so dropping those layers would print the part on the plate.

## Command line

```text
amber slice model.stl -o model.pm3m --printer anycubic-photon-m3-max --layer 0.05 --exposure 3 --supports medium --hollow 2.0
```

`--printer` is a catalog id such as `anycubic-photon-m3-max` or `elegoo-mars-4`. `--supports` is `none`, `light`, `medium`, `heavy`, or `hairpin`. Layer height is clamped to 0.01–0.20 mm. The CLI uses the Colored UV starting point unless you pass `--exposure`. A printer without a v516 profile is written as `.sl1` even if the output name says otherwise. Pass `-o file.sl1` to force that zip on a Photon machine.

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

Photon Workshop v516 (`.pm3m` and the other v516 suffixes in the catalog) uses `pw0Img` run-length layers and a 224 × 168 preview. The M3 Max file is 6480 × 3600 at 46 µm. Each layer record stores that layer's thickness, not an absolute Z. Motion in the file is single-stage. Anti-aliasing is 1, 2, 4, or 8 levels. `.sl1` is a zip of `config.ini` plus one grayscale PNG per layer.

## Not in this version

- Encrypted or proprietary printer files (CTB, GOO, and the rest). Those machines export `.sl1` for a converter.
- Mesh-boolean union. Overlapping solids are unioned in the raster.
- Variable layer height, two-stage lift as its own mode, and a measured exposure for every resin on every printer.
- Live re-slice while you drag a model.

## Credits

Printer volumes and pixel grids come from the UVtools printer profiles (manufacturer figures). The M3 Max orientation matches Photonic Etcher: rotate 180°, no mirror. The v516 layout follows the Photon Workshop file as documented by UVtools and Photonic Etcher. Elegoo times are from Elegoo's resin sheet dated 2023-12-11. Anycubic M3 Max times are from the November 2023 store guide. Amber's slicer, support generator, and file writers are original. SoulCrafted and UVtools source was not copied.

## License

MIT. See [LICENSE](LICENSE).

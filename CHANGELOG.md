# Changelog

## 0.1.0

Amber is a desktop resin slicer. The first printer it was built around is the Anycubic Photon M3 Max. It also knows the plate and pixel grid of the other machines in the UVtools list, writes a Photon Workshop v516 file when that is the machine's format, and writes an open `.sl1` for every machine.

- Simple view for a first print, and Workshop for every control.
- Import STL, OBJ, and 3MF. Move, rotate, scale to a size in millimetres, mirror, duplicate, copy across the bed, cut on Z, and undo.
- Hollow with a wall and caps. The inside stays empty. Drain holes have a diameter and a depth.
- Tree supports, manual supports from under the bed, island supports, braces, and a skate raft whose lip overhangs so a scraper can get under it.
- Cut the view on a height so a custom support can be clicked onto the surface that is left. Hide a model, or show only contact points, necks, trunks, feet, branches, braces, or the raft. Hiding something only changes the view. The slice still includes it.
- Drag a selected tip onto a new spot on that model. Erase tips removes every contact within a radius, and one undo puts the stroke back.
- With a tip selected, the arrow keys nudge it by 1 mm (Shift is 0.1 mm) and seat it on the model again. X and Y in the Select and Support panels do the same. A nudge that leaves the model does nothing. In the layer view the arrows still step through layers.
- After a slice, each island column is a red mark on the plate. Click a mark with the Support tool to plant a tip. The marks stay until a model moves.
- Rest before the cure and rest after the lift, counted once and stored in the file's light-off.
- Fill enclosed voids heals speckles and accidental pockets, and leaves a model you hollowed empty.
- Anti-alias, optional image blur, XY offset, elephant foot, and shrink compensation.
- Measure, overhang colors, overlap warning, layer preview, and a print time split into light and lifting.
- Layers are sliced across cores. Island tracking only clears the pixels a layer actually set, instead of wiping the whole plate each time. The island scan reuses one visited mask up the part, and only wipes it every 255 layers.
- Redo (Ctrl+Y) puts an undone edit back, including a move, a delete, and a cut. A new edit clears that. The status line says the next step: open a model, punch a hole, add supports, slice, or save.
- Save plate writes an `.amber` file: the models, where they sit, their supports and holes, and the printer and resin settings. Open plate puts that job back. Ctrl+Shift+S saves it again.
- Duplicate, copy across the bed, and fill the bed take the supports and the drain holes with each copy. Undo removes that copy's tips and holes.
- Prepare has the same layer slider as Preview. The current layer is drawn on the plate, and the model above it can be cut away.
- Support pillars default to a hexagon. Round and square are in the same menu. The diameter is flat to flat.
- Simple view can show every support piece again after showing only the contact points.
- A layer where two supports overlap stays solid instead of leaving a one-layer line between them.
- Export defaults to the file the selected printer reads: Photon Workshop v516, unencrypted CTB, Prusa SL1, or a PNG layer zip. The file-format menu can write any of those four.

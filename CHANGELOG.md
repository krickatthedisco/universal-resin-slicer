# Changelog

## 0.1.0

Amber is a desktop resin slicer. The first printer it was built around is the Anycubic Photon M3 Max. It also knows the plate and pixel grid of the other machines in the UVtools list, writes a Photon Workshop v516 file when that is the machine's format, and writes an open `.sl1` for every machine.

- Simple view for a first print, and Workshop for every control.
- Import STL, OBJ, and 3MF. Move, rotate, scale to a size in millimetres, mirror, duplicate, copy across the bed, cut on Z, and undo.
- Hollow with a wall and caps. The inside stays empty. Drain holes have a diameter and a depth.
- Tree supports, manual supports from under the bed, island supports, braces, and a skate raft whose lip overhangs so a scraper can get under it.
- Rest before the cure and rest after the lift, counted once and stored in the file's light-off.
- Fill enclosed voids heals speckles and accidental pockets, and leaves a model you hollowed empty.
- Anti-alias, optional image blur, XY offset, elephant foot, and shrink compensation.
- Measure, overhang colors, overlap warning, layer preview, and a print time split into light and lifting.
- Layers are sliced across cores.

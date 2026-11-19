# Smooth FDM Threads (`smooth-fdm-threads`)

Native Rust WebAssembly Component plugin (`craft:plugin@0.1.0` / `commands@0.1`) for **SolidCraft 0.44.1** that generates smooth, exact B-rep internal and external screw threads tuned for FDM 3D printing.

Instead of building faceted triangle meshes in the host, the plugin constructs an exact 2D trapezoidal groove profile (`sketch.create` + `sketch.line` + optional `sketch.fillet`) and sweeps it via SolidCraft's helical B-rep engine (`solid.coil`), followed by optional circular multi-start patterning (`solid.pattern.circular`) and revolved lead-in/lead-out end chamfers (`solid.revolve`). Because the result stays true NURBS/analytic B-rep until export, 3MF/STEP files remain exact and STL exports cleanly without artificial facets.

---

## Installing in SolidCraft

1. Open SolidCraft (**0.44.1** or later).
2. Choose **File ▸ Plugins… → Install plugin…** and select `dist/smooth-fdm-threads.zip`.
3. Confirm the requested `commands@0.1` permissions (`files: []`, `net: false`, `clock: false`).
4. Two commands are added to the **Model** ribbon tab:
   - **apply-thread** — Thread an existing cylindrical shaft or hole face.
   - **create-thread-pair** — Generate a standalone Bolt + Nut (or Bolt / Nut) test pair.

---

## Commands & Parameters

### 1. `apply-thread`

Select any cylindrical shaft or bore face (either via the viewport picker or pre-selected before invoking the command) to cut helical threads along the cylinder's axis. Supports off-axis cylinders (X, Y, or arbitrary world vectors), blind holes, and partial-length threading.

| Parameter | Default | Description |
| --- | --- | --- |
| `face` | *(selected)* | Cylindrical shaft or hole face (`x-solidcraft-selection`). Optional if already selected. |
| `mode` | `auto` | `auto` (detect shaft vs. hole), `external` (shaft), or `internal` (hole). |
| `pitch` | `3.0` | Axial distance between adjacent thread crests (mm). |
| `length` | *(full span)* | Threaded length along the cylinder axis (`0` or blank threads the entire face). |
| `starts` | `1` | Number of thread starts (`1..8`; lead = `starts * pitch`). |
| `left_hand` | `false` | Reverses helical twist direction when `true`. |
| `upper_flank_angle_deg` | `45.0` | Upper flank angle from horizontal (`45°` prints support-free; `30°` matches ISO 60°). |
| `lower_flank_angle_deg` | `45.0` | Lower flank angle from horizontal (vary independently for buttress profiles). |
| `radial_clearance` | `0.2` | Per-side radial clearance applied to internal threads (or external if enabled). |
| `axial_clearance` | `0.1` | Extra axial groove backlash (mm) for smooth printed engagement. |
| `crest_flat` | `0.4` | Flat width at tooth crest (mm; keep ≥ nozzle line width for durability). |
| `root_flat` | `0.4` | Flat width at tooth root (mm). |
| `crest_fillet` | `0.0` | Optional corner fillet radius at crests (`0` disables). |
| `root_fillet` | `0.0` | Optional corner fillet radius at roots (`0` disables). |
| `end_chamfer` | `both` | Revolved lead-in/lead-out chamfer on free ends (`both`, `start`, or `none`). |
| `apply_clearance_to_external` | `false` | When `true` on an external shaft, shrinks/widens the shaft groove instead of expecting clearance on the mating nut. |

### 2. `create-thread-pair`

Generates a ready-to-print mating **Bolt**, **Nut**, or **Bolt + Nut** pair aligned along Z.

Includes all profile, flank-angle, clearance, fillet, and chamfer parameters above, plus:

| Parameter | Default | Description |
| --- | --- | --- |
| `generate` | `pair` | `pair` (Bolt + Nut), `bolt`, or `nut`. |
| `major_diameter` | `20.0` | Nominal outer thread diameter (mm). |
| `length` | `20.0` | Threaded cylinder height along Z (mm). |
| `nut_wall_thickness` | `5.0` | Radial wall thickness outside the nut's thread major radius (mm). |
| `pair_spacing` | `10.0` | Gap between Bolt and Nut when generating both (mm). |

---

## FDM Design Notes

- **45° Self-Supporting Flanks (`45.0° / 45.0°`)**: Vertical-axis threads at 45° overhang print cleanly without supports and hold up well to coarse layer heights (`0.2–0.28 mm`).
- **Buttress Profiles (e.g. `10.0° / 45.0°`)**: Asymmetric upper and lower flank angles handle axial load in one direction while retaining printable 45° overhangs.
- **Nozzle-Friendly Flats (`0.4 mm`)**: Flat crests and roots avoid fragile razor-thin peaks and match standard `0.4 mm` extrusion widths.
- **Clearances (`0.2 mm` radial, `0.1 mm` axial)**: Offsets the internal bore outward before cutting the coil and widens the groove axially so bolt and nut spin freely off the build plate.

---

## Building, Testing & Packaging (`D:\Rust`)

This workspace targets `wasm32-unknown-unknown` packaged into a WebAssembly Component using `wasm-tools`, with the Rust toolchain kept on `D:\Rust`:

```powershell
$env:RUSTUP_HOME = "D:\Rust\.rustup"
$env:CARGO_HOME  = "D:\Rust\.cargo"
$env:Path        = "D:\Rust\.cargo\bin;" + $env:Path

# Run unit & manifest validation tests
cargo test --offline

# Generate standalone SolidCraft CLI preview scripts in target/preview/
cargo run --offline --bin preview

# Build release Wasm component, zip dist/smooth-fdm-threads.zip, and run solidcraft-cli checks
powershell -ExecutionPolicy Bypass -File scripts/package.ps1
```

### Project Layout

- `plugin.json` — SolidCraft `commands@0.1` plugin manifest and JSON Schema inputs.
- `wit/plugin.wit` — Official SolidCraft `v0.44.1` WIT contract (`craft:plugin@0.1.0`).
- `assets/icon.svg` — Monochrome `64×64` ribbon icon.
- `src/math.rs` — Trapezoidal/buttress groove solver, tangent-trim fillet checks, orthonormal frame builder, and cylinder/mesh classification.
- `src/commands.rs` — Command builder translating solved thread geometry into SolidCraft sketch, coil, visibility, circular pattern, and chamfer commands.
- `src/lib.rs` — Wasm `command-plugin` guest entry points (`apply-thread`, `create-thread-pair`) and unit tests.
- `src/bin/preview.rs` — Headless preview script generator for SolidCraft CLI geometry/snapshot checks.
- `scripts/package.ps1` — Release component builder and validator.

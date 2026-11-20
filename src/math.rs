use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tolerance margin (mm) extended radially into open air outside the cylinder/bore
/// so B-rep boolean cuts never suffer from coplanar/tangent boundary retries.
pub const AIR_OVERLAP_MM: f64 = 0.20;

/// Minimum remaining solid tooth tip / groove spacing (mm) before adjacent turns
/// could touch or self-intersect along the helix.
pub const MIN_AXIAL_GAP_MM: f64 = 0.04;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    External,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndChamfer {
    Both,
    Start,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChamferSize {
    #[default]
    Regular,
    Wide,
    Custom(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadProfileParams {
    pub major_diameter: f64,
    pub pitch: f64,
    pub length: f64,
    pub starts: u32,
    pub left_hand: bool,
    pub upper_flank_angle_deg: f64,
    pub lower_flank_angle_deg: f64,
    pub radial_clearance: f64,
    pub axial_clearance: f64,
    pub crest_flat: f64,
    pub root_flat: f64,
    pub crest_fillet: f64,
    pub root_fillet: f64,
    pub end_chamfer: EndChamfer,
    pub chamfer_size: ChamferSize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidationError {
    pub field: String,
    pub reason: String,
}

impl ValidationError {
    pub fn new(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

/// Solved 2D groove cut polygon in the axial sketch plane `(r, z)` where
/// `+r` is radial outward from the cylinder axis and `+z` is along the helix axis.
#[derive(Clone, Debug, PartialEq)]
pub struct SolvedGroove {
    pub kind: ThreadKind,
    pub major_radius: f64,
    pub minor_radius: f64,
    pub bore_or_shaft_radius: f64,
    pub thread_depth: f64,
    pub pitch: f64,
    pub lead: f64,
    pub starts: u32,
    pub left_hand: bool,
    /// Closed 6-point loop in `(r, z)` coordinates centered axially around `z_center_offset`.
    /// Vertices are ordered counter-clockwise in `(r, z)`:
    /// outer/inner air relief -> crest/root -> root/crest -> air relief.
    pub polygon: Vec<[f64; 2]>,
    /// Fillet radius (mm) for the two deep bottom corners of the groove (indices 1 & 2).
    pub bottom_fillet: f64,
    /// Fillet radius (mm) for the two mouth corners on the cylinder/bore surface.
    pub surface_fillet: f64,
    /// Exact axial stub length (mm) needed along the cylinder/bore wall when `surface_fillet > 0`.
    pub surface_stub: f64,
}

/// Orthonormal coordinate frame anchored on a cylinder axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisFrame {
    pub origin: [f64; 3],
    /// Unit radial direction (`x_dir` of sketch).
    pub x_dir: [f64; 3],
    /// Unit axial direction (`y_dir` of sketch, aligned with helix climb axis).
    pub axis: [f64; 3],
}

pub fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn norm3(v: [f64; 3]) -> f64 {
    dot3(v, v).sqrt()
}

pub fn normalize3(v: [f64; 3]) -> Result<[f64; 3], ValidationError> {
    let len = norm3(v);
    if !len.is_finite() || len < 1e-9 {
        return Err(ValidationError::new("axis", "Cylinder axis vector cannot be zero"));
    }
    Ok([v[0] / len, v[1] / len, v[2] / len])
}

/// Builds an orthonormal sketch plane `{origin, x_dir, y_dir = axis}` where sketch `+X` is
/// radially outward and sketch `+Y` is along the helix climb axis.
pub fn build_axis_frame(origin: [f64; 3], raw_axis: [f64; 3]) -> Result<AxisFrame, ValidationError> {
    let axis = normalize3(raw_axis)?;
    // Choose a deterministic reference vector not parallel to `axis`.
    let ref_vec = if axis[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let x_raw = cross3(ref_vec, axis);
    let x_dir = normalize3(x_raw)?;
    Ok(AxisFrame {
        origin,
        x_dir,
        axis,
    })
}

/// Validates thread parameters and solves the nominal thread depth `h_nom`.
pub fn validate_and_depth(p: &ThreadProfileParams) -> Result<f64, ValidationError> {
    if !p.major_diameter.is_finite() || p.major_diameter <= 1.0 {
        return Err(ValidationError::new(
            "major_diameter",
            "major_diameter must be > 1.0 mm",
        ));
    }
    if !p.pitch.is_finite() || p.pitch <= 0.05 {
        return Err(ValidationError::new("pitch", "pitch must be > 0.05 mm"));
    }
    if !p.length.is_finite() || p.length <= 0.2 {
        return Err(ValidationError::new("length", "length must be > 0.2 mm"));
    }
    if !(1..=8).contains(&p.starts) {
        return Err(ValidationError::new("starts", "starts must be between 1 and 8"));
    }
    if !p.upper_flank_angle_deg.is_finite()
        || !(1.0..=80.0).contains(&p.upper_flank_angle_deg)
    {
        return Err(ValidationError::new(
            "upper_flank_angle_deg",
            "upper_flank_angle_deg must be between 1 and 80 degrees",
        ));
    }
    if !p.lower_flank_angle_deg.is_finite()
        || !(1.0..=80.0).contains(&p.lower_flank_angle_deg)
    {
        return Err(ValidationError::new(
            "lower_flank_angle_deg",
            "lower_flank_angle_deg must be between 1 and 80 degrees",
        ));
    }
    if !p.radial_clearance.is_finite() || p.radial_clearance < 0.0 {
        return Err(ValidationError::new(
            "radial_clearance",
            "radial_clearance must be >= 0",
        ));
    }
    if !p.axial_clearance.is_finite() || p.axial_clearance < 0.0 {
        return Err(ValidationError::new(
            "axial_clearance",
            "axial_clearance must be >= 0",
        ));
    }
    if !p.crest_flat.is_finite() || p.crest_flat <= 0.0 {
        return Err(ValidationError::new(
            "crest_flat",
            "crest_flat must be > 0",
        ));
    }
    if !p.root_flat.is_finite() || p.root_flat <= 0.0 {
        return Err(ValidationError::new("root_flat", "root_flat must be > 0"));
    }
    if !p.crest_fillet.is_finite() || p.crest_fillet < 0.0 {
        return Err(ValidationError::new(
            "crest_fillet",
            "crest_fillet must be >= 0",
        ));
    }
    if !p.root_fillet.is_finite() || p.root_fillet < 0.0 {
        return Err(ValidationError::new(
            "root_fillet",
            "root_fillet must be >= 0",
        ));
    }

    let flats_and_clearance = p.crest_flat + p.root_flat + p.axial_clearance;
    if flats_and_clearance >= p.pitch - MIN_AXIAL_GAP_MM {
        return Err(ValidationError::new(
            "pitch",
            format!(
                "crest_flat ({:.3}) + root_flat ({:.3}) + axial_clearance ({:.3}) = {:.3} mm leaves too little room inside pitch {:.3} mm",
                p.crest_flat, p.root_flat, p.axial_clearance, flats_and_clearance, p.pitch
            ),
        ));
    }

    if p.crest_flat - p.axial_clearance <= MIN_AXIAL_GAP_MM {
        return Err(ValidationError::new(
            "axial_clearance",
            "crest_flat - axial_clearance must leave > 0.04 mm solid crest width",
        ));
    }
    if p.root_flat - p.axial_clearance <= MIN_AXIAL_GAP_MM {
        return Err(ValidationError::new(
            "axial_clearance",
            "root_flat - axial_clearance must leave > 0.04 mm solid root width",
        ));
    }

    let tan_sum = p.upper_flank_angle_deg.to_radians().tan()
        + p.lower_flank_angle_deg.to_radians().tan();
    let h_nom = (p.pitch - (p.crest_flat + p.root_flat)) / tan_sum;
    let minor_diameter = p.major_diameter - 2.0 * h_nom;
    if minor_diameter <= 0.5 {
        return Err(ValidationError::new(
            "major_diameter",
            format!(
                "major_diameter ({:.3} mm) is too small for thread depth ({:.3} mm); minor diameter would be {:.3} mm",
                p.major_diameter, h_nom, minor_diameter
            ),
        ));
    }

    // Check optional corner fillet radii against flat and flank budgets.
    let max_fillet = (h_nom * 0.4).min(p.crest_flat * 0.45).min(p.root_flat * 0.45);
    if p.crest_fillet > max_fillet {
        return Err(ValidationError::new(
            "crest_fillet",
            format!(
                "crest_fillet ({:.3} mm) exceeds safe profile limit ({:.3} mm)",
                p.crest_fillet, max_fillet
            ),
        ));
    }
    if p.root_fillet > max_fillet {
        return Err(ValidationError::new(
            "root_fillet",
            format!(
                "root_fillet ({:.3} mm) exceeds safe profile limit ({:.3} mm)",
                p.root_fillet, max_fillet
            ),
        ));
    }

    // Verify axial stub lengths needed for surface fillets still leave positive spacing between turns.
    let min_alpha_rad = p
        .upper_flank_angle_deg
        .min(p.lower_flank_angle_deg)
        .to_radians();
    let trim_factor = ((std::f64::consts::FRAC_PI_2 - min_alpha_rad) * 0.5).tan();
    if p.crest_fillet > 1e-6 {
        let stub = p.crest_fillet * trim_factor + 0.02;
        if p.crest_flat - p.axial_clearance - 2.0 * stub <= MIN_AXIAL_GAP_MM {
            return Err(ValidationError::new(
                "crest_fillet",
                "crest_fillet is too large for remaining crest width after axial_clearance",
            ));
        }
    }
    if p.root_fillet > 1e-6 {
        let stub = p.root_fillet * trim_factor + 0.02;
        if p.root_flat - p.axial_clearance - 2.0 * stub <= MIN_AXIAL_GAP_MM {
            return Err(ValidationError::new(
                "root_fillet",
                "root_fillet is too large for remaining root width after axial_clearance",
            ));
        }
    }
    if let ChamferSize::Custom(c) = p.chamfer_size {
        if !c.is_finite() || c <= 0.0 || c > p.length * 0.45 {
            return Err(ValidationError::new(
                "opt_chamfer_mm",
                format!(
                    "chamfer size ({c:.3} mm) must be > 0 and <= 45% of thread length ({:.3} mm)",
                    p.length * 0.45
                ),
            ));
        }
    }

    Ok(h_nom)
}

pub fn chamfer_expression(prefix: &str, size: ChamferSize, has_nut_wall: bool) -> String {
    match size {
        ChamferSize::Regular => format!("min(0.95 * {prefix}_depth, 0.25 * {prefix}_length)"),
        ChamferSize::Wide => {
            if has_nut_wall {
                format!(
                    "min(1.50 * {prefix}_depth, 0.35 * {prefix}_length, 0.32 * {prefix}_major_d, {prefix}_depth + 0.65 * {prefix}_nut_wall)"
                )
            } else {
                format!(
                    "min(1.50 * {prefix}_depth, 0.35 * {prefix}_length, 0.32 * {prefix}_major_d)"
                )
            }
        }
        ChamferSize::Custom(v) => format!("min({v:.6} mm, 0.40 * {prefix}_length)"),
    }
}

pub fn compute_chamfer_depth(
    depth: f64,
    length: f64,
    major_d: f64,
    nut_wall: Option<f64>,
    size: ChamferSize,
) -> f64 {
    match size {
        ChamferSize::Regular => (0.95 * depth).min(0.25 * length),
        ChamferSize::Wide => {
            let mut d = (1.50 * depth).min(0.35 * length).min(0.32 * major_d);
            if let Some(w) = nut_wall {
                d = d.min(depth + 0.65 * w);
            }
            d
        }
        ChamferSize::Custom(v) => v.min(0.40 * length),
    }
}

fn compute_surface_stub(surface_fillet: f64, p: &ThreadProfileParams) -> f64 {
    if surface_fillet <= 1e-6 {
        return 0.0;
    }
    let min_alpha_rad = p
        .upper_flank_angle_deg
        .min(p.lower_flank_angle_deg)
        .to_radians();
    let trim_factor = ((std::f64::consts::FRAC_PI_2 - min_alpha_rad) * 0.5).tan();
    surface_fillet * trim_factor + 0.02
}

/// Exact axial offset placing an internal mating groove centered over an external bolt tooth,
/// accounting for asymmetric upper/lower flank angles.
pub fn nut_axial_phase_offset(p: &ThreadProfileParams, depth: f64) -> f64 {
    let tan_l = p.lower_flank_angle_deg.to_radians().tan();
    let tan_u = p.upper_flank_angle_deg.to_radians().tan();
    0.5 * p.pitch + 0.5 * depth * (tan_l - tan_u)
}

/// Solves a groove cut profile (`operation: "cut"`) for either an external shaft or internal bore.
///
/// * `apply_clearance`: When `true` on `Internal`, opens the bore by `+radial_clearance` and widens
///   the groove by `+axial_clearance` (standard nut fit). When `false` on `External`, cuts nominal
///   thread dimensions. When `true` on `External`, reduces both shaft/root radii by `-radial_clearance`
///   and widens the valley by `+axial_clearance`.
pub fn solve_groove(
    p: &ThreadProfileParams,
    kind: ThreadKind,
    apply_clearance: bool,
    z_phase_offset: f64,
) -> Result<SolvedGroove, ValidationError> {
    let h_nom = validate_and_depth(p)?;
    let r_maj_nom = p.major_diameter * 0.5;
    let r_min_nom = r_maj_nom - h_nom;

    let cr = if apply_clearance { p.radial_clearance } else { 0.0 };
    let ca = if apply_clearance { p.axial_clearance } else { 0.0 };

    let tan_u = p.upper_flank_angle_deg.to_radians().tan();
    let tan_l = p.lower_flank_angle_deg.to_radians().tan();
    let lead = p.pitch * (p.starts as f64);

    match kind {
        ThreadKind::External => {
            let r_maj = r_maj_nom - cr;
            let r_min = r_min_nom - cr;
            if r_min <= 0.25 {
                return Err(ValidationError::new(
                    "radial_clearance",
                    "radial_clearance reduces external minor radius below 0.25 mm",
                ));
            }
            let r_air = r_maj + AIR_OVERLAP_MM;
            let z_root_lo = z_phase_offset - 0.5 * (p.root_flat + ca);
            let z_root_hi = z_phase_offset + 0.5 * (p.root_flat + ca);
            let z_surf_lo = z_root_lo - h_nom * tan_u;
            let z_surf_hi = z_root_hi + h_nom * tan_l;

            // Ordered so the two groove root vertices are indices 1 and 2,
            // and the two cylinder-surface corners are indices 0 and 3 (before air relief 4, 5).
            let polygon = vec![
                [r_maj, z_surf_hi],
                [r_min, z_root_hi],
                [r_min, z_root_lo],
                [r_maj, z_surf_lo],
                [r_air, z_surf_lo],
                [r_air, z_surf_hi],
            ];

            Ok(SolvedGroove {
                kind,
                major_radius: r_maj,
                minor_radius: r_min,
                bore_or_shaft_radius: r_maj,
                thread_depth: h_nom,
                pitch: p.pitch,
                lead,
                starts: p.starts,
                left_hand: p.left_hand,
                polygon,
                bottom_fillet: p.root_fillet,
                surface_fillet: p.crest_fillet,
                surface_stub: compute_surface_stub(p.crest_fillet, p),
            })
        }
        ThreadKind::Internal => {
            let r_maj = r_maj_nom + cr;
            let r_bore = r_min_nom + cr;
            let r_air = (r_bore - AIR_OVERLAP_MM).max(0.05);

            // Shift internal groove center by +pitch/2 when z_phase_offset == 0?
            // Callers pass explicit `z_phase_offset` (e.g. +pitch/2 for coaxial nut-over-bolt alignment).
            let z_out_lo = z_phase_offset - 0.5 * (p.crest_flat + ca);
            let z_out_hi = z_phase_offset + 0.5 * (p.crest_flat + ca);
            let z_bore_lo = z_out_lo - h_nom * tan_l;
            let z_bore_hi = z_out_hi + h_nom * tan_u;

            // Ordered so outer root vertices are indices 1 and 2,
            // bore surface corners are indices 0 and 3, air relief inside bore are 4 and 5.
            let polygon = vec![
                [r_bore, z_bore_hi],
                [r_maj, z_out_hi],
                [r_maj, z_out_lo],
                [r_bore, z_bore_lo],
                [r_air, z_bore_lo],
                [r_air, z_bore_hi],
            ];

            Ok(SolvedGroove {
                kind,
                major_radius: r_maj,
                minor_radius: r_bore,
                bore_or_shaft_radius: r_bore,
                thread_depth: h_nom,
                pitch: p.pitch,
                lead,
                starts: p.starts,
                left_hand: p.left_hand,
                polygon,
                bottom_fillet: p.crest_fillet,
                surface_fillet: p.root_fillet,
                surface_stub: compute_surface_stub(p.root_fillet, p),
            })
        }
    }
}

/// Information extracted from a selected cylindrical face on a body.
#[derive(Clone, Debug, PartialEq)]
pub struct CylinderFaceInfo {
    pub body: String,
    pub face_index: usize,
    pub face_name: String,
    pub point: [f64; 3],
    pub origin: [f64; 3],
    pub axis: [f64; 3],
    pub radius: f64,
    pub z_min: f64,
    pub z_max: f64,
    pub detected_kind: Option<ThreadKind>,
}

pub fn parse_vec3(v: &Value) -> Option<[f64; 3]> {
    let arr = v.as_array()?;
    if arr.len() != 3 {
        return None;
    }
    Some([arr[0].as_f64()?, arr[1].as_f64()?, arr[2].as_f64()?])
}

/// Computes axial span `[z_min, z_max]` of `face_name` along `axis` from `model.edges` output.
pub fn axial_span_from_edges(
    edges_reply: &Value,
    face_name: &str,
    origin: [f64; 3],
    axis: [f64; 3],
) -> Option<(f64, f64)> {
    let edges = edges_reply.get("edges").and_then(Value::as_array)?;
    let mut z_min = f64::INFINITY;
    let mut z_max = f64::NEG_INFINITY;

    for e in edges {
        let name = e.get("name").and_then(Value::as_str).unwrap_or("");
        let matches = name.split('|').any(|part| part == face_name);
        if !matches {
            continue;
        }
        for key in ["start", "mid", "end"] {
            if let Some(pt) = e.get(key).and_then(parse_vec3) {
                let d = [pt[0] - origin[0], pt[1] - origin[1], pt[2] - origin[2]];
                let t = dot3(d, axis);
                if t < z_min {
                    z_min = t;
                }
                if t > z_max {
                    z_max = t;
                }
            }
        }
    }

    if z_min.is_finite() && z_max.is_finite() && z_max > z_min + 1e-6 {
        Some((z_min, z_max))
    } else {
        None
    }
}

/// Infers `External` vs `Internal` cylinder from `model.mesh` outward-pointing triangle normals
/// by matching triangles whose three vertices lie on the cylinder surface `(radius, [z_min, z_max])`.
pub fn detect_kind_from_mesh(
    mesh_reply: &Value,
    origin: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    z_min: f64,
    z_max: f64,
) -> Option<ThreadKind> {
    let positions = mesh_reply.get("positions").and_then(Value::as_array)?;
    let triangles = mesh_reply.get("triangles").and_then(Value::as_array)?;

    let mut pts = Vec::with_capacity(positions.len());
    for p in positions {
        pts.push(parse_vec3(p)?);
    }

    let mut vote_sum = 0.0;
    let mut hits = 0usize;
    let r_tol = (radius * 0.01).max(0.02);

    let vertex_on_cyl = |pt: [f64; 3]| -> Option<[f64; 3]> {
        let rel = [pt[0] - origin[0], pt[1] - origin[1], pt[2] - origin[2]];
        let z = dot3(rel, axis);
        if z < z_min - 0.05 || z > z_max + 0.05 {
            return None;
        }
        let radial = [
            rel[0] - z * axis[0],
            rel[1] - z * axis[1],
            rel[2] - z * axis[2],
        ];
        let r = norm3(radial);
        if (r - radius).abs() <= r_tol {
            Some(radial)
        } else {
            None
        }
    };

    for tri in triangles {
        let idx = tri.as_array()?;
        if idx.len() != 3 {
            continue;
        }
        let i0 = idx[0].as_u64()? as usize;
        let i1 = idx[1].as_u64()? as usize;
        let i2 = idx[2].as_u64()? as usize;
        let (p0, p1, p2) = (*pts.get(i0)?, *pts.get(i1)?, *pts.get(i2)?);

        let (Some(rad0), Some(rad1), Some(rad2)) =
            (vertex_on_cyl(p0), vertex_on_cyl(p1), vertex_on_cyl(p2))
        else {
            continue;
        };

        let radial = [
            (rad0[0] + rad1[0] + rad2[0]) / 3.0,
            (rad0[1] + rad1[1] + rad2[1]) / 3.0,
            (rad0[2] + rad1[2] + rad2[2]) / 3.0,
        ];
        let r = norm3(radial);
        if r < 1e-6 {
            continue;
        }

        let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
        let n = cross3(e1, e2);
        let n_len = norm3(n);
        if n_len < 1e-9 {
            continue;
        }
        // Only count triangles whose normals are mostly radial (not flat end caps).
        let axial_comp = dot3(n, axis).abs() / n_len;
        if axial_comp > 0.5 {
            continue;
        }
        vote_sum += dot3(n, radial) / (n_len * r);
        hits += 1;
    }

    if hits == 0 || vote_sum.abs() < 0.1 {
        None
    } else if vote_sum > 0.0 {
        Some(ThreadKind::External)
    } else {
        Some(ThreadKind::Internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_fdm_params() -> ThreadProfileParams {
        ThreadProfileParams {
            major_diameter: 20.0,
            pitch: 3.0,
            length: 20.0,
            starts: 1,
            left_hand: false,
            upper_flank_angle_deg: 45.0,
            lower_flank_angle_deg: 45.0,
            radial_clearance: 0.2,
            axial_clearance: 0.1,
            crest_flat: 0.4,
            root_flat: 0.4,
            crest_fillet: 0.0,
            root_fillet: 0.0,
            end_chamfer: EndChamfer::Both,
            chamfer_size: ChamferSize::Regular,
        }
    }

    #[test]
    fn iso_60_depth_matches_five_eighths_triangle_height() {
        let p = ThreadProfileParams {
            major_diameter: 10.0,
            pitch: 1.5,
            length: 15.0,
            starts: 1,
            left_hand: false,
            upper_flank_angle_deg: 30.0,
            lower_flank_angle_deg: 30.0,
            radial_clearance: 0.0,
            axial_clearance: 0.0,
            crest_flat: 1.5 / 8.0,
            root_flat: 1.5 / 4.0,
            crest_fillet: 0.0,
            root_fillet: 0.0,
            end_chamfer: EndChamfer::None,
            chamfer_size: ChamferSize::Regular,
        };
        let h = validate_and_depth(&p).unwrap();
        let expected = 5.0 * 3.0_f64.sqrt() / 16.0 * 1.5;
        assert!((h - expected).abs() < 1e-9, "h={h}, expected={expected}");
    }

    #[test]
    fn mating_pair_maintains_exact_radial_and_axial_clearances() {
        let p = sample_fdm_params();
        let ext = solve_groove(&p, ThreadKind::External, false, 0.0).unwrap();
        let int = solve_groove(&p, ThreadKind::Internal, true, 0.5 * p.pitch).unwrap();

        // Check major & minor radial clearance == radial_clearance
        assert!((int.major_radius - ext.major_radius - p.radial_clearance).abs() < 1e-9);
        assert!((int.minor_radius - ext.minor_radius - p.radial_clearance).abs() < 1e-9);

        // Groove axial widths at surface/bore must stay strictly < pitch to prevent self-intersection.
        let ext_width = ext.polygon[0][1] - ext.polygon[3][1];
        let int_width = int.polygon[0][1] - int.polygon[3][1];
        assert!(ext_width < p.pitch - MIN_AXIAL_GAP_MM + 1e-9);
        assert!(int_width < p.pitch - MIN_AXIAL_GAP_MM + 1e-9);

        // Verify bolt solid tooth centered at z = +pitch/2 lies strictly inside nut groove
        // across every radial sample in [ext.minor_radius, ext.major_radius].
        let tan_u = p.upper_flank_angle_deg.to_radians().tan();
        let tan_l = p.lower_flank_angle_deg.to_radians().tan();
        for i in 0..=10 {
            let frac = i as f64 / 10.0;
            let r = ext.minor_radius + frac * ext.thread_depth;
            let bolt_z_hi = 0.5 * p.pitch + 0.5 * p.crest_flat + (ext.major_radius - r) * tan_u;
            let bolt_z_lo = 0.5 * p.pitch - 0.5 * p.crest_flat - (ext.major_radius - r) * tan_l;

            let nut_z_hi =
                0.5 * p.pitch + 0.5 * (p.crest_flat + p.axial_clearance) + (int.major_radius - r) * tan_u;
            let nut_z_lo =
                0.5 * p.pitch - 0.5 * (p.crest_flat + p.axial_clearance) - (int.major_radius - r) * tan_l;

            let gap_hi = nut_z_hi - bolt_z_hi;
            let gap_lo = bolt_z_lo - nut_z_lo;
            assert!(
                gap_hi >= 0.5 * p.axial_clearance + p.radial_clearance * tan_u - 1e-9,
                "upper flank clearance too tight at frac={frac}: {gap_hi}"
            );
            assert!(
                gap_lo >= 0.5 * p.axial_clearance + p.radial_clearance * tan_l - 1e-9,
                "lower flank clearance too tight at frac={frac}: {gap_lo}"
            );
        }
    }

    #[test]
    fn rejects_invalid_pitch_or_diameter() {
        let mut bad_pitch = sample_fdm_params();
        bad_pitch.pitch = 0.85;
        let err = validate_and_depth(&bad_pitch).unwrap_err();
        assert_eq!(err.field, "pitch");

        let mut bad_dia = sample_fdm_params();
        bad_dia.major_diameter = 2.0;
        bad_dia.pitch = 3.0;
        let err = validate_and_depth(&bad_dia).unwrap_err();
        assert_eq!(err.field, "major_diameter");

        let mut bad_fillet = sample_fdm_params();
        bad_fillet.crest_fillet = 0.25;
        let err = validate_and_depth(&bad_fillet).unwrap_err();
        assert_eq!(err.field, "crest_fillet");
    }

    #[test]
    fn filleted_groove_stays_strictly_below_pitch() {
        let mut p = sample_fdm_params();
        p.crest_fillet = 0.12;
        p.root_fillet = 0.12;
        let ext = solve_groove(&p, ThreadKind::External, false, 0.0).unwrap();
        let int = solve_groove(&p, ThreadKind::Internal, true, 0.5 * p.pitch).unwrap();
        let ext_total_w = (ext.polygon[0][1] - ext.polygon[3][1]) + 2.0 * ext.surface_stub;
        let int_total_w = (int.polygon[0][1] - int.polygon[3][1]) + 2.0 * int.surface_stub;
        assert!(ext_total_w < p.pitch - MIN_AXIAL_GAP_MM + 1e-9);
        assert!(int_total_w < p.pitch - MIN_AXIAL_GAP_MM + 1e-9);
    }

    #[test]
    fn axis_frame_is_orthonormal_for_arbitrary_vectors() {
        for raw in [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [1.0, 2.0, -3.0]] {
            let f = build_axis_frame([10.0, -5.0, 3.0], raw).unwrap();
            assert!((norm3(f.x_dir) - 1.0).abs() < 1e-9);
            assert!((norm3(f.axis) - 1.0).abs() < 1e-9);
            assert!(dot3(f.x_dir, f.axis).abs() < 1e-9);
            let n = cross3(f.x_dir, f.axis);
            assert!((norm3(n) - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn extracts_span_and_kind_from_edges_and_mesh() {
        let edges = json!({
            "edges": [
                {"name": "F2:+z|F3:side", "start": [5.0, 0.0, 20.0], "mid": [-5.0, 0.0, 20.0], "end": [5.0, 0.0, 20.0]},
                {"name": "F2:-z|F3:side", "start": [5.0, 0.0, 0.0], "mid": [-5.0, 0.0, 0.0], "end": [5.0, 0.0, 0.0]}
            ]
        });
        let (zmin, zmax) = axial_span_from_edges(&edges, "F3:side", [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]).unwrap();
        assert!((zmin - 0.0).abs() < 1e-9);
        assert!((zmax - 20.0).abs() < 1e-9);

        // Outward cylinder triangle at r=5: vertices all on r=5, normal roughly +X.
        let c5 = 5.0 * (0.2_f64).cos();
        let s5 = 5.0 * (0.2_f64).sin();
        let ext_mesh = json!({
            "positions": [[5.0, 0.0, 0.0], [c5, s5, 0.0], [5.0, 0.0, 1.0]],
            "triangles": [[0, 1, 2]]
        });
        assert_eq!(
            detect_kind_from_mesh(&ext_mesh, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 5.0, 0.0, 20.0),
            Some(ThreadKind::External)
        );

        // Inward bore triangle at r=5: reversed winding has normal roughly -X.
        let int_mesh = json!({
            "positions": [[5.0, 0.0, 0.0], [5.0, 0.0, 1.0], [c5, s5, 0.0]],
            "triangles": [[0, 1, 2]]
        });
        assert_eq!(
            detect_kind_from_mesh(&int_mesh, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 5.0, 0.0, 20.0),
            Some(ThreadKind::Internal)
        );
    }

    #[test]
    fn asymmetric_buttress_pair_maintains_positive_flank_clearances() {
        let mut p = sample_fdm_params();
        p.major_diameter = 24.0;
        p.pitch = 4.0;
        p.upper_flank_angle_deg = 12.0;
        p.lower_flank_angle_deg = 45.0;
        p.radial_clearance = 0.25;
        p.axial_clearance = 0.15;

        let ext = solve_groove(&p, ThreadKind::External, false, 0.0).unwrap();
        let nut_phase = nut_axial_phase_offset(&p, ext.thread_depth);
        let int = solve_groove(&p, ThreadKind::Internal, true, nut_phase).unwrap();

        let tan_u = p.upper_flank_angle_deg.to_radians().tan();
        let tan_l = p.lower_flank_angle_deg.to_radians().tan();
        let tooth_crest_center = 0.5 * (ext.polygon[0][1] + (p.pitch + ext.polygon[3][1]));
        assert!((tooth_crest_center - nut_phase).abs() < 1e-9);

        for i in 0..=10 {
            let frac = i as f64 / 10.0;
            let r = ext.minor_radius + frac * ext.thread_depth;
            let bolt_z_hi = tooth_crest_center + 0.5 * p.crest_flat + (ext.major_radius - r) * tan_u;
            let bolt_z_lo = tooth_crest_center - 0.5 * p.crest_flat - (ext.major_radius - r) * tan_l;

            let nut_z_hi = nut_phase + 0.5 * (p.crest_flat + p.axial_clearance) + (int.major_radius - r) * tan_u;
            let nut_z_lo = nut_phase - 0.5 * (p.crest_flat + p.axial_clearance) - (int.major_radius - r) * tan_l;

            let gap_hi = nut_z_hi - bolt_z_hi;
            let gap_lo = bolt_z_lo - nut_z_lo;
            assert!(
                (gap_hi - (0.5 * p.axial_clearance + p.radial_clearance * tan_u)).abs() < 1e-9,
                "unexpected upper gap at frac={frac}: {gap_hi}"
            );
            assert!(
                (gap_lo - (0.5 * p.axial_clearance + p.radial_clearance * tan_l)).abs() < 1e-9,
                "unexpected lower gap at frac={frac}: {gap_lo}"
            );
        }
    }
}

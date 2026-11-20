use crate::math::{
    AIR_OVERLAP_MM, AxisFrame, EndChamfer, SolvedGroove, ThreadKind, ThreadProfileParams,
    ValidationError, axial_span_from_edges, build_axis_frame, chamfer_expression,
    detect_kind_from_mesh, dot3, parse_vec3, solve_groove,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineCommand {
    pub command: String,
    pub params: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EndCaps {
    pub start_free: bool,
    pub end_free: bool,
}

impl Default for EndCaps {
    fn default() -> Self {
        Self {
            start_free: true,
            end_free: true,
        }
    }
}

/// Computes the 2D groove profile points for `sketch.line` (`closed: true`),
/// optionally adding short axial tangent stubs when `surface_fillet > 0` so
/// `sketch.fillet` creates a crest/root fillet tangent to the cylinder wall.
pub fn groove_sketch_points(g: &SolvedGroove, z_center: f64) -> (Vec<[f64; 2]>, Vec<String>) {
    // Shift base polygon (solved around z = 0 or +pitch/2) by `z_center`.
    let shift = |pt: [f64; 2]| [pt[0], pt[1] + z_center];
    let p_surf_hi = shift(g.polygon[0]);
    let p_deep_hi = shift(g.polygon[1]);
    let p_deep_lo = shift(g.polygon[2]);
    let p_surf_lo = shift(g.polygon[3]);
    let p_air_lo = shift(g.polygon[4]);
    let p_air_hi = shift(g.polygon[5]);

    if g.surface_fillet > 1e-6 {
        // Add short axial stubs along the surface radius (`p_surf_hi[0]`) so
        // `sketch.fillet` at the surface corners blends tangentially into the cylinder/bore.
        let stub = g.surface_stub;
        let r_surf = p_surf_hi[0];
        let r_air = p_air_hi[0];
        let p_stub_hi = [r_surf, p_surf_hi[1] + stub];
        let p_stub_lo = [r_surf, p_surf_lo[1] - stub];
        let p_air_stub_hi = [r_air, p_stub_hi[1]];
        let p_air_stub_lo = [r_air, p_stub_lo[1]];

        // Closed 8-point loop:
        // l1: p_stub_hi -> p_surf_hi (axial stub on surface)
        // l2: p_surf_hi -> p_deep_hi (upper flank)
        // l3: p_deep_hi -> p_deep_lo (groove bottom)
        // l4: p_deep_lo -> p_surf_lo (lower flank)
        // l5: p_surf_lo -> p_stub_lo (axial stub on surface)
        // l6: p_stub_lo -> p_air_stub_lo (radial step into air)
        // l7: p_air_stub_lo -> p_air_stub_hi (air back wall)
        // l8: p_air_stub_hi -> p_stub_hi (radial step back to surface)
        let pts = vec![
            p_stub_hi,
            p_surf_hi,
            p_deep_hi,
            p_deep_lo,
            p_surf_lo,
            p_stub_lo,
            p_air_stub_lo,
            p_air_stub_hi,
        ];
        let mut fillet_corners = Vec::new();
        // Surface corners: l1.end and l4.end
        fillet_corners.push("l1.end".to_string());
        fillet_corners.push("l4.end".to_string());
        if g.bottom_fillet > 1e-6 {
            fillet_corners.push("l2.end".to_string());
            fillet_corners.push("l3.end".to_string());
        }
        (pts, fillet_corners)
    } else {
        // 6-point loop:
        // l1: p_surf_hi -> p_deep_hi
        // l2: p_deep_hi -> p_deep_lo
        // l3: p_deep_lo -> p_surf_lo
        // l4: p_surf_lo -> p_air_lo
        // l5: p_air_lo -> p_air_hi
        // l6: p_air_hi -> p_surf_hi
        let pts = vec![p_surf_hi, p_deep_hi, p_deep_lo, p_surf_lo, p_air_lo, p_air_hi];
        let mut fillet_corners = Vec::new();
        if g.bottom_fillet > 1e-6 {
            fillet_corners.push("l1.end".to_string());
            fillet_corners.push("l2.end".to_string());
        }
        (pts, fillet_corners)
    }
}

/// Appends the SolidCraft commands required to cut a custom helical thread
/// onto `target_body` over axial coordinates `[z_start, z_end]` relative to `frame.origin`.
pub fn append_thread_commands(
    cmds: &mut Vec<EngineCommand>,
    tag: &str,
    target_body: &str,
    frame: &AxisFrame,
    groove: &SolvedGroove,
    z_start: f64,
    z_end: f64,
    ends: EndCaps,
    end_chamfer: EndChamfer,
) -> Result<(), ValidationError> {
    if z_end <= z_start + 0.1 {
        return Err(ValidationError::new(
            "length",
            "Thread span along axis must be > 0.1 mm",
        ));
    }

    let z_span = z_end - z_start;
    let groove_half_w = (groove.polygon[0][1] - groove.polygon[3][1]) * 0.5 + groove.surface_stub;

    // Keep cuts strictly inside blocked ends (shoulders/blind bottoms), while running
    // smoothly past free ends into open air.
    let start_pad = if ends.start_free {
        groove.lead
    } else {
        -(groove_half_w + 0.05)
    };
    let end_pad = if ends.end_free {
        groove.lead
    } else {
        -(groove_half_w + 0.05)
    };

    let z_coil_start = z_start - start_pad;
    let z_coil_end = z_end + end_pad;
    if z_coil_end <= z_coil_start + 0.1 {
        return Err(ValidationError::new(
            "length",
            "Thread length is too short for the requested pitch and end clearances",
        ));
    }

    let revolutions = (z_coil_end - z_coil_start) / groove.lead;
    let sketch_name = format!("{tag}_Profile");
    let coil_name = format!("{tag}_Coil");

    cmds.push(EngineCommand {
        command: "sketch.create".into(),
        params: json!({
            "plane": {
                "origin": frame.origin,
                "x_dir": frame.x_dir,
                "y_dir": frame.axis,
            },
            "name": sketch_name,
        }),
    });

    let (pts, _fillet_corners) = groove_sketch_points(groove, z_coil_start);
    cmds.push(EngineCommand {
        command: "sketch.line".into(),
        params: json!({
            "points": pts,
            "closed": true,
        }),
    });

    // Apply optional fillets cleanly in sketch space.
    if groove.bottom_fillet > 1e-6 {
        let corners = if groove.surface_fillet > 1e-6 {
            vec!["l2.end", "l3.end"]
        } else {
            vec!["l1.end", "l2.end"]
        };
        cmds.push(EngineCommand {
            command: "sketch.fillet".into(),
            params: json!({
                "points": corners,
                "radius": groove.bottom_fillet,
            }),
        });
    }
    if groove.surface_fillet > 1e-6 {
        cmds.push(EngineCommand {
            command: "sketch.fillet".into(),
            params: json!({
                "points": ["l1.end", "l4.end"],
                "radius": groove.surface_fillet,
            }),
        });
    }

    cmds.push(EngineCommand {
        command: "sketch.finish".into(),
        params: json!({}),
    });

    cmds.push(EngineCommand {
        command: "solid.coil".into(),
        params: json!({
            "sketch": sketch_name,
            "pitch": groove.lead,
            "revolutions": revolutions,
            "base": frame.origin,
            "axis": frame.axis,
            "clockwise": groove.left_hand,
            "operation": "cut",
            "targets": [target_body],
            "name": coil_name,
        }),
    });

    cmds.push(EngineCommand {
        command: "browser.visibility".into(),
        params: json!({
            "items": [sketch_name],
            "visible": false,
        }),
    });

    if groove.starts > 1 {
        cmds.push(EngineCommand {
            command: "solid.pattern.circular".into(),
            params: json!({
                "features": [coil_name],
                "axis": {
                    "origin": frame.origin,
                    "dir": frame.axis,
                },
                "count": groove.starts,
                "name": format!("{tag}_Starts"),
            }),
        });
    }

    // Revolved lead-in / lead-out end chamfers on free ends to remove thin feather edges.
    if end_chamfer != EndChamfer::None {
        let chamfer_depth = (groove.thread_depth * 0.95).min(z_span * 0.25);
        if chamfer_depth > 0.05 {
            let cut_start = ends.start_free;
            let cut_end = ends.end_free && end_chamfer == EndChamfer::Both;
            if cut_start || cut_end {
                let chamfer_sketch = format!("{tag}_ChamferSketch");
                cmds.push(EngineCommand {
                    command: "sketch.create".into(),
                    params: json!({
                        "plane": {
                            "origin": frame.origin,
                            "x_dir": frame.x_dir,
                            "y_dir": frame.axis,
                        },
                        "name": chamfer_sketch,
                    }),
                });

                let add_chamfer_tri = |cmds: &mut Vec<EngineCommand>, z_face: f64, dir_into_part: f64| {
                    let z_out = z_face - dir_into_part * AIR_OVERLAP_MM;
                    let z_in = z_face + dir_into_part * chamfer_depth;
                    let pts = match groove.kind {
                        ThreadKind::External => {
                            let r_in = (groove.major_radius - chamfer_depth).max(0.2);
                            let r_out = groove.major_radius + AIR_OVERLAP_MM;
                            vec![[r_in, z_out], [r_out, z_out], [r_out, z_in]]
                        }
                        ThreadKind::Internal => {
                            let r_in = (groove.bore_or_shaft_radius - AIR_OVERLAP_MM).max(0.05);
                            let r_out = groove.bore_or_shaft_radius + chamfer_depth;
                            vec![[r_in, z_out], [r_out, z_out], [r_in, z_in]]
                        }
                    };
                    cmds.push(EngineCommand {
                        command: "sketch.line".into(),
                        params: json!({
                            "points": pts,
                            "closed": true,
                        }),
                    });
                };

                if cut_start {
                    add_chamfer_tri(cmds, z_start, 1.0);
                }
                if cut_end {
                    add_chamfer_tri(cmds, z_end, -1.0);
                }

                cmds.push(EngineCommand {
                    command: "sketch.finish".into(),
                    params: json!({}),
                });

                cmds.push(EngineCommand {
                    command: "solid.revolve".into(),
                    params: json!({
                        "sketch": chamfer_sketch,
                        "axis": {
                            "origin": frame.origin,
                            "dir": frame.axis,
                        },
                        "angle": "360 deg",
                        "operation": "cut",
                        "targets": [target_body],
                        "name": format!("{tag}_Chamfer"),
                    }),
                });
            }
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairMode {
    Pair,
    Bolt,
    Nut,
}

#[derive(Clone, Debug)]
pub struct PairParams {
    pub profile: ThreadProfileParams,
    pub generate: PairMode,
    pub nut_wall_thickness: f64,
    pub pair_spacing: f64,
    /// When true and `generate == PairMode::Bolt`, applies clearance to the bolt directly.
    pub apply_bolt_clearance: bool,
    /// When true and generating a pair, places Nut coaxially around Bolt (z=0..L)
    /// for cross-section clearance inspection (`snapshot --section xz`).
    pub coaxial_overlay: bool,
}

/// Builds a complete standalone command sequence creating a Bolt, Nut, or Bolt+Nut pair.
pub fn build_thread_pair_commands(p: &PairParams) -> Result<Vec<EngineCommand>, ValidationError> {
    build_named_thread_pair_commands(p, "Bolt", "Nut", "")
}

/// Builds a standalone Bolt, Nut, or Bolt+Nut pair with explicit body names and timeline tag prefix.
pub fn build_named_thread_pair_commands(
    p: &PairParams,
    bolt_name: &str,
    nut_name: &str,
    tag_prefix: &str,
) -> Result<Vec<EngineCommand>, ValidationError> {
    if !p.nut_wall_thickness.is_finite() || p.nut_wall_thickness < 0.5 {
        return Err(ValidationError::new(
            "nut_wall_thickness",
            "nut_wall_thickness must be >= 0.5 mm",
        ));
    }
    if !p.pair_spacing.is_finite() || p.pair_spacing < 0.0 {
        return Err(ValidationError::new(
            "pair_spacing",
            "pair_spacing must be >= 0 mm",
        ));
    }

    let bolt_groove = solve_groove(&p.profile, ThreadKind::External, false, 0.0)?;
    let nut_phase = crate::math::nut_axial_phase_offset(&p.profile, bolt_groove.thread_depth);
    let nut_groove = solve_groove(&p.profile, ThreadKind::Internal, true, nut_phase)?;

    let mut cmds = Vec::new();
    let frame_bolt = build_axis_frame([0.0, 0.0, 0.0], [0.0, 0.0, 1.0])?;
    let nut_outer_r = nut_groove.major_radius + p.nut_wall_thickness;

    if matches!(p.generate, PairMode::Pair | PairMode::Bolt) {
        let bolt_tag = if tag_prefix.is_empty() {
            "Bolt".to_string()
        } else {
            format!("{tag_prefix}_Bolt")
        };
        cmds.push(EngineCommand {
            command: "solid.cylinder".into(),
            params: json!({
                "radius": bolt_groove.major_radius,
                "height": p.profile.length,
                "base": [0.0, 0.0, 0.0],
                "axis": "Z",
                "operation": "new",
                "body_name": bolt_name,
                "name": format!("{bolt_tag}Blank"),
            }),
        });
        append_thread_commands(
            &mut cmds,
            &bolt_tag,
            bolt_name,
            &frame_bolt,
            &bolt_groove,
            0.0,
            p.profile.length,
            EndCaps::default(),
            p.profile.end_chamfer,
        )?;
    }

    if matches!(p.generate, PairMode::Pair | PairMode::Nut) {
        let nut_tag = if tag_prefix.is_empty() {
            "Nut".to_string()
        } else {
            format!("{tag_prefix}_Nut")
        };
        let nut_x = if p.generate == PairMode::Pair && !p.coaxial_overlay {
            bolt_groove.major_radius + p.pair_spacing + nut_outer_r
        } else {
            0.0
        };
        let nut_origin = [nut_x, 0.0, 0.0];
        let frame_nut = build_axis_frame(nut_origin, [0.0, 0.0, 1.0])?;

        cmds.push(EngineCommand {
            command: "solid.cylinder".into(),
            params: json!({
                "radius": nut_outer_r,
                "height": p.profile.length,
                "base": nut_origin,
                "axis": "Z",
                "operation": "new",
                "body_name": nut_name,
                "name": format!("{nut_tag}Blank"),
            }),
        });
        cmds.push(EngineCommand {
            command: "solid.cylinder".into(),
            params: json!({
                "radius": nut_groove.bore_or_shaft_radius,
                "height": p.profile.length + 2.0,
                "base": [nut_x, 0.0, -1.0],
                "axis": "Z",
                "operation": "cut",
                "targets": [nut_name],
                "name": format!("{nut_tag}Bore"),
            }),
        });
        append_thread_commands(
            &mut cmds,
            &nut_tag,
            nut_name,
            &frame_nut,
            &nut_groove,
            0.0,
            p.profile.length,
            EndCaps::default(),
            p.profile.end_chamfer,
        )?;
    }

    Ok(cmds)
}

/// Determines free vs. blocked cylinder end caps by checking adjacent planar face normals
/// in `model.faces` and `model.edges`.
pub fn detect_end_caps(
    faces_reply: &Value,
    edges_reply: &Value,
    cyl_face_name: &str,
    origin: [f64; 3],
    axis: [f64; 3],
    z_min: f64,
    z_max: f64,
) -> EndCaps {
    let mut adj_names = Vec::new();
    if let Some(edges) = edges_reply.get("edges").and_then(Value::as_array) {
        for e in edges {
            let name = e.get("name").and_then(Value::as_str).unwrap_or("");
            let parts: Vec<&str> = name.split('|').collect();
            if parts.contains(&cyl_face_name) {
                for p in parts {
                    if p != cyl_face_name && !p.is_empty() {
                        adj_names.push(p);
                    }
                }
            }
        }
    }

    let mut start_free = false;
    let mut end_free = false;
    if let Some(faces) = faces_reply.get("faces").and_then(Value::as_array) {
        for f in faces {
            let fname = f.get("name").and_then(Value::as_str).unwrap_or("");
            if !adj_names.contains(&fname) {
                continue;
            }
            let Some(normal) = f.get("plane_normal").and_then(parse_vec3) else {
                continue;
            };
            let Some(pt) = f.get("point").and_then(parse_vec3) else {
                continue;
            };
            let z = dot3([pt[0] - origin[0], pt[1] - origin[1], pt[2] - origin[2]], axis);
            let ndot = dot3(normal, axis);
            if (z - z_min).abs() < 0.5 && ndot < -0.5 {
                start_free = true;
            }
            if (z - z_max).abs() < 0.5 && ndot > 0.5 {
                end_free = true;
            }
        }
    }

    EndCaps {
        start_free,
        end_free,
    }
}

/// Extracts `body`, `face_index`, `face_name`, `origin`, `axis`, `radius`, `z_min`, `z_max`,
/// and inferred `ThreadKind` from `model.faces`, `model.edges`, and `model.mesh`.
pub fn inspect_target_cylinder(
    body: &str,
    picked_index: Option<usize>,
    picked_point: Option<[f64; 3]>,
    faces_reply: &Value,
    edges_reply: &Value,
    mesh_reply: Option<&Value>,
) -> Result<(String, AxisFrame, f64, f64, f64, EndCaps, Option<ThreadKind>), ValidationError> {
    let faces = faces_reply
        .get("faces")
        .and_then(Value::as_array)
        .ok_or_else(|| ValidationError::new("face", "SolidCraft returned no faces for body"))?;

    let cyl_face = if let Some(idx) = picked_index {
        faces
            .iter()
            .find(|f| f.get("index").and_then(Value::as_u64) == Some(idx as u64))
    } else if let Some(pt) = picked_point {
        faces
            .iter()
            .filter(|f| f.get("cylinder").is_some_and(Value::is_object))
            .min_by(|a, b| {
                let da = a
                    .get("point")
                    .and_then(parse_vec3)
                    .map(|p| {
                        let d = [p[0] - pt[0], p[1] - pt[1], p[2] - pt[2]];
                        dot3(d, d)
                    })
                    .unwrap_or(f64::INFINITY);
                let db = b
                    .get("point")
                    .and_then(parse_vec3)
                    .map(|p| {
                        let d = [p[0] - pt[0], p[1] - pt[1], p[2] - pt[2]];
                        dot3(d, d)
                    })
                    .unwrap_or(f64::INFINITY);
                da.total_cmp(&db)
            })
    } else {
        None
    }
    .ok_or_else(|| ValidationError::new("face", format!("Selected face not found on body {body}")))?;

    let cyl = cyl_face
        .get("cylinder")
        .filter(|c| c.is_object())
        .ok_or_else(|| {
            ValidationError::new(
                "face",
                "Selected face is not a cylinder. Pick a cylindrical shaft or hole face.",
            )
        })?;

    let face_name = cyl_face
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ValidationError::new("face", "Selected cylindrical face has no name"))?
        .to_string();
    let origin = cyl
        .get("origin")
        .and_then(parse_vec3)
        .ok_or_else(|| ValidationError::new("face", "Cylinder missing origin"))?;
    let raw_axis = cyl
        .get("axis")
        .and_then(parse_vec3)
        .ok_or_else(|| ValidationError::new("face", "Cylinder missing axis"))?;
    let radius = cyl
        .get("radius")
        .and_then(Value::as_f64)
        .ok_or_else(|| ValidationError::new("face", "Cylinder missing radius"))?;

    let frame = build_axis_frame(origin, raw_axis)?;
    let (z_min, z_max) = axial_span_from_edges(edges_reply, &face_name, frame.origin, frame.axis)
        .ok_or_else(|| {
            ValidationError::new(
                "face",
                format!("Could not determine axial bounds of cylindrical face {face_name}"),
            )
        })?;
    let end_caps = detect_end_caps(
        faces_reply,
        edges_reply,
        &face_name,
        frame.origin,
        frame.axis,
        z_min,
        z_max,
    );
    let detected = mesh_reply.and_then(|m| {
        detect_kind_from_mesh(m, frame.origin, frame.axis, radius, z_min, z_max)
    });

    Ok((face_name, frame, radius, z_min, z_max, end_caps, detected))
}

/// Validates an explicit user-supplied `param_prefix` (must match `[A-Za-z][A-Za-z0-9_]*`).
pub fn validate_param_prefix(raw: &str) -> Result<String, ValidationError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 24 {
        return Err(ValidationError::new(
            "param_prefix",
            "param_prefix must be 1-24 characters starting with a letter",
        ));
    }
    let mut chars = trimmed.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(ValidationError::new(
            "param_prefix",
            "param_prefix must start with a letter and contain only letters, digits, or '_'",
        ));
    }
    Ok(trimmed.to_string())
}

/// Allocates a unique parameter prefix (`t1`, `t2`, ...) or validates `requested` against existing parameters.
pub fn allocate_param_prefix(
    requested: Option<&str>,
    params_list: &Value,
) -> Result<String, ValidationError> {
    let existing: Vec<String> = params_list
        .get("parameters")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let in_use = |cand: &str| {
        let dotted = format!("{cand}_");
        existing.iter().any(|n| n == cand || n.starts_with(&dotted))
    };

    if let Some(req) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        let valid = validate_param_prefix(req)?;
        if in_use(&valid) {
            return Err(ValidationError::new(
                "param_prefix",
                format!("Parameter prefix '{valid}' is already in use in this design"),
            ));
        }
        return Ok(valid);
    }

    let mut idx = 1usize;
    loop {
        let candidate = format!("t{idx}");
        if !in_use(&candidate) {
            return Ok(candidate);
        }
        idx += 1;
    }
}

/// Persistent JSON metadata stored inside `<prefix>_meta` so `edit-thread` can inspect and mutate a thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadMeta {
    pub v: u32,
    pub prefix: String,
    pub kind: String,
    pub bodies: Vec<String>,
    pub coils: Vec<String>,
    pub chamfers: Vec<String>,
    #[serde(default)]
    pub offset_feature: Option<String>,
    #[serde(default)]
    pub group_id: Option<u64>,
    pub apply_clearance: bool,
    pub has_crest_fillet: bool,
    pub has_root_fillet: bool,
    pub has_major_d: bool,
    pub has_nut_wall: bool,
    pub has_spacing: bool,
    pub left_hand: bool,
    pub end_chamfer: String,
    #[serde(default)]
    pub face_span: Option<f64>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub chamfer_size: Option<String>,
    #[serde(default)]
    pub offset_mode: Option<String>,
    #[serde(default)]
    pub base_radius: Option<f64>,
    #[serde(default)]
    pub major_is_favorite: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ThreadFeatures {
    pub offset_feature: Option<String>,
    pub sketch: String,
    pub coil: String,
    pub starts_pattern: String,
    pub chamfer_sketch: Option<String>,
    pub chamfer: Option<String>,
}

impl ThreadFeatures {
    pub fn features_in_order(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(off) = &self.offset_feature {
            out.push(off.clone());
        }
        out.push(self.sketch.clone());
        out.push(self.coil.clone());
        out.push(self.starts_pattern.clone());
        if let Some(cs) = &self.chamfer_sketch {
            out.push(cs.clone());
        }
        if let Some(ch) = &self.chamfer {
            out.push(ch.clone());
        }
        out
    }
}

fn add_param(
    cmds: &mut Vec<EngineCommand>,
    name: impl Into<String>,
    expression: impl Into<String>,
    unit: &str,
    favorite: bool,
    comment: &str,
) {
    cmds.push(EngineCommand {
        command: "parameters.add".into(),
        params: json!({
            "name": name.into(),
            "expression": expression.into(),
            "unit": unit,
            "favorite": favorite,
            "comment": comment,
        }),
    });
}

fn push_common_parameters(
    cmds: &mut Vec<EngineCommand>,
    p_prefix: &str,
    prof: &ThreadProfileParams,
    major_d_favorite: bool,
    include_clearances: bool,
    include_nut_wall: Option<f64>,
    include_spacing: Option<f64>,
    base_radius: Option<f64>,
) {
    let mut n = 1usize;
    let mut next_comment = |label: &str| {
        let c = format!("{n}. {label}");
        n += 1;
        c
    };

    if major_d_favorite {
        add_param(
            cmds,
            format!("{p_prefix}_major_d"),
            format!("{:.6} mm", prof.major_diameter),
            "mm",
            true,
            &next_comment("Major Diameter"),
        );
    } else {
        add_param(
            cmds,
            format!("{p_prefix}_major_d"),
            format!("{:.6} mm", prof.major_diameter),
            "mm",
            false,
            "Nominal Major Diameter",
        );
    }
    add_param(
        cmds,
        format!("{p_prefix}_pitch"),
        format!("{:.6} mm", prof.pitch),
        "mm",
        true,
        &next_comment("Pitch"),
    );
    add_param(
        cmds,
        format!("{p_prefix}_length"),
        format!("{:.6} mm", prof.length),
        "mm",
        true,
        &next_comment("Length"),
    );
    add_param(
        cmds,
        format!("{p_prefix}_starts"),
        format!("{}", prof.starts),
        "",
        true,
        &next_comment("Starts (1-8)"),
    );
    add_param(
        cmds,
        format!("{p_prefix}_upper_ang"),
        format!("{:.6} deg", prof.upper_flank_angle_deg),
        "deg",
        true,
        &next_comment("Upper Flank Angle"),
    );
    add_param(
        cmds,
        format!("{p_prefix}_lower_ang"),
        format!("{:.6} deg", prof.lower_flank_angle_deg),
        "deg",
        true,
        &next_comment("Lower Flank Angle"),
    );
    if include_clearances {
        add_param(
            cmds,
            format!("{p_prefix}_radial_clr"),
            format!("{:.6} mm", prof.radial_clearance),
            "mm",
            true,
            &next_comment("Radial Clearance"),
        );
        add_param(
            cmds,
            format!("{p_prefix}_axial_clr"),
            format!("{:.6} mm", prof.axial_clearance),
            "mm",
            true,
            &next_comment("Axial Backlash"),
        );
    }
    add_param(
        cmds,
        format!("{p_prefix}_crest_flat"),
        format!("{:.6} mm", prof.crest_flat),
        "mm",
        true,
        &next_comment("Crest Flat"),
    );
    add_param(
        cmds,
        format!("{p_prefix}_root_flat"),
        format!("{:.6} mm", prof.root_flat),
        "mm",
        true,
        &next_comment("Root Flat"),
    );
    let has_crest = prof.crest_fillet > 1e-6;
    let has_root = prof.root_fillet > 1e-6;
    if has_crest {
        add_param(
            cmds,
            format!("{p_prefix}_crest_fillet"),
            format!("{:.6} mm", prof.crest_fillet),
            "mm",
            true,
            &next_comment("Crest Fillet"),
        );
    }
    if has_root {
        add_param(
            cmds,
            format!("{p_prefix}_root_fillet"),
            format!("{:.6} mm", prof.root_fillet),
            "mm",
            true,
            &next_comment("Root Fillet"),
        );
    }
    if let Some(wall) = include_nut_wall {
        add_param(
            cmds,
            format!("{p_prefix}_nut_wall"),
            format!("{wall:.6} mm"),
            "mm",
            true,
            &next_comment("Nut Wall Thickness"),
        );
    }
    if let Some(spacing) = include_spacing {
        add_param(
            cmds,
            format!("{p_prefix}_spacing"),
            format!("{spacing:.6} mm"),
            "mm",
            true,
            &next_comment("Pair Spacing"),
        );
    }

    // Derived non-favorite formulas driving sketches and features.
    add_param(
        cmds,
        format!("{p_prefix}_depth"),
        format!(
            "({p_prefix}_pitch - {p_prefix}_crest_flat - {p_prefix}_root_flat) / (tan({p_prefix}_upper_ang) + tan({p_prefix}_lower_ang))"
        ),
        "mm",
        false,
        "Derived nominal thread depth",
    );
    add_param(
        cmds,
        format!("{p_prefix}_chamfer"),
        chamfer_expression(p_prefix, prof.chamfer_size, include_nut_wall.is_some()),
        "mm",
        false,
        "Derived lead-in chamfer depth",
    );
    if let Some(r0) = base_radius {
        add_param(
            cmds,
            format!("{p_prefix}_base_r"),
            format!("{r0:.6} mm"),
            "mm",
            false,
            "Picked cylinder base radius",
        );
    }
    if has_crest || has_root {
        add_param(
            cmds,
            format!("{p_prefix}_bu"),
            format!("(90 deg - {p_prefix}_upper_ang) / 2"),
            "deg",
            false,
            "Upper flank complement half-angle",
        );
        add_param(
            cmds,
            format!("{p_prefix}_bl"),
            format!("(90 deg - {p_prefix}_lower_ang) / 2"),
            "deg",
            false,
            "Lower flank complement half-angle",
        );
        if has_crest {
            add_param(
                cmds,
                format!("{p_prefix}_tcu"),
                format!("{p_prefix}_crest_fillet * tan({p_prefix}_bu)"),
                "mm",
                false,
                "Crest upper tangent trim",
            );
            add_param(
                cmds,
                format!("{p_prefix}_tcl"),
                format!("{p_prefix}_crest_fillet * tan({p_prefix}_bl)"),
                "mm",
                false,
                "Crest lower tangent trim",
            );
        }
        if has_root {
            add_param(
                cmds,
                format!("{p_prefix}_tru"),
                format!("{p_prefix}_root_fillet * tan({p_prefix}_bu)"),
                "mm",
                false,
                "Root upper tangent trim",
            );
            add_param(
                cmds,
                format!("{p_prefix}_trl"),
                format!("{p_prefix}_root_fillet * tan({p_prefix}_bl)"),
                "mm",
                false,
                "Root lower tangent trim",
            );
        }
    }
}

pub fn build_pair_parameter_commands(prefix: &str, p: &PairParams) -> Vec<EngineCommand> {
    let mut cmds = Vec::new();
    let include_nut_wall = matches!(p.generate, PairMode::Pair | PairMode::Nut)
        .then_some(p.nut_wall_thickness);
    let include_spacing = (p.generate == PairMode::Pair && !p.coaxial_overlay)
        .then_some(p.pair_spacing);
    let include_clearances = p.generate != PairMode::Bolt || p.apply_bolt_clearance;
    push_common_parameters(
        &mut cmds,
        prefix,
        &p.profile,
        true,
        include_clearances,
        include_nut_wall,
        include_spacing,
        None,
    );
    cmds
}

pub fn build_apply_parameter_commands(
    prefix: &str,
    prof: &ThreadProfileParams,
    base_radius: f64,
    include_clearances: bool,
    major_d_favorite: bool,
) -> Vec<EngineCommand> {
    let mut cmds = Vec::new();
    push_common_parameters(
        &mut cmds,
        prefix,
        prof,
        major_d_favorite,
        include_clearances,
        None,
        None,
        Some(base_radius),
    );
    cmds
}

#[derive(Clone, Debug)]
pub enum ParametricRadialSource {
    /// Pair Bolt or External non-clearance cylinder using `<p>_major_d / 2`.
    MajorDiameterNominal,
    /// Pair Nut using `<p>_major_d / 2 - <p>_depth + <p>_radial_clr` bore.
    NutWithClearance,
    /// Unified `<p>_major_d`-driven external shaft or nominal-major internal bore.
    MajorParam { apply_clearance: bool },
    /// Legacy `v0.2.0` mode using `<p>_base_r` with optional clearance offset.
    PickedRadius { apply_clearance: bool },
}

#[derive(Clone, Debug)]
pub enum ParametricAnchor {
    StartFixed(f64),
    EndFixed(f64),
}

#[derive(Clone, Debug)]
pub struct ParametricThreadSpec {
    pub tag: String,
    pub prefix: String,
    pub body: String,
    pub frame: AxisFrame,
    pub origin_expr: Option<[String; 3]>,
    pub kind: ThreadKind,
    pub radial_source: ParametricRadialSource,
    pub phase_half_pitch: bool,
    pub anchor: ParametricAnchor,
    pub ends: EndCaps,
    pub end_chamfer: EndChamfer,
    pub left_hand: bool,
    pub has_crest_fillet: bool,
    pub has_root_fillet: bool,
}

fn origin_json(frame: &AxisFrame, origin_expr: &Option<[String; 3]>) -> Value {
    match origin_expr {
        Some([x, y, z]) => json!([x, y, z]),
        None => json!(frame.origin),
    }
}

struct CornerFilletExpr {
    radius: String,
    half_angle: String,
    trim: String,
    flank_angle: String,
}

#[allow(clippy::too_many_arguments)]
fn build_parametric_groove_commands(
    cmds: &mut Vec<EngineCommand>,
    s: &ParametricThreadSpec,
    r_surf: &str,
    r_deep: &str,
    r_air: &str,
    bottom_flat: &str,
    z_center_expr: &str,
) {
    let p = &s.prefix;
    let is_ext = s.kind == ThreadKind::External;
    let s_surf = if is_ext { "-" } else { "+" };
    let s_deep = if is_ext { "+" } else { "-" };

    let (ang_hi, ang_lo, b_hi, b_lo) = if is_ext {
        (
            format!("{p}_lower_ang"),
            format!("{p}_upper_ang"),
            format!("{p}_bl"),
            format!("{p}_bu"),
        )
    } else {
        (
            format!("{p}_upper_ang"),
            format!("{p}_lower_ang"),
            format!("{p}_bu"),
            format!("{p}_bl"),
        )
    };

    let (has_b, has_s, surf_hi_f, deep_hi_f, deep_lo_f, surf_lo_f) = if is_ext {
        (
            s.has_root_fillet,
            s.has_crest_fillet,
            CornerFilletExpr {
                radius: format!("{p}_crest_fillet"),
                half_angle: b_hi.clone(),
                trim: format!("{p}_tcl"),
                flank_angle: ang_hi.clone(),
            },
            CornerFilletExpr {
                radius: format!("{p}_root_fillet"),
                half_angle: b_hi.clone(),
                trim: format!("{p}_trl"),
                flank_angle: ang_hi.clone(),
            },
            CornerFilletExpr {
                radius: format!("{p}_root_fillet"),
                half_angle: b_lo.clone(),
                trim: format!("{p}_tru"),
                flank_angle: ang_lo.clone(),
            },
            CornerFilletExpr {
                radius: format!("{p}_crest_fillet"),
                half_angle: b_lo.clone(),
                trim: format!("{p}_tcu"),
                flank_angle: ang_lo.clone(),
            },
        )
    } else {
        (
            s.has_crest_fillet,
            s.has_root_fillet,
            CornerFilletExpr {
                radius: format!("{p}_root_fillet"),
                half_angle: b_hi.clone(),
                trim: format!("{p}_tru"),
                flank_angle: ang_hi.clone(),
            },
            CornerFilletExpr {
                radius: format!("{p}_crest_fillet"),
                half_angle: b_hi.clone(),
                trim: format!("{p}_tcu"),
                flank_angle: ang_hi.clone(),
            },
            CornerFilletExpr {
                radius: format!("{p}_crest_fillet"),
                half_angle: b_lo.clone(),
                trim: format!("{p}_tcl"),
                flank_angle: ang_lo.clone(),
            },
            CornerFilletExpr {
                radius: format!("{p}_root_fillet"),
                half_angle: b_lo.clone(),
                trim: format!("{p}_trl"),
                flank_angle: ang_lo.clone(),
            },
        )
    };

    let depth = format!("{p}_depth");
    let z_dhi = format!("({z_center_expr}) + 0.5 * ({bottom_flat})");
    let z_dlo = format!("({z_center_expr}) - 0.5 * ({bottom_flat})");
    let z_shi = format!("({z_dhi}) + {depth} * tan({ang_hi})");
    let z_slo = format!("({z_dlo}) - {depth} * tan({ang_lo})");

    let mut pending_pts: Vec<Value> = Vec::new();
    let mut prev_ref: Option<String> = None;
    let mut first_ref: Option<String> = None;
    let mut line_idx = 0usize;
    let mut arc_idx = 0usize;

    let flush_line = |cmds: &mut Vec<EngineCommand>,
                      pts_buf: &mut Vec<Value>,
                      extra_end: Option<Value>,
                      close_to_first: bool,
                      prev_ref: &mut Option<String>,
                      first_ref: &mut Option<String>,
                      line_idx: &mut usize| {
        if pts_buf.is_empty() && extra_end.is_none() && !close_to_first {
            return;
        }
        let mut pts = Vec::new();
        let closed = if let Some(pr) = prev_ref.as_ref() {
            pts.push(json!(pr));
            false
        } else {
            false
        };
        pts.append(pts_buf);
        if let Some(ex) = extra_end {
            pts.push(ex);
        }
        let use_closed_flag = if close_to_first {
            if let Some(fr) = first_ref.as_ref() {
                pts.push(json!(fr));
                false
            } else {
                true
            }
        } else {
            closed
        };
        let n_seg = if use_closed_flag {
            pts.len()
        } else {
            pts.len().saturating_sub(1)
        };
        if n_seg == 0 {
            return;
        }
        let first_line_id = *line_idx + 1;
        *line_idx += n_seg;
        let last_line_id = *line_idx;
        if first_ref.is_none() {
            *first_ref = Some(format!("l{first_line_id}.start"));
        }
        *prev_ref = Some(format!("l{last_line_id}.end"));
        let mut params = json!({ "points": pts });
        if use_closed_flag {
            params["closed"] = json!(true);
        }
        cmds.push(EngineCommand {
            command: "sketch.line".into(),
            params,
        });
    };

    let emit_arc = |cmds: &mut Vec<EngineCommand>,
                    pts_buf: &mut Vec<Value>,
                    start_pt: Value,
                    through_pt: Value,
                    end_pt: Value,
                    prev_ref: &mut Option<String>,
                    first_ref: &mut Option<String>,
                    line_idx: &mut usize,
                    arc_idx: &mut usize| {
        let start_val = if !pts_buf.is_empty() || prev_ref.is_some() {
            flush_line(
                cmds,
                pts_buf,
                Some(start_pt.clone()),
                false,
                prev_ref,
                first_ref,
                line_idx,
            );
            json!(prev_ref.as_ref().expect("prev_ref after flush"))
        } else {
            start_pt
        };
        *arc_idx += 1;
        let a_id = format!("a{}", *arc_idx);
        if first_ref.is_none() {
            *first_ref = Some(format!("{a_id}.start"));
        }
        *prev_ref = Some(format!("{a_id}.end"));
        cmds.push(EngineCommand {
            command: "sketch.arc.three_point".into(),
            params: json!({
                "start": start_val,
                "through": through_pt,
                "end": end_pt,
            }),
        });
    };

    // Corner 0: surf_hi
    if has_s {
        let start = json!([r_surf, format!("({z_shi}) + {}", surf_hi_f.trim)]);
        let through = json!([
            format!("({r_surf}) {s_surf} {} * (1 - cos({}))", surf_hi_f.radius, surf_hi_f.half_angle),
            format!("({z_shi}) + {} - {} * sin({})", surf_hi_f.trim, surf_hi_f.radius, surf_hi_f.half_angle)
        ]);
        let end = json!([
            format!("({r_surf}) {s_surf} {} * cos({})", surf_hi_f.trim, surf_hi_f.flank_angle),
            format!("({z_shi}) - {} * sin({})", surf_hi_f.trim, surf_hi_f.flank_angle)
        ]);
        emit_arc(
            cmds,
            &mut pending_pts,
            start,
            through,
            end,
            &mut prev_ref,
            &mut first_ref,
            &mut line_idx,
            &mut arc_idx,
        );
    } else {
        pending_pts.push(json!([r_surf, z_shi]));
    }

    // Corner 1: deep_hi
    if has_b {
        let start = json!([
            format!("({r_deep}) {s_deep} {} * cos({})", deep_hi_f.trim, deep_hi_f.flank_angle),
            format!("({z_dhi}) + {} * sin({})", deep_hi_f.trim, deep_hi_f.flank_angle)
        ]);
        let through = json!([
            format!("({r_deep}) {s_deep} {} * (1 - cos({}))", deep_hi_f.radius, deep_hi_f.half_angle),
            format!("({z_dhi}) - {} + {} * sin({})", deep_hi_f.trim, deep_hi_f.radius, deep_hi_f.half_angle)
        ]);
        let end = json!([r_deep, format!("({z_dhi}) - {}", deep_hi_f.trim)]);
        emit_arc(
            cmds,
            &mut pending_pts,
            start,
            through,
            end,
            &mut prev_ref,
            &mut first_ref,
            &mut line_idx,
            &mut arc_idx,
        );
    } else {
        pending_pts.push(json!([r_deep, z_dhi]));
    }

    // Corner 2: deep_lo
    if has_b {
        let start = json!([r_deep, format!("({z_dlo}) + {}", deep_lo_f.trim)]);
        let through = json!([
            format!("({r_deep}) {s_deep} {} * (1 - cos({}))", deep_lo_f.radius, deep_lo_f.half_angle),
            format!("({z_dlo}) + {} - {} * sin({})", deep_lo_f.trim, deep_lo_f.radius, deep_lo_f.half_angle)
        ]);
        let end = json!([
            format!("({r_deep}) {s_deep} {} * cos({})", deep_lo_f.trim, deep_lo_f.flank_angle),
            format!("({z_dlo}) - {} * sin({})", deep_lo_f.trim, deep_lo_f.flank_angle)
        ]);
        emit_arc(
            cmds,
            &mut pending_pts,
            start,
            through,
            end,
            &mut prev_ref,
            &mut first_ref,
            &mut line_idx,
            &mut arc_idx,
        );
    } else {
        pending_pts.push(json!([r_deep, z_dlo]));
    }

    // Corner 3: surf_lo
    if has_s {
        let start = json!([
            format!("({r_surf}) {s_surf} {} * cos({})", surf_lo_f.trim, surf_lo_f.flank_angle),
            format!("({z_slo}) + {} * sin({})", surf_lo_f.trim, surf_lo_f.flank_angle)
        ]);
        let through = json!([
            format!("({r_surf}) {s_surf} {} * (1 - cos({}))", surf_lo_f.radius, surf_lo_f.half_angle),
            format!("({z_slo}) - {} + {} * sin({})", surf_lo_f.trim, surf_lo_f.radius, surf_lo_f.half_angle)
        ]);
        let end = json!([r_surf, format!("({z_slo}) - {}", surf_lo_f.trim)]);
        emit_arc(
            cmds,
            &mut pending_pts,
            start,
            through,
            end,
            &mut prev_ref,
            &mut first_ref,
            &mut line_idx,
            &mut arc_idx,
        );
    } else {
        pending_pts.push(json!([r_surf, z_slo]));
    }

    // Air relief points matched in Z to outer loop entry/exit
    let z_outer_lo = if has_s {
        format!("({z_slo}) - {}", surf_lo_f.trim)
    } else {
        z_slo
    };
    let z_outer_hi = if has_s {
        format!("({z_shi}) + {}", surf_hi_f.trim)
    } else {
        z_shi
    };
    pending_pts.push(json!([r_air, z_outer_lo]));
    pending_pts.push(json!([r_air, z_outer_hi]));
    flush_line(
        cmds,
        &mut pending_pts,
        None,
        true,
        &mut prev_ref,
        &mut first_ref,
        &mut line_idx,
    );
}

/// Emits expression-driven 0-DOF sketches, coil, circular starts pattern (for live starts edits),
/// and optional revolved end chamfers. Returns created feature names for `timeline.group` and `edit-thread`.
pub fn append_parametric_thread_commands(
    cmds: &mut Vec<EngineCommand>,
    s: &ParametricThreadSpec,
) -> Result<ThreadFeatures, ValidationError> {
    let p = &s.prefix;
    let is_ext = s.kind == ThreadKind::External;

    let (r_surf, r_deep, r_air, bottom_flat) = match (&s.radial_source, s.kind) {
        (ParametricRadialSource::MajorDiameterNominal, ThreadKind::External) => (
            format!("{p}_major_d / 2"),
            format!("{p}_major_d / 2 - {p}_depth"),
            format!("{p}_major_d / 2 + 0.2 mm"),
            format!("{p}_root_flat"),
        ),
        (ParametricRadialSource::NutWithClearance, ThreadKind::Internal) => (
            format!("{p}_major_d / 2 - {p}_depth + {p}_radial_clr"),
            format!("{p}_major_d / 2 + {p}_radial_clr"),
            format!("max(0.05 mm, {p}_major_d / 2 - {p}_depth + {p}_radial_clr - 0.2 mm)"),
            format!("{p}_crest_flat + {p}_axial_clr"),
        ),
        (ParametricRadialSource::MajorParam { apply_clearance }, ThreadKind::External) => {
            if *apply_clearance {
                (
                    format!("{p}_major_d / 2 - {p}_radial_clr"),
                    format!("{p}_major_d / 2 - {p}_radial_clr - {p}_depth"),
                    format!("{p}_major_d / 2 - {p}_radial_clr + 0.2 mm"),
                    format!("{p}_root_flat + {p}_axial_clr"),
                )
            } else {
                (
                    format!("{p}_major_d / 2"),
                    format!("{p}_major_d / 2 - {p}_depth"),
                    format!("{p}_major_d / 2 + 0.2 mm"),
                    format!("{p}_root_flat"),
                )
            }
        }
        (ParametricRadialSource::MajorParam { apply_clearance }, ThreadKind::Internal) => {
            if *apply_clearance {
                (
                    format!("{p}_major_d / 2 - {p}_depth + {p}_radial_clr"),
                    format!("{p}_major_d / 2 + {p}_radial_clr"),
                    format!("max(0.05 mm, {p}_major_d / 2 - {p}_depth + {p}_radial_clr - 0.2 mm)"),
                    format!("{p}_crest_flat + {p}_axial_clr"),
                )
            } else {
                (
                    format!("{p}_major_d / 2 - {p}_depth"),
                    format!("{p}_major_d / 2"),
                    format!("max(0.05 mm, {p}_major_d / 2 - {p}_depth - 0.2 mm)"),
                    format!("{p}_crest_flat"),
                )
            }
        }
        (ParametricRadialSource::PickedRadius { apply_clearance }, ThreadKind::External) => {
            if *apply_clearance {
                (
                    format!("{p}_base_r - {p}_radial_clr"),
                    format!("{p}_base_r - {p}_radial_clr - {p}_depth"),
                    format!("{p}_base_r - {p}_radial_clr + 0.2 mm"),
                    format!("{p}_root_flat + {p}_axial_clr"),
                )
            } else {
                (
                    format!("{p}_base_r"),
                    format!("{p}_base_r - {p}_depth"),
                    format!("{p}_base_r + 0.2 mm"),
                    format!("{p}_root_flat"),
                )
            }
        }
        (ParametricRadialSource::PickedRadius { .. }, ThreadKind::Internal) => (
            format!("{p}_base_r + {p}_radial_clr"),
            format!("{p}_base_r + {p}_radial_clr + {p}_depth"),
            format!("max(0.05 mm, {p}_base_r + {p}_radial_clr - 0.2 mm)"),
            format!("{p}_crest_flat + {p}_axial_clr"),
        ),
        _ => {
            return Err(ValidationError::new(
                "kind",
                "Unsupported radial source for thread kind",
            ));
        }
    };

    let (z_lo, z_hi) = match s.anchor {
        ParametricAnchor::StartFixed(z0) => {
            if z0.abs() < 1e-9 {
                ("0 mm".to_string(), format!("{p}_length"))
            } else {
                (format!("{z0:.6} mm"), format!("{z0:.6} mm + {p}_length"))
            }
        }
        ParametricAnchor::EndFixed(z1) => (
            format!("{z1:.6} mm - {p}_length"),
            format!("{z1:.6} mm"),
        ),
    };

    let lead_expr = format!("{p}_pitch * {p}_starts");
    let surf_fillet_active = if is_ext { s.has_crest_fillet } else { s.has_root_fillet };
    let stub_expr = if surf_fillet_active {
        if is_ext {
            format!("max({p}_tcu, {p}_tcl) + 0.02 mm")
        } else {
            format!("max({p}_tru, {p}_trl) + 0.02 mm")
        }
    } else {
        "0 mm".to_string()
    };
    let blocked_pad = format!(
        "-(0.5 * ({bottom_flat} + {p}_depth * (tan({p}_upper_ang) + tan({p}_lower_ang))) + {stub_expr} + 0.05 mm)"
    );
    let start_pad = if s.ends.start_free {
        lead_expr.clone()
    } else {
        blocked_pad.clone()
    };
    let end_pad = if s.ends.end_free {
        lead_expr.clone()
    } else {
        blocked_pad
    };

    let z_coil_start = if s.ends.start_free && matches!(s.anchor, ParametricAnchor::StartFixed(z0) if z0.abs() < 1e-9) {
        format!("-({lead_expr})")
    } else {
        format!("({z_lo}) - ({start_pad})")
    };
    let revolutions = if s.ends.start_free && s.ends.end_free {
        format!("{p}_length / ({lead_expr}) + 2")
    } else {
        format!("(({z_hi}) + ({end_pad}) - ({z_coil_start})) / ({lead_expr})")
    };

    let z_center_expr = if s.phase_half_pitch {
        format!(
            "({z_coil_start}) + 0.5 * {p}_pitch + 0.5 * {p}_depth * (tan({p}_lower_ang) - tan({p}_upper_ang))"
        )
    } else {
        z_coil_start
    };

    let origin_val = origin_json(&s.frame, &s.origin_expr);
    let sketch_name = format!("{}_Profile", s.tag);
    let coil_name = format!("{}_Coil", s.tag);
    let pattern_name = format!("{}_Starts", s.tag);

    cmds.push(EngineCommand {
        command: "sketch.create".into(),
        params: json!({
            "plane": {
                "origin": origin_val.clone(),
                "x_dir": s.frame.x_dir,
                "y_dir": s.frame.axis,
            },
            "name": sketch_name,
        }),
    });

    build_parametric_groove_commands(
        cmds,
        s,
        &r_surf,
        &r_deep,
        &r_air,
        &bottom_flat,
        &z_center_expr,
    );

    cmds.push(EngineCommand {
        command: "sketch.finish".into(),
        params: json!({}),
    });

    cmds.push(EngineCommand {
        command: "solid.coil".into(),
        params: json!({
            "sketch": sketch_name,
            "pitch": lead_expr,
            "revolutions": revolutions,
            "base": origin_val.clone(),
            "axis": s.frame.axis,
            "clockwise": s.left_hand,
            "operation": "cut",
            "targets": [s.body],
            "name": coil_name,
        }),
    });

    cmds.push(EngineCommand {
        command: "browser.visibility".into(),
        params: json!({
            "items": [sketch_name],
            "visible": false,
        }),
    });

    cmds.push(EngineCommand {
        command: "solid.pattern.circular".into(),
        params: json!({
            "features": [coil_name],
            "axis": {
                "origin": origin_val.clone(),
                "dir": s.frame.axis,
            },
            "count": format!("{p}_starts"),
            "name": pattern_name,
        }),
    });

    let mut chamfer_sketch_out = None;
    let mut chamfer_out = None;
    let cut_start = s.ends.start_free;
    let cut_end = s.ends.end_free && (s.end_chamfer != EndChamfer::Start || !s.ends.start_free);
    if cut_start || cut_end {
        let cs_name = format!("{}_ChamferSketch", s.tag);
        let ch_name = format!("{}_Chamfer", s.tag);

        cmds.push(EngineCommand {
            command: "sketch.create".into(),
            params: json!({
                "plane": {
                    "origin": origin_val.clone(),
                    "x_dir": s.frame.x_dir,
                    "y_dir": s.frame.axis,
                },
                "name": cs_name,
            }),
        });

        let cham = format!("{p}_chamfer");
        let (r_in, r_out) = if is_ext {
            (
                format!("max(0.2 mm, ({r_surf}) - {cham})"),
                format!("({r_surf}) + 0.2 mm"),
            )
        } else {
            (
                format!("max(0.05 mm, ({r_surf}) - 0.2 mm)"),
                format!("({r_surf}) + {cham}"),
            )
        };

        if cut_start {
            let z_out = if z_lo == "0 mm" {
                "-0.2 mm".to_string()
            } else {
                format!("({z_lo}) - 0.2 mm")
            };
            let z_in = if z_lo == "0 mm" {
                cham.clone()
            } else {
                format!("({z_lo}) + {cham}")
            };
            let pts = if is_ext {
                json!([[r_in, z_out], [r_out, z_out], [r_out, z_in]])
            } else {
                json!([[r_in, z_out], [r_out, z_out], [r_in, z_in]])
            };
            cmds.push(EngineCommand {
                command: "sketch.line".into(),
                params: json!({ "points": pts, "closed": true }),
            });
        }

        if cut_end {
            let z_out = format!("({z_hi}) + 0.2 mm");
            let z_in = format!("({z_hi}) - {cham}");
            let pts = if is_ext {
                json!([[r_in, z_out], [r_out, z_out], [r_out, z_in]])
            } else {
                json!([[r_in, z_out], [r_out, z_out], [r_in, z_in]])
            };
            cmds.push(EngineCommand {
                command: "sketch.line".into(),
                params: json!({ "points": pts, "closed": true }),
            });
        }

        cmds.push(EngineCommand {
            command: "sketch.finish".into(),
            params: json!({}),
        });

        cmds.push(EngineCommand {
            command: "solid.revolve".into(),
            params: json!({
                "sketch": cs_name,
                "axis": {
                    "origin": origin_val,
                    "dir": s.frame.axis,
                },
                "angle": "360 deg",
                "operation": "cut",
                "targets": [s.body],
                "name": ch_name,
            }),
        });

        if s.end_chamfer == EndChamfer::None {
            cmds.push(EngineCommand {
                command: "timeline.suppress".into(),
                params: json!({
                    "features": [ch_name],
                    "suppressed": true,
                }),
            });
        }

        chamfer_sketch_out = Some(cs_name);
        chamfer_out = Some(ch_name);
    }

    Ok(ThreadFeatures {
        offset_feature: None,
        sketch: sketch_name,
        coil: coil_name,
        starts_pattern: pattern_name,
        chamfer_sketch: chamfer_sketch_out,
        chamfer: chamfer_out,
    })
}

pub struct ParametricPairBuild {
    pub param_cmds: Vec<EngineCommand>,
    pub bolt_cmds: Vec<EngineCommand>,
    pub bolt_features: Vec<String>,
    pub bolt_coil: Option<String>,
    pub bolt_chamfer: Option<String>,
    pub nut_cmds: Vec<EngineCommand>,
    pub nut_features: Vec<String>,
    pub nut_coil: Option<String>,
    pub nut_chamfer: Option<String>,
    pub bodies: Vec<String>,
}

/// Builds a parametric Bolt, Nut, or Bolt+Nut pair separated cleanly into parameters, Bolt features,
/// and Nut features so callers can wrap them into nested `timeline.group` folders.
pub fn build_parametric_thread_pair(
    prefix: &str,
    p: &PairParams,
    bolt_name: &str,
    nut_name: &str,
    tag_prefix: &str,
) -> Result<ParametricPairBuild, ValidationError> {
    if !p.nut_wall_thickness.is_finite() || p.nut_wall_thickness < 0.5 {
        return Err(ValidationError::new(
            "nut_wall_thickness",
            "nut_wall_thickness must be >= 0.5 mm",
        ));
    }
    if !p.pair_spacing.is_finite() || p.pair_spacing < 0.0 {
        return Err(ValidationError::new(
            "pair_spacing",
            "pair_spacing must be >= 0 mm",
        ));
    }
    let _ = crate::math::validate_and_depth(&p.profile)?;

    let param_cmds = build_pair_parameter_commands(prefix, p);
    let frame_bolt = build_axis_frame([0.0, 0.0, 0.0], [0.0, 0.0, 1.0])?;
    let has_crest_fillet = p.profile.crest_fillet > 1e-6;
    let has_root_fillet = p.profile.root_fillet > 1e-6;

    let mut bolt_cmds = Vec::new();
    let mut bolt_features = Vec::new();
    let mut bolt_coil = None;
    let mut bolt_chamfer = None;
    let mut bodies = Vec::new();

    if matches!(p.generate, PairMode::Pair | PairMode::Bolt) {
        let bolt_clr = p.generate == PairMode::Bolt && p.apply_bolt_clearance;
        let bolt_tag = if tag_prefix.is_empty() {
            "Bolt".to_string()
        } else {
            format!("{tag_prefix}_Bolt")
        };
        let blank = format!("{bolt_tag}Blank");
        let bolt_diam_expr = if bolt_clr {
            format!("{prefix}_major_d - 2 * {prefix}_radial_clr")
        } else {
            format!("{prefix}_major_d")
        };
        let bolt_radial_source = if bolt_clr {
            ParametricRadialSource::MajorParam { apply_clearance: true }
        } else {
            ParametricRadialSource::MajorDiameterNominal
        };
        bolt_cmds.push(EngineCommand {
            command: "solid.cylinder".into(),
            params: json!({
                "diameter": bolt_diam_expr,
                "height": format!("{prefix}_length"),
                "base": [0.0, 0.0, 0.0],
                "axis": "Z",
                "operation": "new",
                "body_name": bolt_name,
                "name": blank,
            }),
        });
        bolt_features.push(blank);

        let tf = append_parametric_thread_commands(
            &mut bolt_cmds,
            &ParametricThreadSpec {
                tag: bolt_tag,
                prefix: prefix.to_string(),
                body: bolt_name.to_string(),
                frame: frame_bolt,
                origin_expr: None,
                kind: ThreadKind::External,
                radial_source: bolt_radial_source,
                phase_half_pitch: false,
                anchor: ParametricAnchor::StartFixed(0.0),
                ends: EndCaps::default(),
                end_chamfer: p.profile.end_chamfer,
                left_hand: p.profile.left_hand,
                has_crest_fillet,
                has_root_fillet,
            },
        )?;
        bolt_coil = Some(tf.coil.clone());
        bolt_chamfer = tf.chamfer.clone();
        bolt_features.extend(tf.features_in_order());
        bodies.push(bolt_name.to_string());
    }

    let mut nut_cmds = Vec::new();
    let mut nut_features = Vec::new();
    let mut nut_coil = None;
    let mut nut_chamfer = None;

    if matches!(p.generate, PairMode::Pair | PairMode::Nut) {
        let nut_tag = if tag_prefix.is_empty() {
            "Nut".to_string()
        } else {
            format!("{tag_prefix}_Nut")
        };
        let (nut_origin_expr, bore_base_json, blank_base_json) =
            if p.generate == PairMode::Pair && !p.coaxial_overlay {
                let nx = format!("{prefix}_major_d + {prefix}_radial_clr + {prefix}_nut_wall + {prefix}_spacing");
                (
                    Some([nx.clone(), "0 mm".into(), "0 mm".into()]),
                    json!([nx, "0 mm", "-1 mm"]),
                    json!([nx, "0 mm", "0 mm"]),
                )
            } else {
                (None, json!([0.0, 0.0, -1.0]), json!([0.0, 0.0, 0.0]))
            };

        let nut_blank = format!("{nut_tag}Blank");
        let nut_bore = format!("{nut_tag}Bore");
        nut_cmds.push(EngineCommand {
            command: "solid.cylinder".into(),
            params: json!({
                "diameter": format!("{prefix}_major_d + 2 * {prefix}_radial_clr + 2 * {prefix}_nut_wall"),
                "height": format!("{prefix}_length"),
                "base": blank_base_json,
                "axis": "Z",
                "operation": "new",
                "body_name": nut_name,
                "name": nut_blank,
            }),
        });
        nut_features.push(nut_blank);

        nut_cmds.push(EngineCommand {
            command: "solid.cylinder".into(),
            params: json!({
                "diameter": format!("{prefix}_major_d - 2 * {prefix}_depth + 2 * {prefix}_radial_clr"),
                "height": format!("{prefix}_length + 2 mm"),
                "base": bore_base_json,
                "axis": "Z",
                "operation": "cut",
                "targets": [nut_name],
                "name": nut_bore,
            }),
        });
        nut_features.push(nut_bore);

        let tf = append_parametric_thread_commands(
            &mut nut_cmds,
            &ParametricThreadSpec {
                tag: nut_tag,
                prefix: prefix.to_string(),
                body: nut_name.to_string(),
                frame: frame_bolt,
                origin_expr: nut_origin_expr,
                kind: ThreadKind::Internal,
                radial_source: ParametricRadialSource::NutWithClearance,
                phase_half_pitch: true,
                anchor: ParametricAnchor::StartFixed(0.0),
                ends: EndCaps::default(),
                end_chamfer: p.profile.end_chamfer,
                left_hand: p.profile.left_hand,
                has_crest_fillet,
                has_root_fillet,
            },
        )?;
        nut_coil = Some(tf.coil.clone());
        nut_chamfer = tf.chamfer.clone();
        nut_features.extend(tf.features_in_order());
        bodies.push(nut_name.to_string());
    }

    Ok(ParametricPairBuild {
        param_cmds,
        bolt_cmds,
        bolt_features,
        bolt_coil,
        bolt_chamfer,
        nut_cmds,
        nut_features,
        nut_coil,
        nut_chamfer,
        bodies,
    })
}

use crate::math::{
    AIR_OVERLAP_MM, AxisFrame, EndChamfer, SolvedGroove, ThreadKind, ThreadProfileParams,
    ValidationError, axial_span_from_edges, build_axis_frame, detect_kind_from_mesh, dot3,
    parse_vec3, solve_groove,
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
    let nut_groove = solve_groove(
        &p.profile,
        ThreadKind::Internal,
        true,
        0.5 * p.profile.pitch,
    )?;

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

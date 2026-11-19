pub mod commands;
pub mod math;

use commands::{
    EndCaps, EngineCommand, PairMode, PairParams, ParametricAnchor, ParametricRadialSource,
    ParametricThreadSpec, ThreadMeta, allocate_param_prefix, append_parametric_thread_commands,
    build_apply_parameter_commands, build_parametric_thread_pair, inspect_target_cylinder,
    validate_param_prefix,
};
use math::{
    EndChamfer, ThreadKind, ThreadProfileParams, ValidationError, dot3, parse_vec3,
    validate_and_depth,
};
use std::collections::HashMap;
use serde_json::{Value, json};

/// Host command interface implemented both by the Wasm component host and native test fakes.
pub trait Runner {
    fn run(&mut self, command: &str, args: Value) -> Result<Value, String>;
    fn log(&mut self, _message: &str) {}
}

#[derive(Clone, Debug, PartialEq)]
pub enum PluginAppError {
    Input { field: String, reason: String },
    Command { command: String, reason: String },
}

impl PluginAppError {
    pub fn input(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Input {
            field: field.into(),
            reason: reason.into(),
        }
    }

    pub fn command(command: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Command {
            command: command.into(),
            reason: reason.into(),
        }
    }
}

impl From<ValidationError> for PluginAppError {
    fn from(e: ValidationError) -> Self {
        Self::Input {
            field: e.field,
            reason: e.reason,
        }
    }
}

fn opt_f64(v: &Value, key: &str) -> Result<Option<f64>, PluginAppError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => match x.as_f64() {
            Some(n) if n.is_finite() => Ok(Some(n)),
            _ => Err(PluginAppError::input(
                key,
                format!("{key} must be a finite number"),
            )),
        },
    }
}

fn get_f64(v: &Value, key: &str, default: f64) -> Result<f64, PluginAppError> {
    Ok(opt_f64(v, key)?.unwrap_or(default))
}

fn get_u32(v: &Value, key: &str, default: u32) -> Result<u32, PluginAppError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(x) => match x.as_u64() {
            Some(n) if n <= u32::MAX as u64 => Ok(n as u32),
            _ => Err(PluginAppError::input(
                key,
                format!("{key} must be a positive integer"),
            )),
        },
    }
}

fn opt_u32(v: &Value, key: &str) -> Result<Option<u32>, PluginAppError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(x) => match x.as_u64() {
            Some(n) if n <= u32::MAX as u64 => Ok(Some(n as u32)),
            _ => Err(PluginAppError::input(
                key,
                format!("{key} must be a positive integer"),
            )),
        },
    }
}

fn get_bool(v: &Value, key: &str, default: bool) -> Result<bool, PluginAppError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(x) => x
            .as_bool()
            .ok_or_else(|| PluginAppError::input(key, format!("{key} must be a boolean"))),
    }
}

fn parse_end_chamfer(v: &Value) -> Result<EndChamfer, PluginAppError> {
    match v.get("end_chamfer").and_then(Value::as_str) {
        None | Some("both") => Ok(EndChamfer::Both),
        Some("start") => Ok(EndChamfer::Start),
        Some("none") => Ok(EndChamfer::None),
        Some(other) => Err(PluginAppError::input(
            "end_chamfer",
            format!("unsupported end_chamfer '{other}'"),
        )),
    }
}

fn parse_profile(
    v: &Value,
    default_major: f64,
    default_length: f64,
) -> Result<ThreadProfileParams, PluginAppError> {
    Ok(ThreadProfileParams {
        major_diameter: get_f64(v, "major_diameter", default_major)?,
        pitch: get_f64(v, "pitch", 3.0)?,
        length: get_f64(v, "length", default_length)?,
        starts: get_u32(v, "starts", 1)?,
        left_hand: get_bool(v, "left_hand", false)?,
        upper_flank_angle_deg: get_f64(v, "upper_flank_angle_deg", 45.0)?,
        lower_flank_angle_deg: get_f64(v, "lower_flank_angle_deg", 45.0)?,
        radial_clearance: get_f64(v, "radial_clearance", 0.2)?,
        axial_clearance: get_f64(v, "axial_clearance", 0.1)?,
        crest_flat: get_f64(v, "crest_flat", 0.4)?,
        root_flat: get_f64(v, "root_flat", 0.4)?,
        crest_fillet: get_f64(v, "crest_fillet", 0.0)?,
        root_fillet: get_f64(v, "root_fillet", 0.0)?,
        end_chamfer: parse_end_chamfer(v)?,
    })
}

fn collect_taken_names(doc: &Value) -> (Vec<String>, Vec<String>) {
    let mut features = Vec::new();
    if let Some(tl) = doc.get("timeline").and_then(Value::as_array) {
        for item in tl {
            if let Some(name) = item.get("name").and_then(Value::as_str) {
                features.push(name.to_string());
            }
        }
    }
    let mut bodies = Vec::new();
    if let Some(bs) = doc.get("bodies").and_then(Value::as_array) {
        for item in bs {
            if let Some(name) = item.as_str() {
                bodies.push(name.to_string());
            } else if let Some(name) = item.get("name").and_then(Value::as_str) {
                bodies.push(name.to_string());
            }
        }
    }
    (features, bodies)
}

fn unique_name(base: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == base) {
        return base.to_string();
    }
    let mut idx = 2usize;
    loop {
        let candidate = format!("{base}_{idx}");
        if !taken.iter().any(|t| t == &candidate) {
            return candidate;
        }
        idx += 1;
    }
}

/// Picks `base` or `<base>_2`, `<base>_3`, ... such that neither `<prefix>` nor any
/// `<prefix>_*` feature name already exists in `taken`.
fn unique_prefix(base: &str, taken: &[String]) -> String {
    let in_use = |cand: &str| {
        let dotted = format!("{cand}_");
        taken.iter().any(|t| t == cand || t.starts_with(&dotted))
    };
    if !in_use(base) {
        return base.to_string();
    }
    let mut idx = 2usize;
    loop {
        let candidate = format!("{base}_{idx}");
        if !in_use(&candidate) {
            return candidate;
        }
        idx += 1;
    }
}

fn run_commands<R: Runner>(r: &mut R, cmds: &[EngineCommand]) -> Result<(), PluginAppError> {
    for c in cmds {
        r.run(&c.command, c.params.clone())
            .map_err(|reason| PluginAppError::command(&c.command, reason))?;
    }
    Ok(())
}

/// Resolves the target face `(body, index, point)` from `params.face` (`x-solidcraft-selection`)
/// or falls back to `selection.get {"detail": true}`.
fn resolve_face_pick<R: Runner>(
    r: &mut R,
    params: &Value,
) -> Result<(String, Option<usize>, Option<[f64; 3]>), PluginAppError> {
    if let Some(arr) = params.get("face").and_then(Value::as_array)
        && !arr.is_empty()
    {
        if arr.len() > 1 {
            return Err(PluginAppError::input(
                "face",
                "Select only one cylindrical face at a time",
            ));
        }
        let item = &arr[0];
        let body = item
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| PluginAppError::input("face", "Selected face reference has no body"))?
            .to_string();
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let point = item.get("point").and_then(parse_vec3);
        return Ok((body, index, point));
    }

    let sel = r
        .run("selection.get", json!({"detail": true}))
        .map_err(|e| PluginAppError::command("selection.get", e))?;
    let items = sel
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let faces: Vec<&Value> = items
        .iter()
        .filter(|i| {
            i.get("kind")
                .or_else(|| i.get("type"))
                .and_then(Value::as_str)
                == Some("face")
        })
        .collect();
    if faces.is_empty() {
        return Err(PluginAppError::input(
            "face",
            "Pick a cylindrical shaft or hole face in the viewport (or pre-select one before running).",
        ));
    }
    if faces.len() > 1 {
        return Err(PluginAppError::input(
            "face",
            "Select only one cylindrical face at a time.",
        ));
    }
    let item = faces[0];
    let body = item
        .get("body")
        .and_then(Value::as_str)
        .ok_or_else(|| PluginAppError::input("face", "Selected face has no body name"))?
        .to_string();
    let index = item
        .get("index")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let point = item.get("point").and_then(parse_vec3);
    Ok((body, index, point))
}

fn format_end_chamfer(ec: EndChamfer) -> &'static str {
    match ec {
        EndChamfer::Both => "both",
        EndChamfer::Start => "start",
        EndChamfer::None => "none",
    }
}

fn check_param_change_errors(reply: &Value) -> Result<(), PluginAppError> {
    if let Some(errs) = reply.get("errors").and_then(Value::as_array)
        && !errs.is_empty()
    {
        let msgs: Vec<String> = errs
            .iter()
            .map(|e| {
                let f = e.get("feature").and_then(Value::as_str).unwrap_or("model");
                let r = e.get("error").and_then(Value::as_str).unwrap_or("recompute failed");
                format!("{f}: {r}")
            })
            .collect();
        return Err(PluginAppError::command(
            "parameters.change",
            msgs.join("; "),
        ));
    }
    Ok(())
}

fn store_thread_meta<R: Runner>(r: &mut R, meta: &ThreadMeta) -> Result<(), PluginAppError> {
    let text = serde_json::to_string(meta)
        .map_err(|e| PluginAppError::command("parameters.add", format!("metadata encode error: {e}")))?;
    r.run(
        "parameters.add",
        json!({
            "name": format!("{}_meta", meta.prefix),
            "expression": text,
            "unit": "text",
            "favorite": false,
            "comment": "Custom Threads metadata",
        }),
    )
    .map_err(|e| PluginAppError::command("parameters.add", e))?;
    Ok(())
}

fn extract_param_state(params_list: &Value) -> (HashMap<String, f64>, Vec<ThreadMeta>) {
    let mut values = HashMap::new();
    let mut metas = Vec::new();
    if let Some(arr) = params_list.get("parameters").and_then(Value::as_array) {
        for item in arr {
            let Some(name) = item.get("name").and_then(Value::as_str) else {
                continue;
            };
            if let Some(v) = item.get("value").and_then(Value::as_f64) {
                values.insert(name.to_string(), v);
            }
            if name.ends_with("_meta") {
                let raw = item
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("expression").and_then(Value::as_str));
                if let Some(s) = raw
                    && let Ok(meta) = serde_json::from_str::<ThreadMeta>(s)
                {
                    metas.push(meta);
                }
            }
        }
    }
    (values, metas)
}

pub fn create_thread_pair<R: Runner>(r: &mut R, params: &Value) -> Result<Value, PluginAppError> {
    let generate = match params.get("generate").and_then(Value::as_str) {
        None | Some("pair") => PairMode::Pair,
        Some("bolt") => PairMode::Bolt,
        Some("nut") => PairMode::Nut,
        Some(other) => {
            return Err(PluginAppError::input(
                "generate",
                format!("unsupported generate mode '{other}'"),
            ));
        }
    };

    let requested_prefix = params
        .get("param_prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(req) = requested_prefix {
        validate_param_prefix(req)?;
    }

    let profile = parse_profile(params, 20.0, 20.0)?;
    let nut_wall_thickness = get_f64(params, "nut_wall_thickness", 5.0)?;
    let pair_spacing = get_f64(params, "pair_spacing", 10.0)?;
    let coaxial_overlay = get_bool(params, "coaxial_overlay", false)?;

    // Validate before querying or mutating the design.
    let h_nom = validate_and_depth(&profile)?;

    let pair_params = PairParams {
        profile: profile.clone(),
        generate,
        nut_wall_thickness,
        pair_spacing,
        coaxial_overlay,
    };

    let params_list = r
        .run("parameters.list", json!({"filter": "user"}))
        .map_err(|e| PluginAppError::command("parameters.list", e))?;
    let prefix = allocate_param_prefix(requested_prefix, &params_list)?;

    let doc = r
        .run(
            "document.inspect",
            json!({"timeline": "summary", "bodies": "names"}),
        )
        .map_err(|e| PluginAppError::command("document.inspect", e))?;
    let (taken_features, taken_bodies) = collect_taken_names(&doc);

    let bolt_name = unique_name("Bolt", &taken_bodies);
    let nut_name = unique_name("Nut", &taken_bodies);
    let tag_prefix = if taken_features.iter().any(|n| n.starts_with("Bolt"))
        || taken_features.iter().any(|n| n.starts_with("Nut"))
    {
        unique_prefix("Pair", &taken_features)
    } else {
        String::new()
    };

    let build =
        build_parametric_thread_pair(&prefix, &pair_params, &bolt_name, &nut_name, &tag_prefix)?;

    run_commands(r, &build.param_cmds)?;

    let mut group_ids: Vec<Value> = Vec::new();
    let mut coils = Vec::new();
    let mut chamfers = Vec::new();

    if !build.bolt_cmds.is_empty() {
        run_commands(r, &build.bolt_cmds)?;
        let g = r
            .run(
                "timeline.group",
                json!({
                    "name": format!("Bolt {prefix}"),
                    "features": build.bolt_features,
                }),
            )
            .map_err(|e| PluginAppError::command("timeline.group", e))?;
        if let Some(gid) = g.get("group").and_then(Value::as_u64) {
            group_ids.push(json!(gid));
        }
        if let Some(c) = build.bolt_coil {
            coils.push(c);
        }
        if let Some(ch) = build.bolt_chamfer {
            chamfers.push(ch);
        }
    }

    if !build.nut_cmds.is_empty() {
        run_commands(r, &build.nut_cmds)?;
        let g = r
            .run(
                "timeline.group",
                json!({
                    "name": format!("Nut {prefix}"),
                    "features": build.nut_features,
                }),
            )
            .map_err(|e| PluginAppError::command("timeline.group", e))?;
        if let Some(gid) = g.get("group").and_then(Value::as_u64) {
            group_ids.push(json!(gid));
        }
        if let Some(c) = build.nut_coil {
            coils.push(c);
        }
        if let Some(ch) = build.nut_chamfer {
            chamfers.push(ch);
        }
    }

    let parent_group_id = if group_ids.len() > 1 {
        let g = r
            .run(
                "timeline.group",
                json!({
                    "name": format!("Thread Pair {prefix}"),
                    "groups": group_ids,
                }),
            )
            .map_err(|e| PluginAppError::command("timeline.group", e))?;
        g.get("group").and_then(Value::as_u64)
    } else {
        group_ids.first().and_then(Value::as_u64)
    };

    let kind_str = match generate {
        PairMode::Pair => "pair",
        PairMode::Bolt => "bolt",
        PairMode::Nut => "nut",
    };
    let meta = ThreadMeta {
        v: 1,
        prefix: prefix.clone(),
        kind: kind_str.to_string(),
        bodies: build.bodies.clone(),
        coils,
        chamfers,
        offset_feature: None,
        group_id: parent_group_id,
        apply_clearance: generate != PairMode::Bolt,
        has_crest_fillet: profile.crest_fillet > 1e-6,
        has_root_fillet: profile.root_fillet > 1e-6,
        has_major_d: true,
        has_nut_wall: matches!(generate, PairMode::Pair | PairMode::Nut),
        has_spacing: generate == PairMode::Pair && !coaxial_overlay,
        left_hand: profile.left_hand,
        end_chamfer: format_end_chamfer(profile.end_chamfer).to_string(),
        face_span: None,
    };
    store_thread_meta(r, &meta)?;

    Ok(json!({
        "bodies": build.bodies,
        "param_prefix": prefix,
        "group_id": parent_group_id,
        "major_diameter": profile.major_diameter,
        "minor_diameter": profile.major_diameter - 2.0 * h_nom,
        "pitch": profile.pitch,
        "lead": profile.pitch * (profile.starts as f64),
        "thread_depth": h_nom,
        "radial_clearance": if meta.apply_clearance { profile.radial_clearance } else { 0.0 },
        "axial_clearance": if meta.apply_clearance { profile.axial_clearance } else { 0.0 },
    }))
}

pub fn apply_thread<R: Runner>(r: &mut R, params: &Value) -> Result<Value, PluginAppError> {
    let mode_str = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let forced_kind = match mode_str {
        "auto" => None,
        "external" => Some(ThreadKind::External),
        "internal" => Some(ThreadKind::Internal),
        other => {
            return Err(PluginAppError::input(
                "mode",
                format!("unsupported mode '{other}'"),
            ));
        }
    };
    let apply_clearance_to_external = get_bool(params, "apply_clearance_to_external", false)?;

    let requested_prefix = params
        .get("param_prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(req) = requested_prefix {
        validate_param_prefix(req)?;
    }

    // Parse and validate profile pitch/angles/flats early (using placeholder diameter/length
    // that are replaced with the actual cylinder face measurements right below).
    let raw_length = opt_f64(params, "length")?;
    let mut profile = parse_profile(params, 100.0, 20.0)?;
    let h_nom = validate_and_depth(&profile)?;

    let params_list = r
        .run("parameters.list", json!({"filter": "user"}))
        .map_err(|e| PluginAppError::command("parameters.list", e))?;
    let prefix = allocate_param_prefix(requested_prefix, &params_list)?;

    let (body, picked_index, picked_point) = resolve_face_pick(r, params)?;
    let faces_reply = r
        .run("model.faces", json!({"body": body}))
        .map_err(|e| PluginAppError::command("model.faces", e))?;
    let edges_reply = r
        .run("model.edges", json!({"body": body}))
        .map_err(|e| PluginAppError::command("model.edges", e))?;

    // Inspect cylinder first without mesh so we can size mesh tolerance to the cylinder radius.
    let (face_name, frame, radius, z_min, z_max, end_caps, _) = inspect_target_cylinder(
        &body,
        picked_index,
        picked_point,
        &faces_reply,
        &edges_reply,
        None,
    )?;

    let kind = match forced_kind {
        Some(k) => k,
        None => {
            let tol = (radius * 0.03).clamp(0.05, 0.5);
            let mesh_reply = r
                .run("model.mesh", json!({"body": body, "tolerance": tol}))
                .map_err(|e| PluginAppError::command("model.mesh", e))?;
            let (_, _, _, _, _, _, detected) = inspect_target_cylinder(
                &body,
                picked_index,
                picked_point,
                &faces_reply,
                &edges_reply,
                Some(&mesh_reply),
            )?;
            detected.ok_or_else(|| {
                PluginAppError::input(
                    "mode",
                    "Could not auto-detect external shaft vs. internal hole; select External or Internal explicitly.",
                )
            })?
        }
    };

    let face_span = z_max - z_min;
    let (z_start, z_end, eff_caps, anchor) = match raw_length {
        Some(len) if len > 0.0 && len < face_span - 1e-4 => {
            if end_caps.start_free && !end_caps.end_free {
                (
                    z_min,
                    z_min + len,
                    EndCaps {
                        start_free: true,
                        end_free: false,
                    },
                    ParametricAnchor::StartFixed(z_min),
                )
            } else if end_caps.end_free && !end_caps.start_free {
                (
                    z_max - len,
                    z_max,
                    EndCaps {
                        start_free: false,
                        end_free: true,
                    },
                    ParametricAnchor::EndFixed(z_max),
                )
            } else if let Some(pt) = picked_point {
                let d = [
                    pt[0] - frame.origin[0],
                    pt[1] - frame.origin[1],
                    pt[2] - frame.origin[2],
                ];
                let z_pick = dot3(d, frame.axis);
                if (z_pick - z_min).abs() <= (z_max - z_pick).abs() {
                    (
                        z_min,
                        z_min + len,
                        EndCaps {
                            start_free: end_caps.start_free,
                            end_free: false,
                        },
                        ParametricAnchor::StartFixed(z_min),
                    )
                } else {
                    (
                        z_max - len,
                        z_max,
                        EndCaps {
                            start_free: false,
                            end_free: end_caps.end_free,
                        },
                        ParametricAnchor::EndFixed(z_max),
                    )
                }
            } else {
                (
                    z_min,
                    z_min + len,
                    EndCaps {
                        start_free: end_caps.start_free,
                        end_free: false,
                    },
                    ParametricAnchor::StartFixed(z_min),
                )
            }
        }
        Some(len) if len < 0.0 => {
            return Err(PluginAppError::input(
                "length",
                "length must be >= 0 (0 threads the full face)",
            ));
        }
        _ => {
            let anchor = if end_caps.end_free && !end_caps.start_free {
                ParametricAnchor::EndFixed(z_max)
            } else {
                ParametricAnchor::StartFixed(z_min)
            };
            (z_min, z_max, end_caps, anchor)
        }
    };

    profile.length = z_end - z_start;
    profile.major_diameter = match kind {
        ThreadKind::External => 2.0 * radius,
        ThreadKind::Internal => 2.0 * (radius + h_nom),
    };
    let h_nom = validate_and_depth(&profile)?;

    let apply_clearance = match kind {
        ThreadKind::Internal => true,
        ThreadKind::External => apply_clearance_to_external,
    };

    let doc = r
        .run(
            "document.inspect",
            json!({"timeline": "summary", "bodies": false}),
        )
        .map_err(|e| PluginAppError::command("document.inspect", e))?;
    let (taken_features, _) = collect_taken_names(&doc);
    let tag = unique_prefix("CustomThread", &taken_features);

    let param_cmds = build_apply_parameter_commands(&prefix, &profile, radius, apply_clearance);
    run_commands(r, &param_cmds)?;

    let mut group_features = Vec::new();
    let mut offset_feature_name = None;

    if apply_clearance && profile.radial_clearance > 1e-6 {
        let off_reply = r
            .run(
                "solid.offset_face",
                json!({
                    "faces": [face_name],
                    "body": body,
                    "distance": format!("-{prefix}_radial_clr"),
                }),
            )
            .map_err(|e| PluginAppError::command("solid.offset_face", e))?;
        if let Some(fname) = off_reply.get("name").and_then(Value::as_str) {
            offset_feature_name = Some(fname.to_string());
            group_features.push(fname.to_string());
        } else if let Some(fid) = off_reply.get("feature").and_then(Value::as_u64) {
            group_features.push(fid.to_string());
        }
    }

    let mut thread_cmds = Vec::new();
    let tf = append_parametric_thread_commands(
        &mut thread_cmds,
        &ParametricThreadSpec {
            tag,
            prefix: prefix.clone(),
            body: body.clone(),
            frame,
            origin_expr: None,
            kind,
            radial_source: ParametricRadialSource::PickedRadius { apply_clearance },
            phase_half_pitch: false,
            anchor,
            ends: eff_caps,
            end_chamfer: profile.end_chamfer,
            left_hand: profile.left_hand,
            has_crest_fillet: profile.crest_fillet > 1e-6,
            has_root_fillet: profile.root_fillet > 1e-6,
        },
    )?;
    run_commands(r, &thread_cmds)?;
    group_features.extend(tf.features_in_order());

    let group_reply = r
        .run(
            "timeline.group",
            json!({
                "name": format!("Custom Thread {prefix}"),
                "features": group_features,
            }),
        )
        .map_err(|e| PluginAppError::command("timeline.group", e))?;
    let group_id = group_reply.get("group").and_then(Value::as_u64);

    let chamfers = tf.chamfer.into_iter().collect();
    let kind_str = match kind {
        ThreadKind::External => "external",
        ThreadKind::Internal => "internal",
    };
    let meta = ThreadMeta {
        v: 1,
        prefix: prefix.clone(),
        kind: kind_str.to_string(),
        bodies: vec![body.clone()],
        coils: vec![tf.coil],
        chamfers,
        offset_feature: offset_feature_name,
        group_id,
        apply_clearance,
        has_crest_fillet: profile.crest_fillet > 1e-6,
        has_root_fillet: profile.root_fillet > 1e-6,
        has_major_d: false,
        has_nut_wall: false,
        has_spacing: false,
        left_hand: profile.left_hand,
        end_chamfer: format_end_chamfer(profile.end_chamfer).to_string(),
        face_span: Some(face_span),
    };
    store_thread_meta(r, &meta)?;

    let (eff_maj, eff_min) = match kind {
        ThreadKind::External => {
            let cr = if apply_clearance { profile.radial_clearance } else { 0.0 };
            let r_maj = radius - cr;
            (2.0 * r_maj, 2.0 * (r_maj - h_nom))
        }
        ThreadKind::Internal => {
            let r_bore = radius + profile.radial_clearance;
            let r_maj = r_bore + h_nom;
            (2.0 * r_maj, 2.0 * r_bore)
        }
    };

    Ok(json!({
        "body": body,
        "face": face_name,
        "kind": kind,
        "param_prefix": prefix,
        "group_id": group_id,
        "major_diameter": eff_maj,
        "minor_diameter": eff_min,
        "pitch": profile.pitch,
        "lead": profile.pitch * (profile.starts as f64),
        "thread_depth": h_nom,
        "radial_clearance": if apply_clearance { profile.radial_clearance } else { 0.0 },
        "axial_clearance": if apply_clearance { profile.axial_clearance } else { 0.0 },
    }))
}

fn resolve_target_meta<R: Runner>(
    r: &mut R,
    params: &Value,
    metas: &[ThreadMeta],
) -> Result<ThreadMeta, PluginAppError> {
    if metas.is_empty() {
        return Err(PluginAppError::input(
            "target",
            "No Custom Threads found in this design. Run Apply Thread or Thread Pair first.",
        ));
    }

    if let Some(req) = params
        .get("param_prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return metas
            .iter()
            .rfind(|m| m.prefix == req)
            .cloned()
            .ok_or_else(|| {
                let avail: Vec<&str> = metas.iter().map(|m| m.prefix.as_str()).collect();
                PluginAppError::input(
                    "param_prefix",
                    format!(
                        "No Custom Thread with prefix '{req}' (available: {})",
                        avail.join(", ")
                    ),
                )
            });
    }

    let extract_body = |item: &Value| -> Option<String> {
        item.get("body")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    if let Some(arr) = params.get("target").and_then(Value::as_array)
        && !arr.is_empty()
    {
        if arr.len() > 1 {
            return Err(PluginAppError::input(
                "target",
                "Select only one body or face to identify the thread to edit.",
            ));
        }
        let body = extract_body(&arr[0]).ok_or_else(|| {
            PluginAppError::input("target", "Selected item has no body name.")
        })?;
        return metas
            .iter()
            .rfind(|m| m.bodies.iter().any(|b| b == &body))
            .cloned()
            .ok_or_else(|| {
                PluginAppError::input(
                    "target",
                    format!("Body '{body}' was not created or threaded by Custom Threads."),
                )
            });
    }

    // Check current viewport selection before falling back to latest thread.
    if let Ok(sel) = r.run("selection.get", json!({"detail": true}))
        && let Some(items) = sel.get("items").and_then(Value::as_array)
    {
        for item in items {
            if let Some(body) = extract_body(item)
                && let Some(found) = metas.iter().rfind(|m| m.bodies.iter().any(|b| b == &body))
            {
                return Ok(found.clone());
            }
        }
    }

    Ok(metas.last().unwrap().clone())
}

pub fn edit_thread<R: Runner>(r: &mut R, params: &Value) -> Result<Value, PluginAppError> {
    let new_left_hand = match params.get("left_hand").and_then(Value::as_str) {
        None | Some("keep") => None,
        Some("left") => Some(true),
        Some("right") => Some(false),
        Some(other) => {
            return Err(PluginAppError::input(
                "left_hand",
                format!("unsupported left_hand option '{other}'"),
            ));
        }
    };
    let new_end_chamfer = match params.get("end_chamfer").and_then(Value::as_str) {
        None | Some("keep") => None,
        Some("both") => Some(EndChamfer::Both),
        Some("none") => Some(EndChamfer::None),
        Some(other) => {
            return Err(PluginAppError::input(
                "end_chamfer",
                format!("unsupported end_chamfer option '{other}'"),
            ));
        }
    };

    let ov_major = opt_f64(params, "major_diameter")?;
    let ov_pitch = opt_f64(params, "pitch")?;
    let ov_length = opt_f64(params, "length")?;
    let ov_starts = opt_u32(params, "starts")?;
    let ov_upper = opt_f64(params, "upper_flank_angle_deg")?;
    let ov_lower = opt_f64(params, "lower_flank_angle_deg")?;
    let ov_rad_clr = opt_f64(params, "radial_clearance")?;
    let ov_ax_clr = opt_f64(params, "axial_clearance")?;
    let ov_crest_flat = opt_f64(params, "crest_flat")?;
    let ov_root_flat = opt_f64(params, "root_flat")?;
    let ov_crest_fil = opt_f64(params, "crest_fillet")?;
    let ov_root_fil = opt_f64(params, "root_fillet")?;
    let ov_nut_wall = opt_f64(params, "nut_wall_thickness")?;
    let ov_spacing = opt_f64(params, "pair_spacing")?;

    let params_list = r
        .run("parameters.list", json!({"filter": "user"}))
        .map_err(|e| PluginAppError::command("parameters.list", e))?;
    let (param_vals, metas) = extract_param_state(&params_list);
    let mut meta = resolve_target_meta(r, params, &metas)?;
    let p = &meta.prefix;

    let read_num = |suffix: &str| -> Result<f64, PluginAppError> {
        let key = format!("{p}_{suffix}");
        param_vals.get(&key).copied().ok_or_else(|| {
            PluginAppError::command("parameters.list", format!("missing expected parameter '{key}'"))
        })
    };

    if ov_major.is_some() && !meta.has_major_d {
        return Err(PluginAppError::input(
            "major_diameter",
            "Cannot resize major_diameter on a thread applied to an existing cylinder; edit the base feature instead.",
        ));
    }
    if ov_nut_wall.is_some() && !meta.has_nut_wall {
        return Err(PluginAppError::input(
            "nut_wall_thickness",
            "This thread has no Nut body.",
        ));
    }
    if ov_spacing.is_some() && !meta.has_spacing {
        return Err(PluginAppError::input(
            "pair_spacing",
            "This thread does not have a side-by-side Bolt + Nut spacing parameter.",
        ));
    }
    if let Some(cf) = ov_crest_fil {
        if !meta.has_crest_fillet {
            if cf > 1e-6 {
                return Err(PluginAppError::input(
                    "crest_fillet",
                    "Cannot add crest_fillet after creation (changes sketch arc topology); recreate with crest_fillet > 0.",
                ));
            }
        } else if cf < 0.005 {
            return Err(PluginAppError::input(
                "crest_fillet",
                "Keep crest_fillet >= 0.005 mm on a filleted thread (cannot drop to 0 without recreating).",
            ));
        }
    }
    if let Some(rf) = ov_root_fil {
        if !meta.has_root_fillet {
            if rf > 1e-6 {
                return Err(PluginAppError::input(
                    "root_fillet",
                    "Cannot add root_fillet after creation (changes sketch arc topology); recreate with root_fillet > 0.",
                ));
            }
        } else if rf < 0.005 {
            return Err(PluginAppError::input(
                "root_fillet",
                "Keep root_fillet >= 0.005 mm on a filleted thread (cannot drop to 0 without recreating).",
            ));
        }
    }
    if !meta.apply_clearance {
        if ov_rad_clr.is_some_and(|v| v > 1e-6) {
            return Err(PluginAppError::input(
                "radial_clearance",
                "This thread was created without clearance offsets (external/bolt mode); edit the mating nut instead.",
            ));
        }
        if ov_ax_clr.is_some_and(|v| v > 1e-6) {
            return Err(PluginAppError::input(
                "axial_clearance",
                "This thread was created without clearance offsets (external/bolt mode); edit the mating nut instead.",
            ));
        }
    } else if (meta.kind == "external" || meta.kind == "internal")
        && meta.offset_feature.is_none()
        && ov_rad_clr.is_some_and(|v| v > 1e-6)
    {
        return Err(PluginAppError::input(
            "radial_clearance",
            "This thread was applied with radial_clearance = 0 mm so no OffsetFace feature exists; recreate with radial_clearance > 0.",
        ));
    }

    if let Some(len) = ov_length
        && let Some(max_span) = meta.face_span
        && len > max_span + 1e-4
    {
        return Err(PluginAppError::input(
            "length",
            format!("length ({len:.3} mm) exceeds target cylinder span ({max_span:.3} mm)"),
        ));
    }

    let old_rad_clr = if meta.apply_clearance {
        read_num("radial_clr")?
    } else {
        0.0
    };
    let cur_ax_clr = if meta.apply_clearance {
        read_num("axial_clr")?
    } else {
        0.0
    };
    let cur_pitch = read_num("pitch")?;
    let cur_length = read_num("length")?;
    let cur_starts = read_num("starts")?.round().max(1.0) as u32;
    let cur_upper = read_num("upper_ang")?;
    let cur_lower = read_num("lower_ang")?;
    let cur_crest_flat = read_num("crest_flat")?;
    let cur_root_flat = read_num("root_flat")?;
    let cur_crest_fil = if meta.has_crest_fillet {
        read_num("crest_fillet")?
    } else {
        0.0
    };
    let cur_root_fil = if meta.has_root_fillet {
        read_num("root_fillet")?
    } else {
        0.0
    };
    let new_pitch = ov_pitch.unwrap_or(cur_pitch);
    let new_upper = ov_upper.unwrap_or(cur_upper);
    let new_lower = ov_lower.unwrap_or(cur_lower);
    let new_crest_flat = ov_crest_flat.unwrap_or(cur_crest_flat);
    let new_root_flat = ov_root_flat.unwrap_or(cur_root_flat);

    let est_depth = (new_pitch - new_crest_flat - new_root_flat)
        / (new_upper.to_radians().tan() + new_lower.to_radians().tan());
    let cur_major_d = if meta.has_major_d {
        read_num("major_d")?
    } else {
        let base_r = read_num("base_r")?;
        if meta.kind == "internal" {
            2.0 * (base_r + est_depth)
        } else {
            2.0 * base_r
        }
    };

    let merged = ThreadProfileParams {
        major_diameter: ov_major.unwrap_or(cur_major_d),
        pitch: new_pitch,
        length: ov_length.unwrap_or(cur_length),
        starts: ov_starts.unwrap_or(cur_starts),
        left_hand: new_left_hand.unwrap_or(meta.left_hand),
        upper_flank_angle_deg: new_upper,
        lower_flank_angle_deg: new_lower,
        radial_clearance: ov_rad_clr.unwrap_or(old_rad_clr),
        axial_clearance: ov_ax_clr.unwrap_or(cur_ax_clr),
        crest_flat: new_crest_flat,
        root_flat: new_root_flat,
        crest_fillet: ov_crest_fil.unwrap_or(cur_crest_fil),
        root_fillet: ov_root_fil.unwrap_or(cur_root_fil),
        end_chamfer: match meta.end_chamfer.as_str() {
            "none" => EndChamfer::None,
            "start" => EndChamfer::Start,
            _ => EndChamfer::Both,
        },
    };

    let new_depth = validate_and_depth(&merged)?;
    if let Some(wall) = ov_nut_wall
        && (!wall.is_finite() || wall < 0.5)
    {
        return Err(PluginAppError::input(
            "nut_wall_thickness",
            "nut_wall_thickness must be >= 0.5 mm",
        ));
    }
    if let Some(sp) = ov_spacing
        && (!sp.is_finite() || sp < 0.0)
    {
        return Err(PluginAppError::input(
            "pair_spacing",
            "pair_spacing must be >= 0 mm",
        ));
    }
    if new_end_chamfer == Some(EndChamfer::Both) && meta.chamfers.is_empty() {
        return Err(PluginAppError::input(
            "end_chamfer",
            "This thread was created with end_chamfer: none so no chamfer features exist to enable.",
        ));
    }

    let mut changes = serde_json::Map::new();
    if let Some(v) = ov_major {
        changes.insert(format!("{p}_major_d"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_pitch {
        changes.insert(format!("{p}_pitch"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_length {
        changes.insert(format!("{p}_length"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_starts {
        changes.insert(format!("{p}_starts"), json!(format!("{v}")));
    }
    if let Some(v) = ov_upper {
        changes.insert(format!("{p}_upper_ang"), json!(format!("{v:.6} deg")));
    }
    if let Some(v) = ov_lower {
        changes.insert(format!("{p}_lower_ang"), json!(format!("{v:.6} deg")));
    }
    if let Some(v) = ov_rad_clr
        && meta.apply_clearance
    {
        changes.insert(format!("{p}_radial_clr"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_ax_clr
        && meta.apply_clearance
    {
        changes.insert(format!("{p}_axial_clr"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_crest_flat {
        changes.insert(format!("{p}_crest_flat"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_root_flat {
        changes.insert(format!("{p}_root_flat"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_crest_fil
        && meta.has_crest_fillet
    {
        changes.insert(format!("{p}_crest_fillet"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_root_fil
        && meta.has_root_fillet
    {
        changes.insert(format!("{p}_root_fillet"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_nut_wall {
        changes.insert(format!("{p}_nut_wall"), json!(format!("{v:.6} mm")));
    }
    if let Some(v) = ov_spacing {
        changes.insert(format!("{p}_spacing"), json!(format!("{v:.6} mm")));
    }

    // SMLib offset_faces rejects 0 mm; suppress OffsetFace BEFORE dropping radial_clr to 0,
    // and unsuppress AFTER restoring radial_clr > 0.
    let new_rad_clr = merged.radial_clearance;
    if let Some(ref off_name) = meta.offset_feature
        && old_rad_clr > 1e-6
        && new_rad_clr <= 1e-6
    {
        r.run(
            "timeline.suppress",
            json!({"features": [off_name], "suppressed": true}),
        )
        .map_err(|e| PluginAppError::command("timeline.suppress", e))?;
    }

    if !changes.is_empty() {
        let reply = r
            .run("parameters.change", json!({"changes": changes}))
            .map_err(|e| PluginAppError::command("parameters.change", e))?;
        check_param_change_errors(&reply)?;
    }

    if let Some(ref off_name) = meta.offset_feature
        && old_rad_clr <= 1e-6
        && new_rad_clr > 1e-6
    {
        r.run(
            "timeline.suppress",
            json!({"features": [off_name], "suppressed": false}),
        )
        .map_err(|e| PluginAppError::command("timeline.suppress", e))?;
    }

    let mut meta_changed = false;
    if let Some(lh) = new_left_hand
        && lh != meta.left_hand
    {
        for coil in &meta.coils {
            r.run(
                "timeline.edit",
                json!({"feature": coil, "set": {"clockwise": lh}}),
            )
            .map_err(|e| PluginAppError::command("timeline.edit", e))?;
        }
        meta.left_hand = lh;
        meta_changed = true;
    }

    if let Some(ec) = new_end_chamfer {
        let suppressed = ec == EndChamfer::None;
        let ec_str = format_end_chamfer(ec).to_string();
        if !meta.chamfers.is_empty() && ec_str != meta.end_chamfer {
            r.run(
                "timeline.suppress",
                json!({"features": meta.chamfers, "suppressed": suppressed}),
            )
            .map_err(|e| PluginAppError::command("timeline.suppress", e))?;
            meta.end_chamfer = ec_str;
            meta_changed = true;
        }
    }

    if meta_changed {
        let updated_text = serde_json::to_string(&meta)
            .map_err(|e| PluginAppError::command("parameters.change", format!("{e}")))?;
        let reply = r
            .run(
                "parameters.change",
                json!({
                    "name": format!("{}_meta", meta.prefix),
                    "expression": updated_text,
                }),
            )
            .map_err(|e| PluginAppError::command("parameters.change", e))?;
        check_param_change_errors(&reply)?;
    }

    Ok(json!({
        "param_prefix": meta.prefix,
        "bodies": meta.bodies,
        "major_diameter": merged.major_diameter,
        "minor_diameter": merged.major_diameter - 2.0 * new_depth,
        "pitch": merged.pitch,
        "length": merged.length,
        "starts": merged.starts,
        "lead": merged.pitch * (merged.starts as f64),
        "left_hand": meta.left_hand,
        "end_chamfer": meta.end_chamfer,
        "thread_depth": new_depth,
        "radial_clearance": merged.radial_clearance,
        "axial_clearance": merged.axial_clearance,
    }))
}

pub fn execute_entry<R: Runner>(
    r: &mut R,
    entry: &str,
    params: &Value,
) -> Result<Value, PluginAppError> {
    match entry {
        "apply-thread" | "apply_thread" => apply_thread(r, params),
        "create-thread-pair" | "create_thread_pair" => create_thread_pair(r, params),
        "edit-thread" | "edit_thread" => edit_thread(r, params),
        other => Err(PluginAppError::input(
            "entry",
            format!("unknown plugin entry '{other}'"),
        )),
    }
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({ path: "wit", world: "command-plugin" });

    use super::*;
    use craft::plugin::{
        commands as host_commands, log,
        types::{CommandFailure, FieldFailure, PluginError},
    };

    struct Host;

    impl Runner for Host {
        fn run(&mut self, command: &str, args: Value) -> Result<Value, String> {
            match host_commands::run(command, &args.to_string()) {
                Ok(s) => Ok(serde_json::from_str(&s).unwrap_or(Value::Null)),
                Err(PluginError::CommandFailed(f)) => Err(format!("{}: {}", f.command, f.reason)),
                Err(e) => Err(format!("{command}: {e:?}")),
            }
        }

        fn log(&mut self, message: &str) {
            log::log(log::Level::Info, message);
        }
    }

    fn to_plugin_error(e: PluginAppError) -> PluginError {
        match e {
            PluginAppError::Input { field, reason } => {
                PluginError::InvalidInput(FieldFailure { field, reason })
            }
            PluginAppError::Command { command, reason } => {
                PluginError::CommandFailed(CommandFailure { command, reason })
            }
        }
    }

    struct CustomThreads;

    impl exports::craft::plugin::entry::Guest for CustomThreads {
        fn execute(entry: String, params: String) -> Result<String, PluginError> {
            let v: Value = serde_json::from_str(&params).map_err(|e| {
                PluginError::InvalidInput(FieldFailure {
                    field: "params".into(),
                    reason: format!("invalid JSON parameters: {e}"),
                })
            })?;
            let result = execute_entry(&mut Host, &entry, &v).map_err(to_plugin_error)?;
            Ok(result.to_string())
        }
    }

    export!(CustomThreads);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRunner {
        selection: Value,
        faces: Value,
        edges: Value,
        mesh: Value,
        doc: Value,
        parameters: Vec<Value>,
        next_group_id: u64,
        next_offset_id: u64,
        calls: Vec<(String, Value)>,
    }

    fn parse_fake_num(expr: &str) -> Option<f64> {
        let trimmed = expr
            .trim()
            .trim_end_matches("mm")
            .trim_end_matches("deg")
            .trim();
        trimmed.parse::<f64>().ok()
    }

    impl FakeRunner {
        fn external_shaft() -> Self {
            Self {
                selection: json!({
                    "count": 1,
                    "items": [{
                        "kind": "face",
                        "type": "face",
                        "body": "ShaftBody",
                        "index": 0,
                        "point": [10.0, 0.0, 18.0]
                    }]
                }),
                faces: json!({
                    "body": "ShaftBody",
                    "faces": [
                        {
                            "index": 0,
                            "name": "F1:side",
                            "point": [10.0, 0.0, 10.0],
                            "cylinder": {"origin": [0.0, 0.0, 0.0], "axis": [0.0, 0.0, 1.0], "radius": 10.0}
                        },
                        {
                            "index": 1,
                            "name": "F1:top",
                            "point": [0.0, 0.0, 20.0],
                            "plane_normal": [0.0, 0.0, 1.0]
                        },
                        {
                            "index": 2,
                            "name": "F1:bottom",
                            "point": [0.0, 0.0, 0.0],
                            "plane_normal": [0.0, 0.0, -1.0]
                        }
                    ]
                }),
                edges: json!({
                    "edges": [
                        {"name": "F1:top|F1:side", "start": [10.0, 0.0, 20.0], "mid": [-10.0, 0.0, 20.0], "end": [10.0, 0.0, 20.0]},
                        {"name": "F1:bottom|F1:side", "start": [10.0, 0.0, 0.0], "mid": [-10.0, 0.0, 0.0], "end": [10.0, 0.0, 0.0]}
                    ]
                }),
                mesh: json!({
                    "positions": [[10.0, 0.0, 0.0], [10.0 * 0.2_f64.cos(), 10.0 * 0.2_f64.sin(), 0.0], [10.0, 0.0, 1.0]],
                    "triangles": [[0, 1, 2]]
                }),
                doc: json!({"timeline": [{"id": 1, "name": "Shaft"}], "bodies": ["ShaftBody"]}),
                parameters: Vec::new(),
                next_group_id: 10,
                next_offset_id: 1,
                calls: Vec::new(),
            }
        }

        fn internal_bore_with_blind_bottom() -> Self {
            Self {
                selection: json!({"count": 0, "items": []}),
                faces: json!({
                    "body": "NutBlock",
                    "faces": [
                        {
                            "index": 6,
                            "name": "F2:side",
                            "point": [8.9, 0.0, 12.0],
                            "cylinder": {"origin": [0.0, 0.0, 0.0], "axis": [0.0, 0.0, 1.0], "radius": 8.9}
                        },
                        {
                            "index": 0,
                            "name": "F1:+z",
                            "point": [12.0, 0.0, 20.0],
                            "plane_normal": [0.0, 0.0, 1.0]
                        },
                        {
                            "index": 7,
                            "name": "F2:bottom",
                            "point": [0.0, 0.0, 4.0],
                            "plane_normal": [0.0, 0.0, 1.0]
                        }
                    ]
                }),
                edges: json!({
                    "edges": [
                        {"name": "F1:+z|F2:side", "start": [8.9, 0.0, 20.0], "mid": [-8.9, 0.0, 20.0], "end": [8.9, 0.0, 20.0]},
                        {"name": "F2:bottom|F2:side", "start": [8.9, 0.0, 4.0], "mid": [-8.9, 0.0, 4.0], "end": [8.9, 0.0, 4.0]}
                    ]
                }),
                mesh: json!({
                    "positions": [[8.9, 0.0, 4.0], [8.9, 0.0, 5.0], [8.9 * 0.2_f64.cos(), 8.9 * 0.2_f64.sin(), 4.0]],
                    "triangles": [[0, 1, 2]]
                }),
                doc: json!({"timeline": [{"id": 1, "name": "Block"}], "bodies": ["NutBlock"]}),
                parameters: Vec::new(),
                next_group_id: 10,
                next_offset_id: 1,
                calls: Vec::new(),
            }
        }

        fn called_commands(&self) -> Vec<&str> {
            self.calls.iter().map(|(c, _)| c.as_str()).collect()
        }
    }

    impl Runner for FakeRunner {
        fn run(&mut self, command: &str, args: Value) -> Result<Value, String> {
            self.calls.push((command.to_string(), args.clone()));
            Ok(match command {
                "selection.get" => self.selection.clone(),
                "model.faces" => self.faces.clone(),
                "model.edges" => self.edges.clone(),
                "model.mesh" => self.mesh.clone(),
                "document.inspect" => self.doc.clone(),
                "parameters.add" => {
                    let name = args["name"].as_str().unwrap().to_string();
                    let expr = args["expression"].as_str().unwrap_or("").to_string();
                    let unit = args["unit"].as_str().unwrap_or("").to_string();
                    let fav = args["favorite"].as_bool().unwrap_or(false);
                    let comment = args["comment"].as_str().unwrap_or("").to_string();
                    let mut entry = json!({
                        "name": name,
                        "expression": expr,
                        "unit": unit,
                        "favorite": fav,
                        "comment": comment,
                        "source": "user",
                    });
                    if unit == "text" {
                        entry["text"] = json!(expr);
                        entry["value"] = Value::Null;
                    } else if let Some(v) = parse_fake_num(&expr) {
                        entry["value"] = json!(v);
                        entry["display_value"] = json!(v);
                    }
                    self.parameters.push(entry);
                    json!({"name": name})
                }
                "parameters.change" => {
                    if let Some(changes) = args.get("changes").and_then(Value::as_object) {
                        for (k, v) in changes {
                            if let Some(p) = self
                                .parameters
                                .iter_mut()
                                .find(|p| p.get("name").and_then(Value::as_str) == Some(k))
                            {
                                let expr = v.as_str().unwrap_or("").to_string();
                                p["expression"] = json!(expr);
                                if let Some(num) = parse_fake_num(&expr) {
                                    p["value"] = json!(num);
                                    p["display_value"] = json!(num);
                                }
                            }
                        }
                    } else if let Some(name) = args.get("name").and_then(Value::as_str) {
                        if let Some(p) = self
                            .parameters
                            .iter_mut()
                            .find(|p| p.get("name").and_then(Value::as_str) == Some(name))
                        {
                            let expr = args["expression"].as_str().unwrap_or("").to_string();
                            p["expression"] = json!(expr);
                            if p["unit"] == "text" {
                                p["text"] = json!(expr);
                            } else if let Some(num) = parse_fake_num(&expr) {
                                p["value"] = json!(num);
                                p["display_value"] = json!(num);
                            }
                        }
                    }
                    json!({"errors": []})
                }
                "parameters.list" => {
                    let fav_only = args.get("favorites").and_then(Value::as_bool).unwrap_or(false)
                        || args.get("filter").and_then(Value::as_str) == Some("favorites");
                    let list: Vec<Value> = if fav_only {
                        self.parameters
                            .iter()
                            .filter(|p| p["favorite"].as_bool() == Some(true))
                            .cloned()
                            .collect()
                    } else {
                        self.parameters.clone()
                    };
                    json!({"count": list.len(), "parameters": list})
                }
                "solid.offset_face" => {
                    let idx = self.next_offset_id;
                    self.next_offset_id += 1;
                    let name = format!("OffsetFace{idx}");
                    json!({"feature": idx + 50, "name": name})
                }
                "timeline.group" => {
                    let gid = self.next_group_id;
                    self.next_group_id += 1;
                    json!({"group": gid, "name": args["name"]})
                }
                _ => json!({}),
            })
        }
    }

    #[test]
    fn create_thread_pair_runs_and_creates_nested_folders_and_ordered_favorites() {
        let mut r = FakeRunner::external_shaft();
        r.doc = json!({"timeline": [{"name": "BoltBlank"}], "bodies": ["Bolt", "Nut"]});
        let out = execute_entry(&mut r, "create_thread_pair", &json!({})).unwrap();
        assert_eq!(out["bodies"], json!(["Bolt_2", "Nut_2"]));
        assert_eq!(out["param_prefix"], "t1");
        assert!(r.called_commands().contains(&"solid.coil"));
        assert!(r.called_commands().contains(&"browser.visibility"));
        assert!(r.called_commands().contains(&"solid.pattern.circular"));

        let groups: Vec<&Value> = r
            .calls
            .iter()
            .filter(|(c, _)| c == "timeline.group")
            .map(|(_, a)| a)
            .collect();
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0]["name"], "Bolt t1");
        assert_eq!(groups[1]["name"], "Nut t1");
        assert_eq!(groups[2]["name"], "Thread Pair t1");
        assert_eq!(groups[2]["groups"], json!([10, 11]));

        let fav_names: Vec<&str> = r
            .parameters
            .iter()
            .filter(|p| p["favorite"].as_bool() == Some(true))
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            fav_names,
            vec![
                "t1_major_d",
                "t1_pitch",
                "t1_length",
                "t1_starts",
                "t1_upper_ang",
                "t1_lower_ang",
                "t1_radial_clr",
                "t1_axial_clr",
                "t1_crest_flat",
                "t1_root_flat",
                "t1_nut_wall",
                "t1_spacing"
            ]
        );
        // Helpers stay out of Favorites.
        let hidden_names: Vec<&str> = r
            .parameters
            .iter()
            .filter(|p| p["favorite"].as_bool() == Some(false))
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert!(hidden_names.contains(&"t1_depth"));
        assert!(hidden_names.contains(&"t1_chamfer"));
        assert!(hidden_names.contains(&"t1_meta"));

        // Second pair allocates `t2`, and rejects duplicate custom prefix `t1`.
        let err = execute_entry(&mut r, "create-thread-pair", &json!({"param_prefix": "t1"}))
            .unwrap_err();
        assert!(matches!(err, PluginAppError::Input { field, .. } if field == "param_prefix"));
        let out2 = execute_entry(&mut r, "create-thread-pair", &json!({})).unwrap();
        assert_eq!(out2["param_prefix"], "t2");
    }

    #[test]
    fn apply_thread_handles_external_shaft_and_partial_length() {
        let mut r = FakeRunner::external_shaft();
        let out = execute_entry(
            &mut r,
            "apply_thread",
            &json!({"length": 12.0, "starts": 2, "pitch": 2.5}),
        )
        .unwrap();
        assert_eq!(out["kind"], "external");
        assert_eq!(out["major_diameter"], 20.0);
        assert_eq!(out["param_prefix"], "t1");
        assert!(!r.called_commands().contains(&"solid.offset_face"));
        assert!(r.called_commands().contains(&"solid.pattern.circular"));

        let grp = r
            .calls
            .iter()
            .find(|(c, _)| c == "timeline.group")
            .unwrap();
        assert_eq!(grp.1["name"], "Custom Thread t1");
    }

    #[test]
    fn apply_thread_handles_internal_blind_hole_with_clearance() {
        let mut r = FakeRunner::internal_bore_with_blind_bottom();
        let out = execute_entry(
            &mut r,
            "apply_thread",
            &json!({
                "face": [{"type": "face", "body": "NutBlock", "index": 6, "point": [8.9, 0.0, 12.0]}],
                "radial_clearance": 0.25,
                "axial_clearance": 0.1
            }),
        )
        .unwrap();
        assert_eq!(out["kind"], "internal");
        assert_eq!(out["radial_clearance"], 0.25);
        assert!(r.called_commands().contains(&"solid.offset_face"));
        let offset_call = r
            .calls
            .iter()
            .find(|(c, _)| c == "solid.offset_face")
            .unwrap();
        assert_eq!(offset_call.1["distance"], "-t1_radial_clr");

        let grp = r
            .calls
            .iter()
            .find(|(c, _)| c == "timeline.group")
            .unwrap();
        assert_eq!(grp.1["name"], "Custom Thread t1");
        assert_eq!(grp.1["features"][0], "OffsetFace1");

        // Bottom cap is blocked (`plane_normal == +Z`), so only top free cap is chamfered.
        let chamfer_lines = r
            .calls
            .iter()
            .filter(|(c, _)| c == "sketch.line")
            .skip(1)
            .count();
        assert_eq!(chamfer_lines, 1);
    }

    #[test]
    fn apply_thread_avoids_colliding_feature_prefixes() {
        let mut r = FakeRunner::internal_bore_with_blind_bottom();
        r.doc = json!({
            "timeline": [
                {"name": "CustomThread_Profile"},
                {"name": "CustomThread_Coil"}
            ],
            "bodies": ["NutBlock"]
        });
        execute_entry(
            &mut r,
            "apply-thread",
            &json!({"face": [{"type": "face", "body": "NutBlock", "index": 6}]}),
        )
        .unwrap();
        let sketch_names: Vec<&str> = r
            .calls
            .iter()
            .filter(|(c, _)| c == "sketch.create")
            .filter_map(|(_, a)| a["name"].as_str())
            .collect();
        assert!(sketch_names.contains(&"CustomThread_2_Profile"));
        assert!(sketch_names.contains(&"CustomThread_2_ChamferSketch"));
    }

    #[test]
    fn edit_thread_updates_parameters_coil_chamfers_and_validates() {
        let mut r = FakeRunner::external_shaft();
        execute_entry(&mut r, "create-thread-pair", &json!({})).unwrap();
        execute_entry(
            &mut r,
            "apply-thread",
            &json!({"param_prefix": "shaft", "apply_clearance_to_external": true}),
        )
        .unwrap();

        r.calls.clear();
        let edited = execute_entry(
            &mut r,
            "edit-thread",
            &json!({
                "target": [{"type": "body", "name": "Bolt"}],
                "major_diameter": 24.0,
                "pitch": 4.0,
                "starts": 2,
                "left_hand": "left",
                "end_chamfer": "none"
            }),
        )
        .unwrap();
        assert_eq!(edited["param_prefix"], "t1");
        assert_eq!(edited["major_diameter"], 24.0);
        assert_eq!(edited["starts"], 2);
        assert_eq!(edited["left_hand"], true);
        assert_eq!(edited["end_chamfer"], "none");

        assert!(r.called_commands().contains(&"parameters.change"));
        let edit_calls: Vec<&Value> = r
            .calls
            .iter()
            .filter(|(c, _)| c == "timeline.edit")
            .map(|(_, a)| a)
            .collect();
        assert_eq!(edit_calls.len(), 2);
        assert_eq!(edit_calls[0]["set"]["clockwise"], true);
        let sup = r
            .calls
            .iter()
            .find(|(c, _)| c == "timeline.suppress")
            .unwrap();
        assert_eq!(sup.1["suppressed"], true);

        // Editing shaft clearance to 0 suppresses OffsetFace1 BEFORE changing parameter to 0 mm.
        r.calls.clear();
        execute_entry(
            &mut r,
            "edit-thread",
            &json!({"param_prefix": "shaft", "radial_clearance": 0.0}),
        )
        .unwrap();
        let sup_idx = r
            .calls
            .iter()
            .position(|(c, a)| c == "timeline.suppress" && a["suppressed"] == true)
            .unwrap();
        let chg_idx = r
            .calls
            .iter()
            .position(|(c, a)| c == "parameters.change" && a.get("changes").is_some())
            .unwrap();
        assert!(sup_idx < chg_idx);

        // Invalid edit is rejected cleanly before mutating parameters.
        r.calls.clear();
        let err = execute_entry(
            &mut r,
            "edit-thread",
            &json!({"param_prefix": "t1", "pitch": 0.5}),
        )
        .unwrap_err();
        assert!(matches!(err, PluginAppError::Input { field, .. } if field == "pitch"));
        assert!(!r.called_commands().contains(&"parameters.change"));

        // Adding fillet to unfilleted thread fails cleanly.
        let err_fil = execute_entry(
            &mut r,
            "edit-thread",
            &json!({"param_prefix": "t1", "crest_fillet": 0.1}),
        )
        .unwrap_err();
        assert!(matches!(err_fil, PluginAppError::Input { field, .. } if field == "crest_fillet"));
    }

    #[test]
    fn manifest_defaults_and_commands_match_implementation() {
        let m: Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(m["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(m["capabilities"], json!(["commands@0.1"]));
        assert_eq!(m["runtime"]["permissions"]["command_groups"], json!([]));

        let allowed: Vec<&str> = m["runtime"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut seen_icons = std::collections::HashSet::new();
        let mut seen_slots = std::collections::HashSet::new();

        for entry in m["runtime"]["entries"].as_array().unwrap() {
            let name = entry["name"].as_str().unwrap();
            let icon = entry["input_schema"]["x-solidcraft-icon"]
                .as_str()
                .expect("entry must declare x-solidcraft-icon");
            assert!(!icon.is_empty(), "entry '{name}' has empty x-solidcraft-icon");
            assert!(
                manifest_dir.join(icon).is_file(),
                "entry '{name}' icon file '{icon}' does not exist"
            );
            assert!(
                seen_icons.insert(icon.to_string()),
                "duplicate icon '{icon}' across plugin entries"
            );

            let slots = entry["slots"].as_array().expect("entry must declare slots");
            assert!(!slots.is_empty(), "entry '{name}' must declare at least one slot");
            for slot in slots {
                let slot_name = slot["name"].as_str().expect("slot must have name");
                assert!(!slot_name.is_empty(), "entry '{name}' has empty slot name");
                assert!(
                    seen_slots.insert(slot_name.to_string()),
                    "duplicate slot label '{slot_name}' across plugin entries"
                );
            }

            let props = entry["input_schema"]["properties"].as_object().unwrap();
            let mut defaults = serde_json::Map::new();
            for (k, prop) in props {
                if let Some(d) = prop.get("default") {
                    defaults.insert(k.clone(), d.clone());
                }
            }
            let mut r = FakeRunner::external_shaft();
            if name == "apply-thread" {
                defaults.insert("starts".into(), json!(2));
                defaults.insert("crest_fillet".into(), json!(0.1));
                defaults.insert("root_fillet".into(), json!(0.1));
                defaults.insert("apply_clearance_to_external".into(), json!(true));
            } else if name == "edit-thread" {
                execute_entry(
                    &mut r,
                    "create-thread-pair",
                    &json!({"crest_fillet": 0.1, "root_fillet": 0.1}),
                )
                .unwrap();
                defaults.insert("pitch".into(), json!(3.5));
                defaults.insert("starts".into(), json!(2));
                defaults.insert("left_hand".into(), json!("left"));
                defaults.insert("end_chamfer".into(), json!("none"));
                r.calls.clear();
            }
            execute_entry(&mut r, name, &Value::Object(defaults)).unwrap();
            for c in r.called_commands() {
                assert!(
                    allowed.contains(&c),
                    "entry '{name}' called '{c}' which is missing from plugin.json runtime.commands"
                );
            }
        }
    }
}

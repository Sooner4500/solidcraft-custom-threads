pub mod commands;
pub mod fdm;
pub mod math;
pub mod presets;

use commands::{
    EndCaps, EngineCommand, PairMode, PairParams, ParametricAnchor, ParametricRadialSource,
    ParametricThreadSpec, ThreadMeta, allocate_param_prefix, append_parametric_thread_commands,
    build_apply_parameter_commands, build_parametric_thread_pair, inspect_target_cylinder,
    validate_param_prefix,
};
use fdm::{
    ChamferChoice, FdmOptions, FitTier, MatingTarget, format_folder_name, resolve_custom_profile,
    resolve_preset_profile,
};
use math::{
    ChamferSize, EndChamfer, ThreadKind, ThreadProfileParams, ValidationError, chamfer_expression,
    dot3, parse_vec3, validate_and_depth,
};
use presets::{resolve_preset_by_id, validate_preset_face_match};
use serde_json::{Value, json};
use std::collections::HashMap;

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

#[derive(Clone, Debug)]
enum ResolvedTarget {
    PickedFace {
        body: String,
        index: Option<usize>,
        point: Option<[f64; 3]>,
        forced_kind: Option<ThreadKind>,
    },
    Standalone(PairMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputChoice {
    Auto,
    External,
    Internal,
    Pair,
    Bolt,
    Nut,
}

impl OutputChoice {
    fn parse(raw: Option<&str>) -> Result<Self, PluginAppError> {
        match raw.unwrap_or("auto") {
            "auto" => Ok(Self::Auto),
            "external" => Ok(Self::External),
            "internal" => Ok(Self::Internal),
            "pair" => Ok(Self::Pair),
            "bolt" => Ok(Self::Bolt),
            "nut" => Ok(Self::Nut),
            other => Err(PluginAppError::input(
                "opt_output",
                format!("Unsupported output mode '{other}'"),
            )),
        }
    }
}

fn parse_face_item(item: &Value) -> Result<(String, Option<usize>, Option<[f64; 3]>), PluginAppError> {
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
    Ok((body, index, point))
}

fn resolve_target_mode<R: Runner>(
    r: &mut R,
    params: &Value,
    output: OutputChoice,
) -> Result<ResolvedTarget, PluginAppError> {
    if let Some(arr) = params.get("face").and_then(Value::as_array)
        && !arr.is_empty()
    {
        if matches!(output, OutputChoice::Pair | OutputChoice::Bolt | OutputChoice::Nut) {
            return Err(PluginAppError::input(
                "opt_output",
                "Clear Cylindrical Face (or set Output to Auto / External / Internal) to thread the picked face.",
            ));
        }
        if arr.len() > 1 {
            return Err(PluginAppError::input(
                "face",
                "Select only one cylindrical face at a time.",
            ));
        }
        let (body, index, point) = parse_face_item(&arr[0])?;
        let forced_kind = match output {
            OutputChoice::External => Some(ThreadKind::External),
            OutputChoice::Internal => Some(ThreadKind::Internal),
            _ => None,
        };
        return Ok(ResolvedTarget::PickedFace {
            body,
            index,
            point,
            forced_kind,
        });
    }

    match output {
        OutputChoice::Pair => return Ok(ResolvedTarget::Standalone(PairMode::Pair)),
        OutputChoice::Bolt => return Ok(ResolvedTarget::Standalone(PairMode::Bolt)),
        OutputChoice::Nut => return Ok(ResolvedTarget::Standalone(PairMode::Nut)),
        _ => {}
    }

    // Check current viewport selection for a single pre-selected face.
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

    if faces.len() > 1 {
        return Err(PluginAppError::input(
            "face",
            "Select only one cylindrical face at a time.",
        ));
    }
    if faces.len() == 1 {
        let (body, index, point) = parse_face_item(faces[0])?;
        let forced_kind = match output {
            OutputChoice::External => Some(ThreadKind::External),
            OutputChoice::Internal => Some(ThreadKind::Internal),
            OutputChoice::Auto => None,
            _ => None,
        };
        return Ok(ResolvedTarget::PickedFace {
            body,
            index,
            point,
            forced_kind,
        });
    }

    match output {
        OutputChoice::External | OutputChoice::Internal => Err(PluginAppError::input(
            "face",
            "Pick a cylindrical shaft or hole face in the viewport (or switch Output to Bolt+Nut / Bolt / Nut).",
        )),
        OutputChoice::Auto => Ok(ResolvedTarget::Standalone(PairMode::Pair)),
        _ => unreachable!(),
    }
}

fn format_end_chamfer(ec: EndChamfer) -> &'static str {
    match ec {
        EndChamfer::Both => "both",
        EndChamfer::Start => "start",
        EndChamfer::None => "none",
    }
}

fn format_chamfer_size(sz: ChamferSize) -> &'static str {
    match sz {
        ChamferSize::Regular => "regular",
        ChamferSize::Wide => "wide",
        ChamferSize::Custom(_) => "custom",
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
                let r = e
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("recompute failed");
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

fn compute_expected_offset(
    offset_mode: Option<&str>,
    apply_clearance: bool,
    base_r: f64,
    major_d: f64,
    depth: f64,
    rad_clr: f64,
) -> f64 {
    let eff_clr = if apply_clearance { rad_clr } else { 0.0 };
    match offset_mode {
        Some("external_major") => (major_d * 0.5 - eff_clr) - base_r,
        Some("internal_nominal") => base_r - (major_d * 0.5 - depth + eff_clr),
        None => {
            if apply_clearance {
                -rad_clr
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

struct ThreadExecutionSpec {
    label: String,
    profile: ThreadProfileParams,
    mating: MatingTarget,
    nut_wall_thickness: f64,
    pair_spacing: f64,
    face_span_limit: Option<f64>,
}

fn run_standalone<R: Runner>(
    r: &mut R,
    generate: PairMode,
    spec: ThreadExecutionSpec,
    requested_prefix: Option<&str>,
) -> Result<Value, PluginAppError> {
    let apply_bolt_clearance =
        generate == PairMode::Bolt && spec.mating == MatingTarget::Metal;
    let apply_clearance = match generate {
        PairMode::Pair | PairMode::Nut => true,
        PairMode::Bolt => apply_bolt_clearance,
    };

    let pair_params = PairParams {
        profile: spec.profile.clone(),
        generate,
        nut_wall_thickness: spec.nut_wall_thickness,
        pair_spacing: spec.pair_spacing,
        apply_bolt_clearance,
        coaxial_overlay: false,
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

    let folder_name = format_folder_name(&spec.label, &prefix, &spec.profile, apply_clearance);
    let parent_group_id = if group_ids.len() > 1 {
        let g = r
            .run(
                "timeline.group",
                json!({
                    "name": folder_name,
                    "groups": group_ids,
                }),
            )
            .map_err(|e| PluginAppError::command("timeline.group", e))?;
        g.get("group").and_then(Value::as_u64)
    } else if let Some(single_gid) = group_ids.first().and_then(Value::as_u64) {
        let _ = r.run(
            "timeline.rename_group",
            json!({"group": single_gid, "name": folder_name}),
        );
        Some(single_gid)
    } else {
        None
    };

    let h_nom = validate_and_depth(&spec.profile)?;
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
        apply_clearance,
        has_crest_fillet: spec.profile.crest_fillet > 1e-6,
        has_root_fillet: spec.profile.root_fillet > 1e-6,
        has_major_d: true,
        has_nut_wall: matches!(generate, PairMode::Pair | PairMode::Nut),
        has_spacing: generate == PairMode::Pair,
        left_hand: spec.profile.left_hand,
        end_chamfer: format_end_chamfer(spec.profile.end_chamfer).to_string(),
        face_span: None,
        label: Some(spec.label.clone()),
        chamfer_size: Some(format_chamfer_size(spec.profile.chamfer_size).to_string()),
        offset_mode: None,
        base_radius: None,
        major_is_favorite: true,
    };
    store_thread_meta(r, &meta)?;

    Ok(json!({
        "bodies": build.bodies,
        "param_prefix": prefix,
        "folder_name": folder_name,
        "group_id": parent_group_id,
        "major_diameter": spec.profile.major_diameter,
        "minor_diameter": spec.profile.major_diameter - 2.0 * h_nom,
        "pitch": spec.profile.pitch,
        "lead": spec.profile.pitch * (spec.profile.starts as f64),
        "thread_depth": h_nom,
        "radial_clearance": if apply_clearance { spec.profile.radial_clearance } else { 0.0 },
        "axial_clearance": if apply_clearance { spec.profile.axial_clearance } else { 0.0 },
    }))
}

struct PickedCylinderInfo {
    body: String,
    face_name: String,
    frame: math::AxisFrame,
    radius: f64,
    z_min: f64,
    z_max: f64,
    end_caps: EndCaps,
    kind: ThreadKind,
    picked_point: Option<[f64; 3]>,
}

fn inspect_picked_face<R: Runner>(
    r: &mut R,
    body: String,
    picked_index: Option<usize>,
    picked_point: Option<[f64; 3]>,
    forced_kind: Option<ThreadKind>,
) -> Result<PickedCylinderInfo, PluginAppError> {
    let faces_reply = r
        .run("model.faces", json!({"body": body}))
        .map_err(|e| PluginAppError::command("model.faces", e))?;
    let edges_reply = r
        .run("model.edges", json!({"body": body}))
        .map_err(|e| PluginAppError::command("model.edges", e))?;

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
                    "opt_output",
                    "Could not auto-detect external shaft vs. internal hole; set Output to External or Internal explicitly.",
                )
            })?
        }
    };

    Ok(PickedCylinderInfo {
        body,
        face_name,
        frame,
        radius,
        z_min,
        z_max,
        end_caps,
        kind,
        picked_point,
    })
}

fn compute_span_on_face(
    cyl: &PickedCylinderInfo,
    raw_length: Option<f64>,
) -> Result<(f64, f64, EndCaps, ParametricAnchor, f64), PluginAppError> {
    let face_span = cyl.z_max - cyl.z_min;
    match raw_length {
        Some(len) if len <= 0.0 => Err(PluginAppError::input(
            "length",
            "Thread length must be > 0 mm (or leave blank for full face)",
        )),
        Some(len) if len < face_span - 1e-4 => {
            if cyl.end_caps.start_free && !cyl.end_caps.end_free {
                Ok((
                    cyl.z_min,
                    cyl.z_min + len,
                    EndCaps {
                        start_free: true,
                        end_free: false,
                    },
                    ParametricAnchor::StartFixed(cyl.z_min),
                    face_span,
                ))
            } else if cyl.end_caps.end_free && !cyl.end_caps.start_free {
                Ok((
                    cyl.z_max - len,
                    cyl.z_max,
                    EndCaps {
                        start_free: false,
                        end_free: true,
                    },
                    ParametricAnchor::EndFixed(cyl.z_max),
                    face_span,
                ))
            } else if let Some(pt) = cyl.picked_point {
                let d = [
                    pt[0] - cyl.frame.origin[0],
                    pt[1] - cyl.frame.origin[1],
                    pt[2] - cyl.frame.origin[2],
                ];
                let z_pick = dot3(d, cyl.frame.axis);
                if (z_pick - cyl.z_min).abs() <= (cyl.z_max - z_pick).abs() {
                    Ok((
                        cyl.z_min,
                        cyl.z_min + len,
                        EndCaps {
                            start_free: cyl.end_caps.start_free,
                            end_free: false,
                        },
                        ParametricAnchor::StartFixed(cyl.z_min),
                        face_span,
                    ))
                } else {
                    Ok((
                        cyl.z_max - len,
                        cyl.z_max,
                        EndCaps {
                            start_free: false,
                            end_free: cyl.end_caps.end_free,
                        },
                        ParametricAnchor::EndFixed(cyl.z_max),
                        face_span,
                    ))
                }
            } else {
                Ok((
                    cyl.z_min,
                    cyl.z_min + len,
                    EndCaps {
                        start_free: cyl.end_caps.start_free,
                        end_free: false,
                    },
                    ParametricAnchor::StartFixed(cyl.z_min),
                    face_span,
                ))
            }
        }
        _ => {
            let anchor = if cyl.end_caps.end_free && !cyl.end_caps.start_free {
                ParametricAnchor::EndFixed(cyl.z_max)
            } else {
                ParametricAnchor::StartFixed(cyl.z_min)
            };
            Ok((
                cyl.z_min,
                cyl.z_max,
                cyl.end_caps.clone(),
                anchor,
                face_span,
            ))
        }
    }
}

fn run_on_picked_face<R: Runner>(
    r: &mut R,
    cyl: PickedCylinderInfo,
    anchor: ParametricAnchor,
    eff_caps: EndCaps,
    face_span: f64,
    spec: ThreadExecutionSpec,
    requested_prefix: Option<&str>,
    major_d_favorite: bool,
) -> Result<Value, PluginAppError> {
    let mut prof = spec.profile;
    let h_nom = validate_and_depth(&prof)?;
    let apply_clearance = spec.mating.applies_to(cyl.kind);

    let params_list = r
        .run("parameters.list", json!({"filter": "user"}))
        .map_err(|e| PluginAppError::command("parameters.list", e))?;
    let prefix = allocate_param_prefix(requested_prefix, &params_list)?;

    let doc = r
        .run(
            "document.inspect",
            json!({"timeline": "summary", "bodies": false}),
        )
        .map_err(|e| PluginAppError::command("document.inspect", e))?;
    let (taken_features, _) = collect_taken_names(&doc);
    let tag = unique_prefix("CustomThread", &taken_features);

    let param_cmds =
        build_apply_parameter_commands(&prefix, &prof, cyl.radius, apply_clearance, major_d_favorite);
    run_commands(r, &param_cmds)?;

    let offset_mode = match cyl.kind {
        ThreadKind::External => "external_major",
        ThreadKind::Internal => "internal_nominal",
    };

    let offset_val = compute_expected_offset(
        Some(offset_mode),
        apply_clearance,
        cyl.radius,
        prof.major_diameter,
        h_nom,
        prof.radial_clearance,
    );

    let mut group_features = Vec::new();
    let mut offset_feature_name = None;

    if offset_val.abs() > 1e-6 {
        let offset_expr = match (cyl.kind, apply_clearance) {
            (ThreadKind::External, true) => {
                format!("{prefix}_major_d / 2 - {prefix}_radial_clr - {prefix}_base_r")
            }
            (ThreadKind::External, false) => format!("{prefix}_major_d / 2 - {prefix}_base_r"),
            (ThreadKind::Internal, true) => format!(
                "{prefix}_base_r - ({prefix}_major_d / 2 - {prefix}_depth + {prefix}_radial_clr)"
            ),
            (ThreadKind::Internal, false) => {
                format!("{prefix}_base_r - ({prefix}_major_d / 2 - {prefix}_depth)")
            }
        };
        let off_reply = r
            .run(
                "solid.offset_face",
                json!({
                    "faces": [cyl.face_name],
                    "body": cyl.body,
                    "distance": offset_expr,
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
            body: cyl.body.clone(),
            frame: cyl.frame,
            origin_expr: None,
            kind: cyl.kind,
            radial_source: ParametricRadialSource::MajorParam { apply_clearance },
            phase_half_pitch: cyl.kind == ThreadKind::Internal,
            anchor,
            ends: eff_caps,
            end_chamfer: prof.end_chamfer,
            left_hand: prof.left_hand,
            has_crest_fillet: prof.crest_fillet > 1e-6,
            has_root_fillet: prof.root_fillet > 1e-6,
        },
    )?;
    run_commands(r, &thread_cmds)?;
    group_features.extend(tf.features_in_order());

    let folder_name = format_folder_name(&spec.label, &prefix, &prof, apply_clearance);
    let group_reply = r
        .run(
            "timeline.group",
            json!({
                "name": folder_name,
                "features": group_features,
            }),
        )
        .map_err(|e| PluginAppError::command("timeline.group", e))?;
    let group_id = group_reply.get("group").and_then(Value::as_u64);

    let chamfers = tf.chamfer.into_iter().collect();
    let kind_str = match cyl.kind {
        ThreadKind::External => "external",
        ThreadKind::Internal => "internal",
    };
    let meta = ThreadMeta {
        v: 1,
        prefix: prefix.clone(),
        kind: kind_str.to_string(),
        bodies: vec![cyl.body.clone()],
        coils: vec![tf.coil],
        chamfers,
        offset_feature: offset_feature_name,
        group_id,
        apply_clearance,
        has_crest_fillet: prof.crest_fillet > 1e-6,
        has_root_fillet: prof.root_fillet > 1e-6,
        has_major_d: false,
        has_nut_wall: false,
        has_spacing: false,
        left_hand: prof.left_hand,
        end_chamfer: format_end_chamfer(prof.end_chamfer).to_string(),
        face_span: spec.face_span_limit.or(Some(face_span)),
        label: Some(spec.label),
        chamfer_size: Some(format_chamfer_size(prof.chamfer_size).to_string()),
        offset_mode: Some(offset_mode.to_string()),
        base_radius: Some(cyl.radius),
        major_is_favorite: major_d_favorite,
    };
    store_thread_meta(r, &meta)?;

    let clr_rad = if apply_clearance {
        prof.radial_clearance
    } else {
        0.0
    };
    let (eff_maj, eff_min) = match cyl.kind {
        ThreadKind::External => {
            let r_maj = prof.major_diameter * 0.5 - clr_rad;
            (2.0 * r_maj, 2.0 * (r_maj - h_nom))
        }
        ThreadKind::Internal => {
            let r_maj = prof.major_diameter * 0.5 + clr_rad;
            (2.0 * r_maj, 2.0 * (r_maj - h_nom))
        }
    };
    // Avoid unused mut warning cleanly while keeping prof values intact.
    prof.radial_clearance = clr_rad;

    Ok(json!({
        "body": cyl.body,
        "face": cyl.face_name,
        "kind": cyl.kind,
        "param_prefix": prefix,
        "folder_name": folder_name,
        "group_id": group_id,
        "major_diameter": eff_maj,
        "minor_diameter": eff_min,
        "pitch": prof.pitch,
        "lead": prof.pitch * (prof.starts as f64),
        "thread_depth": h_nom,
        "radial_clearance": clr_rad,
        "axial_clearance": if apply_clearance { prof.axial_clearance } else { 0.0 },
    }))
}

fn auto_nut_wall(major_d: f64) -> f64 {
    (major_d * 0.20).clamp(3.5, 10.0)
}

pub fn run_preset_thread<R: Runner>(r: &mut R, params: &Value) -> Result<Value, PluginAppError> {
    let preset_id = params
        .get("preset")
        .and_then(Value::as_str)
        .unwrap_or("ght_3_4");
    let fdm_adjust = get_bool(params, "fdm_adjust", true)?;
    let fit = FitTier::parse(params.get("fdm_fit").and_then(Value::as_str))?;
    let nozzle_mm = get_f64(params, "fdm_nozzle_mm", 0.40)?;
    let layer_mm = get_f64(params, "fdm_layer_mm", 0.20)?;
    let mating = MatingTarget::parse(
        params.get("fdm_target").and_then(Value::as_str),
        "fdm_target",
        MatingTarget::Metal,
    )?;
    let chamfer = ChamferChoice::parse(params.get("opt_chamfer").and_then(Value::as_str))?;
    let fdm = FdmOptions {
        adjust: fdm_adjust,
        fit,
        nozzle_mm,
        layer_mm,
        mating,
        chamfer,
    };

    let left_hand = get_bool(params, "opt_left_hand", false)?;
    let raw_length = opt_f64(params, "opt_length")?;
    let output = OutputChoice::parse(params.get("opt_output").and_then(Value::as_str))?;
    let target = resolve_target_mode(r, params, output)?;

    match target {
        ResolvedTarget::PickedFace {
            body,
            index,
            point,
            forced_kind,
        } => {
            let cyl = inspect_picked_face(r, body, index, point, forced_kind)?;
            let picked_d = 2.0 * cyl.radius;
            let is_internal = cyl.kind == ThreadKind::Internal;
            let preset = resolve_preset_by_id(preset_id, Some(picked_d), is_internal)?;
            if !matches!(preset_id, "auto_iso" | "auto_unc") {
                validate_preset_face_match(picked_d, &preset, is_internal)?;
            }
            let (z_start, z_end, eff_caps, anchor, face_span) =
                compute_span_on_face(&cyl, raw_length)?;
            let prof = resolve_preset_profile(
                &preset,
                Some(z_end - z_start),
                None,
                left_hand,
                &fdm,
            )?;
            let exec = ThreadExecutionSpec {
                label: preset.short_label.to_string(),
                profile: prof,
                mating,
                nut_wall_thickness: auto_nut_wall(preset.major_diameter),
                pair_spacing: 10.0,
                face_span_limit: Some(face_span),
            };
            run_on_picked_face(r, cyl, anchor, eff_caps, face_span, exec, None, true)
        }
        ResolvedTarget::Standalone(pair_mode) => {
            let preset = resolve_preset_by_id(preset_id, None, false)?;
            if let Some(len) = raw_length
                && (!len.is_finite() || len <= 0.2)
            {
                return Err(PluginAppError::input(
                    "opt_length",
                    "Length must be > 0.2 mm",
                ));
            }
            let prof = resolve_preset_profile(&preset, raw_length, None, left_hand, &fdm)?;
            let exec = ThreadExecutionSpec {
                label: preset.short_label.to_string(),
                profile: prof,
                mating,
                nut_wall_thickness: auto_nut_wall(preset.major_diameter),
                pair_spacing: 10.0,
                face_span_limit: None,
            };
            run_standalone(r, pair_mode, exec, None)
        }
    }
}

pub fn run_custom_thread<R: Runner>(r: &mut R, params: &Value) -> Result<Value, PluginAppError> {
    let pitch = get_f64(params, "pitch", 3.0)?;
    let dim_diameter = get_f64(params, "dim_diameter", 20.0)?;
    let starts = get_u32(params, "opt_starts", 1)?;
    let left_hand = get_bool(params, "opt_left_hand", false)?;
    let raw_length = opt_f64(params, "opt_length")?;
    let output = OutputChoice::parse(params.get("opt_output").and_then(Value::as_str))?;

    let fdm_adjust = get_bool(params, "fdm_adjust", true)?;
    let fit = FitTier::parse(params.get("fdm_fit").and_then(Value::as_str))?;
    let nozzle_mm = get_f64(params, "fdm_nozzle_mm", 0.40)?;
    let layer_mm = get_f64(params, "fdm_layer_mm", 0.20)?;
    let mating = MatingTarget::parse(
        params.get("fdm_target").and_then(Value::as_str),
        "fdm_target",
        MatingTarget::Printed,
    )?;
    let chamfer = ChamferChoice::parse(params.get("opt_chamfer").and_then(Value::as_str))?;
    let fdm = FdmOptions {
        adjust: fdm_adjust,
        fit,
        nozzle_mm,
        layer_mm,
        mating,
        chamfer,
    };

    // Early validation before modifying/reading geometry where standalone checks apply.
    let target = resolve_target_mode(r, params, output)?;

    match target {
        ResolvedTarget::PickedFace {
            body,
            index,
            point,
            forced_kind,
        } => {
            let cyl = inspect_picked_face(r, body, index, point, forced_kind)?;
            let major_d = 2.0 * cyl.radius;
            let (z_start, z_end, eff_caps, anchor, face_span) =
                compute_span_on_face(&cyl, raw_length)?;
            let prof = resolve_custom_profile(
                major_d,
                pitch,
                z_end - z_start,
                starts,
                left_hand,
                &fdm,
            )?;
            let label = format!("Custom Ø{major_d:.1}x{pitch:.2}");
            let exec = ThreadExecutionSpec {
                label,
                profile: prof,
                mating,
                nut_wall_thickness: auto_nut_wall(major_d),
                pair_spacing: 10.0,
                face_span_limit: Some(face_span),
            };
            run_on_picked_face(r, cyl, anchor, eff_caps, face_span, exec, None, false)
        }
        ResolvedTarget::Standalone(pair_mode) => {
            let len = match raw_length {
                Some(l) if l > 0.0 => l,
                Some(_) => {
                    return Err(PluginAppError::input(
                        "opt_length",
                        "Length must be > 0 mm",
                    ));
                }
                None => 20.0,
            };
            let prof =
                resolve_custom_profile(dim_diameter, pitch, len, starts, left_hand, &fdm)?;
            let label = format!("Custom Ø{dim_diameter:.1}x{pitch:.2}");
            let exec = ThreadExecutionSpec {
                label,
                profile: prof,
                mating,
                nut_wall_thickness: auto_nut_wall(dim_diameter),
                pair_spacing: 10.0,
                face_span_limit: None,
            };
            run_standalone(r, pair_mode, exec, None)
        }
    }
}

pub fn run_advanced_thread<R: Runner>(r: &mut R, params: &Value) -> Result<Value, PluginAppError> {
    let requested_prefix = params
        .get("opt_param_prefix")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(req) = requested_prefix {
        validate_param_prefix(req)?;
    }

    let pitch = get_f64(params, "pitch", 3.0)?;
    let dim_diameter = get_f64(params, "dim_diameter", 20.0)?;
    let raw_length = opt_f64(params, "dim_length")?;
    let starts = get_u32(params, "dim_starts", 1)?;
    let left_hand = get_bool(params, "dim_left_hand", false)?;
    let upper_deg = get_f64(params, "flank_upper_deg", 45.0)?;
    let lower_deg = get_f64(params, "flank_lower_deg", 45.0)?;
    let rad_clr = get_f64(params, "gap_radial_clr", 0.20)?;
    let ax_clr = get_f64(params, "gap_axial_clr", 0.10)?;
    let mating = MatingTarget::parse(
        params.get("gap_mating").and_then(Value::as_str),
        "gap_mating",
        MatingTarget::Printed,
    )?;
    let crest_flat = get_f64(params, "tooth_crest_flat", 0.40)?;
    let root_flat = get_f64(params, "tooth_root_flat", 0.40)?;
    let crest_fillet = get_f64(params, "tooth_crest_fillet", 0.0)?;
    let root_fillet = get_f64(params, "tooth_root_fillet", 0.0)?;
    let chamfer_choice = ChamferChoice::parse(params.get("opt_chamfer").and_then(Value::as_str))?;
    let (end_chamfer, mut chamfer_size) = chamfer_choice.to_end_and_size();
    if let Some(custom_mm) = opt_f64(params, "opt_chamfer_mm")? {
        chamfer_size = ChamferSize::Custom(custom_mm);
    }
    let nut_wall = get_f64(params, "opt_nut_wall", 5.0)?;
    let pair_spacing = get_f64(params, "opt_pair_spacing", 10.0)?;
    let output = OutputChoice::parse(params.get("opt_output").and_then(Value::as_str))?;

    let build_profile = |major_diameter: f64, length: f64| -> Result<ThreadProfileParams, PluginAppError> {
        let prof = ThreadProfileParams {
            major_diameter,
            pitch,
            length,
            starts,
            left_hand,
            upper_flank_angle_deg: upper_deg,
            lower_flank_angle_deg: lower_deg,
            radial_clearance: rad_clr,
            axial_clearance: ax_clr,
            crest_flat,
            root_flat,
            crest_fillet,
            root_fillet,
            end_chamfer,
            chamfer_size,
        };
        let _ = validate_and_depth(&prof)?;
        Ok(prof)
    };

    // Pre-validate basic profile scalars cleanly.
    let _ = build_profile(100.0, 20.0)?;
    let target = resolve_target_mode(r, params, output)?;

    match target {
        ResolvedTarget::PickedFace {
            body,
            index,
            point,
            forced_kind,
        } => {
            let cyl = inspect_picked_face(r, body, index, point, forced_kind)?;
            let major_d = 2.0 * cyl.radius;
            let (z_start, z_end, eff_caps, anchor, face_span) =
                compute_span_on_face(&cyl, raw_length)?;
            let prof = build_profile(major_d, z_end - z_start)?;
            let label = format!("Adv Ø{major_d:.1}x{pitch:.2}");
            let exec = ThreadExecutionSpec {
                label,
                profile: prof,
                mating,
                nut_wall_thickness: nut_wall,
                pair_spacing,
                face_span_limit: Some(face_span),
            };
            run_on_picked_face(
                r,
                cyl,
                anchor,
                eff_caps,
                face_span,
                exec,
                requested_prefix,
                false,
            )
        }
        ResolvedTarget::Standalone(pair_mode) => {
            let len = match raw_length {
                Some(l) if l > 0.0 => l,
                Some(_) => {
                    return Err(PluginAppError::input(
                        "dim_length",
                        "Length must be > 0 mm",
                    ));
                }
                None => 20.0,
            };
            let prof = build_profile(dim_diameter, len)?;
            let label = format!("Adv Ø{dim_diameter:.1}x{pitch:.2}");
            let exec = ThreadExecutionSpec {
                label,
                profile: prof,
                mating,
                nut_wall_thickness: nut_wall,
                pair_spacing,
                face_span_limit: None,
            };
            run_standalone(r, pair_mode, exec, requested_prefix)
        }
    }
}

fn resolve_target_meta<R: Runner>(
    r: &mut R,
    params: &Value,
    metas: &[ThreadMeta],
) -> Result<ThreadMeta, PluginAppError> {
    if metas.is_empty() {
        return Err(PluginAppError::input(
            "target",
            "No Custom Threads found in this design. Run Preset Thread, Custom Thread, or Advanced Thread first.",
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

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum EditChamferReq {
        Regular,
        Wide,
        None,
    }
    let req_chamfer = match params.get("end_chamfer").and_then(Value::as_str) {
        None | Some("keep") => None,
        Some("regular") | Some("both") => Some(EditChamferReq::Regular),
        Some("wide") => Some(EditChamferReq::Wide),
        Some("none") => Some(EditChamferReq::None),
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
    let ov_chamfer_mm = opt_f64(params, "chamfer_mm")?;
    let ov_nut_wall = opt_f64(params, "nut_wall_thickness")?;
    let ov_spacing = opt_f64(params, "pair_spacing")?;

    let params_list = r
        .run("parameters.list", json!({"filter": "user"}))
        .map_err(|e| PluginAppError::command("parameters.list", e))?;
    let (param_vals, metas) = extract_param_state(&params_list);
    let mut meta = resolve_target_meta(r, params, &metas)?;
    let p = meta.prefix.clone();

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
                "This thread was created without clearance offsets (nominal external/bolt mode); edit the mating nut instead.",
            ));
        }
        if ov_ax_clr.is_some_and(|v| v > 1e-6) {
            return Err(PluginAppError::input(
                "axial_clearance",
                "This thread was created without clearance offsets (nominal external/bolt mode); edit the mating nut instead.",
            ));
        }
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

    if let Some(ch_req) = req_chamfer
        && ch_req != EditChamferReq::None
        && meta.chamfers.is_empty()
    {
        return Err(PluginAppError::input(
            "end_chamfer",
            "This v0.2.0 thread was created with end_chamfer: none so no chamfer features exist to enable.",
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

    let old_depth = (cur_pitch - cur_crest_flat - cur_root_flat)
        / (cur_upper.to_radians().tan() + cur_lower.to_radians().tan());
    let est_new_depth = (new_pitch - new_crest_flat - new_root_flat)
        / (new_upper.to_radians().tan() + new_lower.to_radians().tan());

    let (cur_major_d, base_r_opt) = if meta.offset_mode.is_some() || meta.has_major_d {
        let md = read_num("major_d")?;
        (md, meta.base_radius)
    } else {
        let br = read_num("base_r")?;
        let md = if meta.kind == "internal" {
            2.0 * (br + est_new_depth)
        } else {
            2.0 * br
        };
        (md, Some(br))
    };

    let old_chamfer_size = match meta.chamfer_size.as_deref() {
        Some("wide") => ChamferSize::Wide,
        Some("custom") => ChamferSize::Custom(read_num("chamfer").unwrap_or(old_depth * 0.95)),
        _ => ChamferSize::Regular,
    };

    let next_chamfer_size = if let Some(mm) = ov_chamfer_mm {
        ChamferSize::Custom(mm)
    } else {
        match req_chamfer {
            Some(EditChamferReq::Regular) => ChamferSize::Regular,
            Some(EditChamferReq::Wide) => ChamferSize::Wide,
            Some(EditChamferReq::None) | None => old_chamfer_size,
        }
    };

    let effective_end_chamfer = match req_chamfer {
        Some(EditChamferReq::None) => EndChamfer::None,
        Some(EditChamferReq::Regular) | Some(EditChamferReq::Wide) => EndChamfer::Both,
        None => match meta.end_chamfer.as_str() {
            "none" => EndChamfer::None,
            "start" => EndChamfer::Start,
            _ => EndChamfer::Both,
        },
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
        end_chamfer: effective_end_chamfer,
        chamfer_size: next_chamfer_size,
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

    // Verify SMLib OffsetFace offset transitions cleanly.
    let (old_offset, new_offset) = if let Some(br) = base_r_opt {
        (
            compute_expected_offset(
                meta.offset_mode.as_deref(),
                meta.apply_clearance,
                br,
                cur_major_d,
                old_depth,
                old_rad_clr,
            ),
            compute_expected_offset(
                meta.offset_mode.as_deref(),
                meta.apply_clearance,
                br,
                merged.major_diameter,
                new_depth,
                merged.radial_clearance,
            ),
        )
    } else if meta.apply_clearance && (meta.kind == "external" || meta.kind == "internal") {
        (-old_rad_clr, -merged.radial_clearance)
    } else {
        (0.0, 0.0)
    };

    if (meta.kind == "external" || meta.kind == "internal")
        && meta.offset_feature.is_none()
        && new_offset.abs() > 1e-6
    {
        return Err(PluginAppError::input(
            "radial_clearance",
            "This thread was applied with zero initial face offset so no OffsetFace feature exists; recreate with radial_clearance > 0.",
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
    if ov_chamfer_mm.is_some()
        || matches!(req_chamfer, Some(EditChamferReq::Regular | EditChamferReq::Wide))
    {
        changes.insert(
            format!("{p}_chamfer"),
            json!(chamfer_expression(&p, next_chamfer_size, meta.has_nut_wall)),
        );
    }

    if let Some(ref off_name) = meta.offset_feature
        && old_offset.abs() > 1e-6
        && new_offset.abs() <= 1e-6
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
        && old_offset.abs() <= 1e-6
        && new_offset.abs() > 1e-6
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

    let new_size_str = format_chamfer_size(next_chamfer_size).to_string();
    if meta.chamfer_size.as_deref() != Some(new_size_str.as_str())
        && (ov_chamfer_mm.is_some()
            || matches!(req_chamfer, Some(EditChamferReq::Regular | EditChamferReq::Wide)))
    {
        meta.chamfer_size = Some(new_size_str);
        meta_changed = true;
    }

    if let Some(ch_req) = req_chamfer {
        let suppressed = ch_req == EditChamferReq::None;
        let ec_str = if suppressed { "none" } else { "both" }.to_string();
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

    let raw_label = meta
        .label
        .clone()
        .unwrap_or_else(|| format!("Custom Ø{:.1}x{:.2}", merged.major_diameter, merged.pitch));
    let display_label = if raw_label.starts_with("Custom Ø") {
        format!("Custom Ø{:.1}x{:.2}", merged.major_diameter, merged.pitch)
    } else if raw_label.starts_with("Adv Ø") {
        format!("Adv Ø{:.1}x{:.2}", merged.major_diameter, merged.pitch)
    } else if ov_major.is_some() || ov_pitch.is_some() {
        format!("{raw_label} (Ø{:.1}x{:.2})", merged.major_diameter, merged.pitch)
    } else {
        raw_label
    };
    if meta.label.as_deref() != Some(display_label.as_str()) {
        meta.label = Some(display_label.clone());
        meta_changed = true;
    }
    let updated_folder_name =
        format_folder_name(&display_label, &meta.prefix, &merged, meta.apply_clearance);

    if let Some(gid) = meta.group_id {
        let _ = r.run(
            "timeline.rename_group",
            json!({"group": gid, "name": updated_folder_name}),
        );
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
        "folder_name": updated_folder_name,
        "bodies": meta.bodies,
        "major_diameter": merged.major_diameter,
        "minor_diameter": merged.major_diameter - 2.0 * new_depth,
        "pitch": merged.pitch,
        "length": merged.length,
        "starts": merged.starts,
        "lead": merged.pitch * (merged.starts as f64),
        "left_hand": meta.left_hand,
        "end_chamfer": meta.end_chamfer,
        "chamfer_size": meta.chamfer_size,
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
        "preset-thread" | "preset_thread" => run_preset_thread(r, params),
        "custom-thread" | "custom_thread" => run_custom_thread(r, params),
        "advanced-thread" | "advanced_thread" => run_advanced_thread(r, params),
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
        fn empty_selection() -> Self {
            Self {
                selection: json!({"count": 0, "items": []}),
                faces: json!({"body": "None", "faces": []}),
                edges: json!({"edges": []}),
                mesh: json!({"positions": [], "triangles": []}),
                doc: json!({"timeline": [], "bodies": []}),
                parameters: Vec::new(),
                next_group_id: 10,
                next_offset_id: 1,
                calls: Vec::new(),
            }
        }

        fn external_shaft(radius: f64) -> Self {
            Self {
                selection: json!({
                    "count": 1,
                    "items": [{
                        "kind": "face",
                        "type": "face",
                        "body": "ShaftBody",
                        "index": 0,
                        "point": [radius, 0.0, 18.0]
                    }]
                }),
                faces: json!({
                    "body": "ShaftBody",
                    "faces": [
                        {
                            "index": 0,
                            "name": "F1:side",
                            "point": [radius, 0.0, 10.0],
                            "cylinder": {"origin": [0.0, 0.0, 0.0], "axis": [0.0, 0.0, 1.0], "radius": radius}
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
                        {"name": "F1:top|F1:side", "start": [radius, 0.0, 20.0], "mid": [-radius, 0.0, 20.0], "end": [radius, 0.0, 20.0]},
                        {"name": "F1:bottom|F1:side", "start": [radius, 0.0, 0.0], "mid": [-radius, 0.0, 0.0], "end": [radius, 0.0, 0.0]}
                    ]
                }),
                mesh: json!({
                    "positions": [[radius, 0.0, 0.0], [radius * 0.2_f64.cos(), radius * 0.2_f64.sin(), 0.0], [radius, 0.0, 1.0]],
                    "triangles": [[0, 1, 2]]
                }),
                doc: json!({"timeline": [{"id": 1, "name": "Shaft"}], "bodies": ["ShaftBody"]}),
                parameters: Vec::new(),
                next_group_id: 10,
                next_offset_id: 1,
                calls: Vec::new(),
            }
        }

        fn internal_bore(radius: f64, blind_bottom: bool) -> Self {
            Self {
                selection: json!({"count": 0, "items": []}),
                faces: json!({
                    "body": "NutBlock",
                    "faces": [
                        {
                            "index": 6,
                            "name": "F2:side",
                            "point": [radius, 0.0, 12.0],
                            "cylinder": {"origin": [0.0, 0.0, 0.0], "axis": [0.0, 0.0, 1.0], "radius": radius}
                        },
                        {
                            "index": 0,
                            "name": "F1:+z",
                            "point": [radius + 5.0, 0.0, 20.0],
                            "plane_normal": [0.0, 0.0, 1.0]
                        },
                        {
                            "index": 7,
                            "name": "F2:bottom",
                            "point": [0.0, 0.0, 4.0],
                            "plane_normal": [0.0, 0.0, if blind_bottom { 1.0 } else { -1.0 }]
                        }
                    ]
                }),
                edges: json!({
                    "edges": [
                        {"name": "F1:+z|F2:side", "start": [radius, 0.0, 20.0], "mid": [-radius, 0.0, 20.0], "end": [radius, 0.0, 20.0]},
                        {"name": "F2:bottom|F2:side", "start": [radius, 0.0, 4.0], "mid": [-radius, 0.0, 4.0], "end": [radius, 0.0, 4.0]}
                    ]
                }),
                mesh: json!({
                    "positions": [[radius, 0.0, 4.0], [radius, 0.0, 5.0], [radius * 0.2_f64.cos(), radius * 0.2_f64.sin(), 4.0]],
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
                    let list = self.parameters.clone();
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
                "timeline.rename_group" => {
                    json!({"group": args["group"], "name": args["name"]})
                }
                _ => json!({}),
            })
        }
    }

    #[test]
    fn exhaustive_fixed_preset_matrix_passes_fdm_validation() {
        let fits = [FitTier::Tight, FitTier::Regular, FitTier::Wide, FitTier::None];
        let nozzles = [0.25, 0.40, 0.60];
        let layers = [0.12, 0.20, 0.28];
        let chamfers = [
            ChamferChoice::RegularBoth,
            ChamferChoice::WideBoth,
            ChamferChoice::None,
        ];
        let mut checked = 0usize;
        for preset in presets::FIXED_PRESETS {
            for &adjust in &[true, false] {
                for &fit in &fits {
                    for &nozzle_mm in &nozzles {
                        for &layer_mm in &layers {
                            for &chamfer in &chamfers {
                                let opt = FdmOptions {
                                    adjust,
                                    fit,
                                    nozzle_mm,
                                    layer_mm,
                                    mating: MatingTarget::Metal,
                                    chamfer,
                                };
                                let prof = resolve_preset_profile(preset, None, None, false, &opt)
                                    .unwrap_or_else(|e| {
                                        panic!(
                                            "preset '{}' failed (adj={}, fit={:?}, nz={}, ly={}): {:?}",
                                            preset.id, adjust, fit, nozzle_mm, layer_mm, e
                                        )
                                    });
                                let depth = validate_and_depth(&prof).unwrap();
                                assert!(depth > 0.1, "shallow depth on {}", preset.id);
                                if adjust && fit != FitTier::None {
                                    let nom_depth = presets::nominal_depth(preset);
                                    assert!(
                                        nom_depth - depth < prof.radial_clearance,
                                        "preset '{}' metal clearance root overlap: nom={} fdm={} clr={}",
                                        preset.id,
                                        nom_depth,
                                        depth,
                                        prof.radial_clearance
                                    );
                                }
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, presets::FIXED_PRESETS.len() * 2 * 4 * 3 * 3 * 3);
    }

    #[test]
    fn auto_iso_and_unc_match_shaft_nominal_hole_and_tap_drill() {
        // M10 shaft (Ø10.0) and M10 tap drill hole (Ø8.5)
        let m10_s = presets::match_iso_coarse(10.0, false).unwrap();
        assert_eq!(m10_s.id, "m10");
        let m8_tap = presets::match_iso_coarse(6.75, true).unwrap();
        assert_eq!(m8_tap.id, "m8");
        let m8_nom_hole = presets::match_iso_coarse(8.0, true).unwrap();
        assert_eq!(m8_nom_hole.id, "m8");

        // 1/4"-20 UNC (6.35 mm shaft or ~5.1 mm tap drill)
        let unc_1_4 = presets::match_unc_coarse(6.35, false).unwrap();
        assert_eq!(unc_1_4.id, "unc_1_4");
        let unc_tap = presets::match_unc_coarse(5.1, true).unwrap();
        assert_eq!(unc_tap.id, "unc_1_4");

        // Out-of-range diameter rejected cleanly
        let err = presets::match_iso_coarse(150.0, false).unwrap_err();
        assert_eq!(err.field, "preset");

        // Extreme preset mismatch rejected cleanly
        let mason = presets::lookup_preset("mason_86g").unwrap();
        let err_mm = validate_preset_face_match(5.0, mason, true).unwrap_err();
        assert_eq!(err_mm.field, "preset");
    }

    #[test]
    fn preset_thread_standalone_and_picked_face_routing() {
        // 1. Standalone Garden Hose pair (nothing selected)
        let mut r = FakeRunner::empty_selection();
        let out = execute_entry(
            &mut r,
            "preset-thread",
            &json!({"preset": "ght_3_4", "opt_chamfer": "wide_both"}),
        )
        .unwrap();
        assert_eq!(out["bodies"], json!(["Bolt", "Nut"]));
        assert_eq!(out["param_prefix"], "t1");
        assert!(out["folder_name"].as_str().unwrap().contains("Garden Hose"));
        assert!(r.called_commands().contains(&"solid.coil"));

        // 2. Picked Ø8.0 nominal hole with M8 + Metal -> offset_face adjusts bore inward
        let mut r2 = FakeRunner::internal_bore(4.0, true);
        let out2 = execute_entry(
            &mut r2,
            "preset-thread",
            &json!({
                "preset": "m8",
                "face": [{"type": "face", "body": "NutBlock", "index": 6, "point": [4.0, 0.0, 12.0]}],
                "fdm_target": "metal"
            }),
        )
        .unwrap();
        assert_eq!(out2["kind"], "internal");
        assert!(r2.called_commands().contains(&"solid.offset_face"));
        let off = r2.calls.iter().find(|(c, _)| c == "solid.offset_face").unwrap();
        assert!(off.1["distance"].as_str().unwrap().contains("t1_major_d / 2"));

        // 3. Auto-match ISO without a face returns crisp error
        let mut r3 = FakeRunner::empty_selection();
        let err = execute_entry(&mut r3, "preset-thread", &json!({"preset": "auto_iso"})).unwrap_err();
        assert!(matches!(err, PluginAppError::Input { field, .. } if field == "face"));
    }

    #[test]
    fn custom_thread_handles_nominal_hole_and_chamfer_none_suppress() {
        let mut r = FakeRunner::internal_bore(10.0, true);
        let out = execute_entry(
            &mut r,
            "custom-thread",
            &json!({
                "pitch": 3.0,
                "face": [{"type": "face", "body": "NutBlock", "index": 6}],
                "fdm_adjust": true,
                "fdm_fit": "wide",
                "opt_chamfer": "none"
            }),
        )
        .unwrap();
        assert_eq!(out["kind"], "internal");
        assert!((out["radial_clearance"].as_f64().unwrap() - 0.32).abs() < 1e-6);
        assert!(r.called_commands().contains(&"solid.offset_face"));
        // Even with chamfer = none, chamfer feature is created & suppressed so Edit Thread can turn it on!
        assert!(r.calls.iter().any(|(c, a)| c == "timeline.suppress" && a["suppressed"] == true));
    }

    #[test]
    fn advanced_thread_and_edit_thread_round_trip_and_legacy_v020_compat() {
        let mut r = FakeRunner::external_shaft(10.0);
        // Create standalone pair via opt_output="pair" even with a face selected in viewport
        let created = execute_entry(
            &mut r,
            "advanced-thread",
            &json!({"opt_output": "pair", "opt_chamfer": "none", "tooth_crest_fillet": 0.1}),
        )
        .unwrap();
        assert_eq!(created["bodies"], json!(["Bolt", "Nut"]));

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
                "end_chamfer": "wide"
            }),
        )
        .unwrap();
        assert_eq!(edited["major_diameter"], 24.0);
        assert_eq!(edited["starts"], 2);
        assert_eq!(edited["left_hand"], true);
        assert_eq!(edited["end_chamfer"], "both");
        assert_eq!(edited["chamfer_size"], "wide");
        assert!(edited["folder_name"].as_str().unwrap().contains("Adv Ø24.0x4.00"));
        assert!(r.called_commands().contains(&"timeline.rename_group"));
        assert!(r.calls.iter().any(|(c, a)| c == "timeline.suppress" && a["suppressed"] == false));

        // Simulate legacy v0.2.0 ThreadMeta stored in design parameters
        let mut r_legacy = FakeRunner::external_shaft(10.0);
        for (name, expr, unit) in [
            ("old_pitch", "3.0 mm", "mm"),
            ("old_length", "20.0 mm", "mm"),
            ("old_starts", "1", ""),
            ("old_upper_ang", "45.0 deg", "deg"),
            ("old_lower_ang", "45.0 deg", "deg"),
            ("old_radial_clr", "0.2 mm", "mm"),
            ("old_axial_clr", "0.1 mm", "mm"),
            ("old_crest_flat", "0.4 mm", "mm"),
            ("old_root_flat", "0.4 mm", "mm"),
            ("old_base_r", "10.0 mm", "mm"),
        ] {
            r_legacy.parameters.push(json!({
                "name": name,
                "expression": expr,
                "unit": unit,
                "value": parse_fake_num(expr),
                "favorite": true,
            }));
        }
        let v020_meta = r#"{"v":1,"prefix":"old","kind":"internal","bodies":["NutBlock"],"coils":["CustomThread_Coil"],"chamfers":["CustomThread_Chamfer"],"offset_feature":"OffsetFace1","group_id":10,"apply_clearance":true,"has_crest_fillet":false,"has_root_fillet":false,"has_major_d":false,"has_nut_wall":false,"has_spacing":false,"left_hand":false,"end_chamfer":"both","face_span":20.0}"#;
        r_legacy.parameters.push(json!({
            "name": "old_meta",
            "expression": v020_meta,
            "text": v020_meta,
            "unit": "text",
            "favorite": false,
        }));
        let res_leg = execute_entry(
            &mut r_legacy,
            "edit-thread",
            &json!({"param_prefix": "old", "radial_clearance": 0.0}),
        )
        .unwrap();
        assert_eq!(res_leg["param_prefix"], "old");
        let sup_idx = r_legacy
            .calls
            .iter()
            .position(|(c, a)| c == "timeline.suppress" && a["suppressed"] == true)
            .unwrap();
        let chg_idx = r_legacy
            .calls
            .iter()
            .position(|(c, a)| c == "parameters.change" && a.get("changes").is_some())
            .unwrap();
        assert!(sup_idx < chg_idx);
    }

    #[test]
    fn manifest_size_defaults_svg_lint_and_commands_valid() {
        let raw = include_str!("../plugin.json");
        let m: Value = serde_json::from_str(raw).unwrap();
        let minified = serde_json::to_vec(&m).unwrap();
        if minified.len() > 16_384 {
            let breakdown: Vec<String> = m["runtime"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    format!(
                        "{}: {}",
                        e["name"].as_str().unwrap(),
                        serde_json::to_vec(e).unwrap().len()
                    )
                })
                .collect();
            panic!(
                "Minified plugin.json is {} bytes (> 16,384): {}",
                minified.len(),
                breakdown.join(", ")
            );
        }
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
        let mut all_referenced_svgs = std::collections::HashSet::new();
        let mut entry_icons = std::collections::HashSet::new();
        let mut seen_slot_names = std::collections::HashSet::new();
        let mut declared_entries = std::collections::HashSet::new();

        for entry in m["runtime"]["entries"].as_array().unwrap() {
            let name = entry["name"].as_str().unwrap();
            declared_entries.insert(name.to_string());
            let icon = entry["input_schema"]["x-solidcraft-icon"]
                .as_str()
                .expect("entry must declare x-solidcraft-icon");
            assert!(entry_icons.insert(icon.to_string()), "duplicate entry icon {icon}");
            all_referenced_svgs.insert(icon.to_string());

            let slots = entry["slots"].as_array().expect("entry must declare slots");
            assert!(!slots.is_empty());
            for slot in slots {
                let skind = slot["kind"].as_str().expect("slot missing kind");
                assert!(
                    matches!(skind, "ribbon" | "menu"),
                    "unexpected slot kind '{skind}' on {name}"
                );
                let sname = slot["name"].as_str().expect("slot missing name");
                assert!(!sname.trim().is_empty(), "empty slot name on {name}");
                assert!(
                    seen_slot_names.insert(sname.to_string()),
                    "duplicate slot label '{sname}' on {name}"
                );
            }

            let props = entry["input_schema"]["properties"].as_object().unwrap();
            let mut defaults = serde_json::Map::new();
            for (k, prop) in props {
                let desc = prop
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("property {name}.{k} missing description"));
                let title = prop["title"].as_str().unwrap_or("");
                assert!(
                    desc.trim().len() >= 25,
                    "property {name}.{k} description too short: {desc:?}"
                );
                assert_ne!(
                    desc.trim(),
                    title,
                    "property {name}.{k} description must not simply repeat title"
                );
                if let Some(d) = prop.get("default") {
                    defaults.insert(k.clone(), d.clone());
                }
                if let Some(opts) = prop.get("oneOf").and_then(Value::as_array) {
                    let any_has_icon = opts.iter().any(|o| o.get("x-solidcraft-icon").is_some());
                    if any_has_icon {
                        for o in opts {
                            let oi = o.get("x-solidcraft-icon").and_then(Value::as_str).unwrap_or_else(|| {
                                panic!("property {name}.{k} option missing x-solidcraft-icon");
                            });
                            all_referenced_svgs.insert(oi.to_string());
                        }
                    }
                }
            }

            let mut r = FakeRunner::empty_selection();
            if name == "edit-thread" {
                execute_entry(
                    &mut r,
                    "advanced-thread",
                    &json!({"tooth_crest_fillet": 0.1, "tooth_root_fillet": 0.1}),
                )
                .unwrap();
                defaults.insert("pitch".into(), json!(3.5));
                defaults.insert("starts".into(), json!(2));
                defaults.insert("left_hand".into(), json!("left"));
                defaults.insert("end_chamfer".into(), json!("wide"));
                r.calls.clear();
            }
            execute_entry(&mut r, name, &Value::Object(defaults)).unwrap_or_else(|e| {
                panic!("entry '{name}' failed with manifest defaults: {e:?}");
            });
            for c in r.called_commands() {
                assert!(
                    allowed.contains(&c),
                    "entry '{name}' called '{c}' missing from runtime.commands"
                );
            }
        }

        // SolidCraft SVG subset check on every referenced icon (<= 64 files total)
        // and ensure no unreferenced orphan SVGs remain in assets/.
        assert!(all_referenced_svgs.len() <= 64);
        for entry in std::fs::read_dir(manifest_dir.join("assets")).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("svg") {
                let rel = format!(
                    "assets/{}",
                    path.file_name().unwrap().to_str().unwrap()
                );
                assert!(
                    all_referenced_svgs.contains(&rel),
                    "orphan SVG file not referenced in plugin.json: {rel}"
                );
            }
        }
        for rel in &all_referenced_svgs {
            assert!(rel.starts_with("assets/") && rel.ends_with(".svg"), "bad svg path {rel}");
            let full = manifest_dir.join(rel);
            assert!(full.is_file(), "missing icon file {rel}");
            let svg = std::fs::read_to_string(&full).unwrap();
            assert!(svg.contains("viewBox=\"0 0 64 64\""), "{rel} missing viewBox 0 0 64 64");
            assert!(svg.contains("currentColor"), "{rel} should use currentColor");
            for forbidden in ["<text", "<rect", "<line", "<polygon", "<polyline", "<use", "<style", "<image"] {
                assert!(!svg.contains(forbidden), "{rel} uses forbidden SVG element {forbidden}");
            }
            let mut rest = svg.as_str();
            while let Some(pos) = rest.find("d=\"") {
                let after = &rest[pos + 3..];
                let end = after.find('"').unwrap_or_else(|| panic!("{rel} unterminated d attribute"));
                let d_val = &after[..end];
                for ch in d_val.chars().filter(|c| c.is_ascii_alphabetic()) {
                    assert!(
                        matches!(ch, 'M' | 'L' | 'C' | 'Q' | 'A' | 'Z'),
                        "{rel} has disallowed SVG path command '{ch}' in d=\"{d_val}\""
                    );
                }
                rest = &after[end + 1..];
            }
        }

        fn count_files_recursive(dir: &std::path::Path) -> usize {
            let mut count = 0;
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    count += count_files_recursive(&path);
                } else if path.is_file() {
                    count += 1;
                }
            }
            count
        }

        // Verify SolidCraft `plugin test` suite (`tests/*.json`) covers every entry cleanly.
        let tests_dir = manifest_dir.join("tests");
        assert!(tests_dir.is_dir(), "missing tests/ directory");
        let mut covered_entries = std::collections::HashSet::new();
        let mut top_level_cases = 0usize;
        for entry in std::fs::read_dir(&tests_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
                top_level_cases += 1;
                let fname = path.file_name().unwrap().to_str().unwrap();
                let raw_case = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("failed to read tests/{fname}: {e}"));
                let case: Value = serde_json::from_str(&raw_case)
                    .unwrap_or_else(|e| panic!("invalid JSON in tests/{fname}: {e}"));
                let case_entry = case["entry"]
                    .as_str()
                    .unwrap_or_else(|| panic!("tests/{fname} missing string entry"));
                assert!(
                    declared_entries.contains(case_entry),
                    "tests/{fname} targets unknown entry '{case_entry}'"
                );
                covered_entries.insert(case_entry.to_string());
                if let Some(design) = case.get("design") {
                    let design_rel = design
                        .as_str()
                        .unwrap_or_else(|| panic!("tests/{fname} design must be a string"));
                    let rel_path = std::path::Path::new(design_rel);
                    assert!(
                        !rel_path.is_absolute()
                            && !rel_path
                                .components()
                                .any(|c| matches!(c, std::path::Component::ParentDir)),
                        "tests/{fname} invalid design fixture path '{design_rel}'"
                    );
                    assert!(
                        tests_dir.join(rel_path).is_file(),
                        "tests/{fname} missing fixture tests/{design_rel}"
                    );
                }
            }
        }
        assert!(top_level_cases > 0, "tests/ must contain at least one *.json case");
        for req in &declared_entries {
            assert!(
                covered_entries.contains(req),
                "manifest entry '{req}' has no case in tests/*.json"
            );
        }
        let total_packaged_files = 2 + all_referenced_svgs.len() + count_files_recursive(&tests_dir);
        assert!(
            total_packaged_files < 256,
            "packaged file count {total_packaged_files} exceeds SolidCraft 256-file cap"
        );
    }
}

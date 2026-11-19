pub mod commands;
pub mod math;

use commands::{
    EndCaps, EngineCommand, PairMode, PairParams, append_thread_commands,
    build_named_thread_pair_commands, inspect_target_cylinder,
};
use math::{
    EndChamfer, ThreadKind, ThreadProfileParams, ValidationError, dot3, parse_vec3, solve_groove,
    validate_and_depth,
};
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

    let profile = parse_profile(params, 20.0, 20.0)?;
    let nut_wall_thickness = get_f64(params, "nut_wall_thickness", 5.0)?;
    let pair_spacing = get_f64(params, "pair_spacing", 10.0)?;
    let coaxial_overlay = get_bool(params, "coaxial_overlay", false)?;

    // Validate before querying or mutating the design.
    let h_nom = validate_and_depth(&profile)?;

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

    let pair_params = PairParams {
        profile: profile.clone(),
        generate,
        nut_wall_thickness,
        pair_spacing,
        coaxial_overlay,
    };
    let cmds = build_named_thread_pair_commands(&pair_params, &bolt_name, &nut_name, &tag_prefix)?;
    run_commands(r, &cmds)?;

    let mut bodies = Vec::new();
    if matches!(generate, PairMode::Pair | PairMode::Bolt) {
        bodies.push(bolt_name);
    }
    if matches!(generate, PairMode::Pair | PairMode::Nut) {
        bodies.push(nut_name);
    }

    Ok(json!({
        "bodies": bodies,
        "major_diameter": profile.major_diameter,
        "minor_diameter": profile.major_diameter - 2.0 * h_nom,
        "pitch": profile.pitch,
        "lead": profile.pitch * (profile.starts as f64),
        "thread_depth": h_nom,
        "radial_clearance": profile.radial_clearance,
        "axial_clearance": profile.axial_clearance,
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

    // Parse and validate profile pitch/angles/flats early (using placeholder diameter/length
    // that are replaced with the actual cylinder face measurements right below).
    let raw_length = opt_f64(params, "length")?;
    let mut profile = parse_profile(params, 100.0, 20.0)?;
    let h_nom = validate_and_depth(&profile)?;

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
    let (z_start, z_end, eff_caps) = match raw_length {
        Some(len) if len > 0.0 && len < face_span - 1e-4 => {
            if end_caps.start_free && !end_caps.end_free {
                (
                    z_min,
                    z_min + len,
                    EndCaps {
                        start_free: true,
                        end_free: false,
                    },
                )
            } else if end_caps.end_free && !end_caps.start_free {
                (
                    z_max - len,
                    z_max,
                    EndCaps {
                        start_free: false,
                        end_free: true,
                    },
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
                    )
                } else {
                    (
                        z_max - len,
                        z_max,
                        EndCaps {
                            start_free: false,
                            end_free: end_caps.end_free,
                        },
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
                )
            }
        }
        Some(len) if len < 0.0 => {
            return Err(PluginAppError::input(
                "length",
                "length must be >= 0 (0 threads the full face)",
            ));
        }
        _ => (z_min, z_max, end_caps),
    };

    profile.length = z_end - z_start;
    profile.major_diameter = match kind {
        ThreadKind::External => 2.0 * radius,
        ThreadKind::Internal => 2.0 * (radius + h_nom),
    };

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
    let tag = unique_prefix("SmoothThread", &taken_features);

    let groove = solve_groove(&profile, kind, apply_clearance, 0.0)?;
    let mut cmds = Vec::new();

    // When clearance is enabled, offset the cylindrical wall into the material first:
    // negative `distance` shrinks an external shaft and widens an internal bore.
    if apply_clearance && profile.radial_clearance > 1e-6 {
        cmds.push(EngineCommand {
            command: "solid.offset_face".into(),
            params: json!({
                "faces": [face_name],
                "body": body,
                "distance": -profile.radial_clearance,
            }),
        });
    }

    append_thread_commands(
        &mut cmds,
        &tag,
        &body,
        &frame,
        &groove,
        z_start,
        z_end,
        eff_caps,
        profile.end_chamfer,
    )?;

    run_commands(r, &cmds)?;

    Ok(json!({
        "body": body,
        "face": face_name,
        "kind": kind,
        "major_diameter": 2.0 * groove.major_radius,
        "minor_diameter": 2.0 * groove.minor_radius,
        "pitch": profile.pitch,
        "lead": groove.lead,
        "thread_depth": h_nom,
        "radial_clearance": if apply_clearance { profile.radial_clearance } else { 0.0 },
        "axial_clearance": if apply_clearance { profile.axial_clearance } else { 0.0 },
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

    struct SmoothFdmThreads;

    impl exports::craft::plugin::entry::Guest for SmoothFdmThreads {
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

    export!(SmoothFdmThreads);
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
        calls: Vec<(String, Value)>,
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
                calls: Vec::new(),
            }
        }

        fn called_commands(&self) -> Vec<&str> {
            self.calls.iter().map(|(c, _)| c.as_str()).collect()
        }
    }

    impl Runner for FakeRunner {
        fn run(&mut self, command: &str, args: Value) -> Result<Value, String> {
            self.calls.push((command.to_string(), args));
            Ok(match command {
                "selection.get" => self.selection.clone(),
                "model.faces" => self.faces.clone(),
                "model.edges" => self.edges.clone(),
                "model.mesh" => self.mesh.clone(),
                "document.inspect" => self.doc.clone(),
                _ => json!({}),
            })
        }
    }

    #[test]
    fn create_thread_pair_runs_and_avoids_duplicate_names() {
        let mut r = FakeRunner::external_shaft();
        r.doc = json!({"timeline": [{"name": "BoltBlank"}], "bodies": ["Bolt", "Nut"]});
        let out = execute_entry(&mut r, "create_thread_pair", &json!({})).unwrap();
        assert_eq!(out["bodies"], json!(["Bolt_2", "Nut_2"]));
        assert!(r.called_commands().contains(&"solid.coil"));
        assert!(r.called_commands().contains(&"browser.visibility"));
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
        assert!(!r.called_commands().contains(&"solid.offset_face"));
        assert!(r.called_commands().contains(&"solid.pattern.circular"));
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
        assert_eq!(offset_call.1["distance"], -0.25);
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
                {"name": "SmoothThread_Profile"},
                {"name": "SmoothThread_Coil"}
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
        assert!(sketch_names.contains(&"SmoothThread_2_Profile"));
        assert!(sketch_names.contains(&"SmoothThread_2_ChamferSketch"));
    }

    #[test]
    fn manifest_defaults_and_commands_match_implementation() {
        let m: Value = serde_json::from_str(include_str!("../plugin.json")).unwrap();
        assert_eq!(m["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(m["capabilities"], json!(["commands@0.1"]));

        let allowed: Vec<&str> = m["runtime"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        for entry in m["runtime"]["entries"].as_array().unwrap() {
            let name = entry["name"].as_str().unwrap();
            let props = entry["input_schema"]["properties"].as_object().unwrap();
            let mut defaults = serde_json::Map::new();
            for (k, prop) in props {
                if let Some(d) = prop.get("default") {
                    defaults.insert(k.clone(), d.clone());
                }
            }
            if name == "apply-thread" {
                defaults.insert("starts".into(), json!(2));
                defaults.insert("crest_fillet".into(), json!(0.1));
                defaults.insert("root_fillet".into(), json!(0.1));
                defaults.insert("apply_clearance_to_external".into(), json!(true));
            }
            let mut r = FakeRunner::external_shaft();
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

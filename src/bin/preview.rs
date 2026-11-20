use custom_threads::commands::{PairMode, PairParams, build_thread_pair_commands};
use custom_threads::fdm::{ChamferChoice, FdmOptions, FitTier, MatingTarget, resolve_preset_profile};
use custom_threads::math::{ChamferSize, EndChamfer, ThreadProfileParams};
use custom_threads::presets::lookup_preset;
use serde_json::json;

fn write_case(out_dir: &std::path::Path, name: &str, params: &PairParams) {
    let cmds = build_thread_pair_commands(params).expect("valid preset");
    let path = out_dir.join(format!("{name}.json"));
    let body = json!({ "commands": cmds });
    std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap()).unwrap();
    println!("Wrote {} ({} commands)", path.display(), cmds.len());
}

fn base_profile() -> ThreadProfileParams {
    ThreadProfileParams {
        major_diameter: 20.0,
        pitch: 3.0,
        length: 20.0,
        starts: 1,
        left_hand: false,
        upper_flank_angle_deg: 45.0,
        lower_flank_angle_deg: 45.0,
        radial_clearance: 0.20,
        axial_clearance: 0.10,
        crest_flat: 0.40,
        root_flat: 0.40,
        crest_fillet: 0.0,
        root_fillet: 0.0,
        end_chamfer: EndChamfer::Both,
        chamfer_size: ChamferSize::Regular,
    }
}

fn main() {
    let out_dir_arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/preview".to_string());
    let out_dir = std::path::PathBuf::from(out_dir_arg);
    std::fs::create_dir_all(&out_dir).unwrap();

    // 1. Default 45 deg FDM bolt + nut pair
    write_case(
        &out_dir,
        "fdm45_pair",
        &PairParams {
            profile: base_profile(),
            generate: PairMode::Pair,
            nut_wall_thickness: 5.0,
            pair_spacing: 8.0,
            apply_bolt_clearance: false,
            coaxial_overlay: false,
        },
    );

    // 2. Garden Hose 3/4" GHT printed pair
    let ght = lookup_preset("ght_3_4").unwrap();
    let ght_prof = resolve_preset_profile(
        ght,
        None,
        None,
        false,
        &FdmOptions {
            adjust: true,
            fit: FitTier::Regular,
            nozzle_mm: 0.4,
            layer_mm: 0.2,
            mating: MatingTarget::Printed,
            chamfer: ChamferChoice::WideBoth,
        },
    )
    .unwrap();
    write_case(
        &out_dir,
        "preset_ght_3_4",
        &PairParams {
            profile: ght_prof,
            generate: PairMode::Pair,
            nut_wall_thickness: 5.5,
            pair_spacing: 10.0,
            apply_bolt_clearance: false,
            coaxial_overlay: false,
        },
    );

    // 3. M8x1.25 Regular FDM pair
    let m8 = lookup_preset("m8").unwrap();
    let m8_prof = resolve_preset_profile(
        m8,
        None,
        None,
        false,
        &FdmOptions {
            adjust: true,
            fit: FitTier::Regular,
            nozzle_mm: 0.4,
            layer_mm: 0.2,
            mating: MatingTarget::Printed,
            chamfer: ChamferChoice::RegularBoth,
        },
    )
    .unwrap();
    write_case(
        &out_dir,
        "preset_m8",
        &PairParams {
            profile: m8_prof,
            generate: PairMode::Pair,
            nut_wall_thickness: 4.0,
            pair_spacing: 8.0,
            apply_bolt_clearance: false,
            coaxial_overlay: false,
        },
    );

    // 4. Asymmetric buttress (10 deg / 45 deg)
    let mut buttress = base_profile();
    buttress.major_diameter = 24.0;
    buttress.pitch = 3.5;
    buttress.upper_flank_angle_deg = 10.0;
    buttress.lower_flank_angle_deg = 45.0;
    write_case(
        &out_dir,
        "buttress_10_45",
        &PairParams {
            profile: buttress,
            generate: PairMode::Pair,
            nut_wall_thickness: 5.0,
            pair_spacing: 8.0,
            apply_bolt_clearance: false,
            coaxial_overlay: false,
        },
    );

    // 5. Off-axis shaft (+X)
    let mut p_off = base_profile();
    p_off.major_diameter = 16.0;
    p_off.pitch = 2.5;
    let frame_x = custom_threads::math::build_axis_frame([10.0, 20.0, 5.0], [1.0, 0.0, 0.0]).unwrap();
    let g_off = custom_threads::math::solve_groove(&p_off, custom_threads::math::ThreadKind::External, false, 0.0).unwrap();
    let mut off_cmds = vec![custom_threads::commands::EngineCommand {
        command: "solid.cylinder".into(),
        params: json!({
            "radius": 8.0,
            "height": 20.0,
            "base": [10.0, 20.0, 5.0],
            "axis": "X",
            "operation": "new",
            "body_name": "OffAxisShaft",
            "name": "OffAxisBlank",
        }),
    }];
    custom_threads::commands::append_thread_commands(
        &mut off_cmds,
        "OffAxis",
        "OffAxisShaft",
        &frame_x,
        &g_off,
        0.0,
        20.0,
        custom_threads::commands::EndCaps::default(),
        p_off.end_chamfer,
    )
    .unwrap();
    std::fs::write(
        out_dir.join("off_axis_x.json"),
        serde_json::to_string_pretty(&json!({"commands": off_cmds})).unwrap(),
    )
    .unwrap();
}

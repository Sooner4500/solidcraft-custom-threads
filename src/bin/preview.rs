use custom_threads::commands::{PairMode, PairParams, build_thread_pair_commands};
use custom_threads::math::{EndChamfer, ThreadProfileParams};
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
            coaxial_overlay: false,
        },
    );

    // 1b. Bolt only (for single-body triangle comparison vs built-in M20)
    write_case(
        &out_dir,
        "fdm45_bolt",
        &PairParams {
            profile: base_profile(),
            generate: PairMode::Bolt,
            nut_wall_thickness: 5.0,
            pair_spacing: 8.0,
            coaxial_overlay: false,
        },
    );

    // 2. Coaxial overlay for cross-section clearance inspection
    write_case(
        &out_dir,
        "fdm45_coaxial",
        &PairParams {
            profile: base_profile(),
            generate: PairMode::Pair,
            nut_wall_thickness: 5.0,
            pair_spacing: 0.0,
            coaxial_overlay: true,
        },
    );

    // 3. Asymmetric buttress (10 deg load flank / 45 deg overhang flank)
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
            coaxial_overlay: false,
        },
    );

    // 4. Multi-start (2 starts, P=2.5, lead=5.0)
    let mut multi = base_profile();
    multi.pitch = 2.5;
    multi.starts = 2;
    write_case(
        &out_dir,
        "multistart_2",
        &PairParams {
            profile: multi,
            generate: PairMode::Pair,
            nut_wall_thickness: 5.0,
            pair_spacing: 8.0,
            coaxial_overlay: false,
        },
    );

    // 5. Left-hand thread
    let mut lh = base_profile();
    lh.left_hand = true;
    write_case(
        &out_dir,
        "left_hand",
        &PairParams {
            profile: lh,
            generate: PairMode::Pair,
            nut_wall_thickness: 5.0,
            pair_spacing: 8.0,
            coaxial_overlay: false,
        },
    );

    // 6. Filleted crest & root
    let mut filleted = base_profile();
    filleted.crest_fillet = 0.12;
    filleted.root_fillet = 0.12;
    write_case(
        &out_dir,
        "filleted_45",
        &PairParams {
            profile: filleted,
            generate: PairMode::Pair,
            nut_wall_thickness: 5.0,
            pair_spacing: 8.0,
            coaxial_overlay: false,
        },
    );

    // 7. Off-axis shaft (+X at [10, 20, 5]) using append_thread_commands directly
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

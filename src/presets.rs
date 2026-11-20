use crate::math::{ThreadProfileParams, ValidationError, validate_and_depth};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadPreset {
    pub id: &'static str,
    pub short_label: &'static str,
    pub major_diameter: f64,
    pub pitch: f64,
    pub upper_deg: f64,
    pub lower_deg: f64,
    pub crest_flat: f64,
    pub root_flat: f64,
    pub default_length: f64,
    pub starts: u32,
}

const fn iso_preset(
    id: &'static str,
    short_label: &'static str,
    major_diameter: f64,
    pitch: f64,
    default_length: f64,
) -> ThreadPreset {
    let flat = pitch / 8.0;
    ThreadPreset {
        id,
        short_label,
        major_diameter,
        pitch,
        upper_deg: 30.0,
        lower_deg: 30.0,
        crest_flat: flat,
        root_flat: flat,
        default_length,
        starts: 1,
    }
}

pub const FIXED_PRESETS: &[ThreadPreset] = &[
    // Everyday, Bottles, Jars & Valves
    ThreadPreset {
        id: "ght_3_4",
        short_label: "Garden Hose 3/4\" GHT",
        major_diameter: 26.988,
        pitch: 2.209,
        upper_deg: 30.0,
        lower_deg: 30.0,
        crest_flat: 2.209 / 8.0,
        root_flat: 2.209 / 8.0,
        default_length: 16.0,
        starts: 1,
    },
    ThreadPreset {
        id: "pco_1881",
        short_label: "Soda Bottle PCO-1881",
        major_diameter: 27.40,
        pitch: 2.70,
        upper_deg: 45.0,
        lower_deg: 15.0,
        crest_flat: 0.48,
        root_flat: 0.70,
        default_length: 14.0,
        starts: 1,
    },
    ThreadPreset {
        id: "mason_70g",
        short_label: "Mason Jar 70G",
        major_diameter: 71.20,
        pitch: 6.35,
        upper_deg: 30.0,
        lower_deg: 30.0,
        crest_flat: 1.40,
        root_flat: 3.45,
        default_length: 16.0,
        starts: 1,
    },
    ThreadPreset {
        id: "mason_86g",
        short_label: "Mason Jar Wide 86G",
        major_diameter: 86.20,
        pitch: 6.35,
        upper_deg: 30.0,
        lower_deg: 30.0,
        crest_flat: 1.40,
        root_flat: 3.45,
        default_length: 16.0,
        starts: 1,
    },
    ThreadPreset {
        id: "acme_broom",
        short_label: "Broom Pole 3/4\"-5",
        major_diameter: 19.05,
        pitch: 5.080,
        upper_deg: 14.5,
        lower_deg: 14.5,
        crest_flat: 1.883,
        root_flat: 1.883,
        default_length: 25.0,
        starts: 1,
    },
    ThreadPreset {
        id: "valve_8v1",
        short_label: "Tire Valve 8V1",
        major_diameter: 7.747,
        pitch: 0.794,
        upper_deg: 30.0,
        lower_deg: 30.0,
        crest_flat: 0.794 / 8.0,
        root_flat: 0.794 / 8.0,
        default_length: 10.0,
        starts: 1,
    },
    ThreadPreset {
        id: "valve_5v2",
        short_label: "Presta Valve 5V2",
        major_diameter: 5.20,
        pitch: 1.058,
        upper_deg: 30.0,
        lower_deg: 30.0,
        crest_flat: 1.058 / 8.0,
        root_flat: 1.058 / 8.0,
        default_length: 10.0,
        starts: 1,
    },
    // Metric Bolts & Nuts (ISO 60 deg)
    iso_preset("m5", "M5x0.8", 5.0, 0.8, 12.0),
    iso_preset("m6", "M6x1.0", 6.0, 1.0, 14.0),
    iso_preset("m8", "M8x1.25", 8.0, 1.25, 16.0),
    iso_preset("m10", "M10x1.5", 10.0, 1.5, 18.0),
    iso_preset("m12", "M12x1.75", 12.0, 1.75, 20.0),
    iso_preset("m16", "M16x2.0", 16.0, 2.0, 22.0),
    iso_preset("m20", "M20x2.5", 20.0, 2.5, 24.0),
    iso_preset("m24", "M24x3.0", 24.0, 3.0, 28.0),
    // US Bolts, Nuts & Tripods (UNC 60 deg)
    iso_preset("unc_1_4", "Tripod 1/4\"-20", 6.350, 25.4 / 20.0, 14.0),
    iso_preset("unc_5_16", "UNC 5/16\"-18", 7.9375, 25.4 / 18.0, 16.0),
    iso_preset("unc_3_8", "Tripod 3/8\"-16", 9.525, 25.4 / 16.0, 18.0),
    iso_preset("unc_1_2", "UNC 1/2\"-13", 12.700, 25.4 / 13.0, 20.0),
    iso_preset("unc_5_8", "UNC 5/8\"-11", 15.875, 25.4 / 11.0, 22.0),
    iso_preset("unc_3_4", "UNC 3/4\"-10", 19.050, 25.4 / 10.0, 24.0),
    // Plumbing, Water Tanks & Cable Glands
    iso_preset("npsm_1_2", "Pipe 1/2\" NPSM", 21.224, 25.4 / 14.0, 16.0),
    iso_preset("npsm_3_4", "Pipe 3/4\" NPSM", 26.568, 25.4 / 14.0, 18.0),
    ThreadPreset {
        id: "bspp_1_2",
        short_label: "Pipe G 1/2\" BSPP",
        major_diameter: 20.955,
        pitch: 25.4 / 14.0,
        upper_deg: 27.5,
        lower_deg: 27.5,
        crest_flat: 0.30,
        root_flat: 0.30,
        default_length: 16.0,
        starts: 1,
    },
    ThreadPreset {
        id: "bspp_3_4",
        short_label: "Pipe G 3/4\" BSPP",
        major_diameter: 26.441,
        pitch: 25.4 / 14.0,
        upper_deg: 27.5,
        lower_deg: 27.5,
        crest_flat: 0.30,
        root_flat: 0.30,
        default_length: 18.0,
        starts: 1,
    },
    ThreadPreset {
        id: "ibc_s60x6",
        short_label: "IBC Tote S60x6",
        major_diameter: 60.0,
        pitch: 6.0,
        upper_deg: 30.0,
        lower_deg: 45.0,
        crest_flat: 0.634,
        root_flat: 0.634,
        default_length: 24.0,
        starts: 1,
    },
    ThreadPreset {
        id: "pg13_5",
        short_label: "Cable Gland PG13.5",
        major_diameter: 20.40,
        pitch: 25.4 / 18.0,
        upper_deg: 40.0,
        lower_deg: 40.0,
        crest_flat: 0.144,
        root_flat: 0.144,
        default_length: 12.0,
        starts: 1,
    },
    ThreadPreset {
        id: "pg16",
        short_label: "Cable Gland PG16",
        major_diameter: 22.50,
        pitch: 25.4 / 18.0,
        upper_deg: 40.0,
        lower_deg: 40.0,
        crest_flat: 0.144,
        root_flat: 0.144,
        default_length: 12.0,
        starts: 1,
    },
    ThreadPreset {
        id: "pg21",
        short_label: "Cable Gland PG21",
        major_diameter: 28.30,
        pitch: 25.4 / 16.0,
        upper_deg: 40.0,
        lower_deg: 40.0,
        crest_flat: 0.160,
        root_flat: 0.160,
        default_length: 14.0,
        starts: 1,
    },
    // Camera Lenses & Filters
    iso_preset("cam_m42", "Camera T2 M42x0.75", 42.0, 0.75, 8.0),
    iso_preset("cam_m52", "Filter 52mm M52x0.75", 52.0, 0.75, 8.0),
    iso_preset("cam_m58", "Filter 58mm M58x0.75", 58.0, 0.75, 8.0),
    iso_preset("cam_m67", "Filter 67mm M67x0.75", 67.0, 0.75, 8.0),
];

const ISO_COARSE_TABLE: &[ThreadPreset] = &[
    iso_preset("m3", "M3x0.5", 3.0, 0.5, 10.0),
    iso_preset("m3_5", "M3.5x0.6", 3.5, 0.6, 10.0),
    iso_preset("m4", "M4x0.7", 4.0, 0.7, 10.0),
    iso_preset("m5", "M5x0.8", 5.0, 0.8, 12.0),
    iso_preset("m6", "M6x1.0", 6.0, 1.0, 14.0),
    iso_preset("m7", "M7x1.0", 7.0, 1.0, 14.0),
    iso_preset("m8", "M8x1.25", 8.0, 1.25, 16.0),
    iso_preset("m10", "M10x1.5", 10.0, 1.5, 18.0),
    iso_preset("m12", "M12x1.75", 12.0, 1.75, 20.0),
    iso_preset("m14", "M14x2.0", 14.0, 2.0, 20.0),
    iso_preset("m16", "M16x2.0", 16.0, 2.0, 22.0),
    iso_preset("m18", "M18x2.5", 18.0, 2.5, 22.0),
    iso_preset("m20", "M20x2.5", 20.0, 2.5, 24.0),
    iso_preset("m22", "M22x2.5", 22.0, 2.5, 26.0),
    iso_preset("m24", "M24x3.0", 24.0, 3.0, 28.0),
    iso_preset("m27", "M27x3.0", 27.0, 3.0, 30.0),
    iso_preset("m30", "M30x3.5", 30.0, 3.5, 32.0),
    iso_preset("m33", "M33x3.5", 33.0, 3.5, 35.0),
    iso_preset("m36", "M36x4.0", 36.0, 4.0, 36.0),
    iso_preset("m39", "M39x4.0", 39.0, 4.0, 40.0),
    iso_preset("m42", "M42x4.5", 42.0, 4.5, 42.0),
    iso_preset("m45", "M45x4.5", 45.0, 4.5, 45.0),
    iso_preset("m48", "M48x5.0", 48.0, 5.0, 48.0),
    iso_preset("m52", "M52x5.0", 52.0, 5.0, 52.0),
    iso_preset("m56", "M56x5.5", 56.0, 5.5, 56.0),
    iso_preset("m60", "M60x5.5", 60.0, 5.5, 60.0),
    iso_preset("m64", "M64x6.0", 64.0, 6.0, 64.0),
];

const UNC_COARSE_TABLE: &[ThreadPreset] = &[
    iso_preset("unc_10", "#10-24 UNC", 4.826, 25.4 / 24.0, 12.0),
    iso_preset("unc_1_4", "1/4\"-20 UNC", 6.350, 25.4 / 20.0, 14.0),
    iso_preset("unc_5_16", "5/16\"-18 UNC", 7.9375, 25.4 / 18.0, 16.0),
    iso_preset("unc_3_8", "3/8\"-16 UNC", 9.525, 25.4 / 16.0, 18.0),
    iso_preset("unc_7_16", "7/16\"-14 UNC", 11.1125, 25.4 / 14.0, 18.0),
    iso_preset("unc_1_2", "1/2\"-13 UNC", 12.700, 25.4 / 13.0, 20.0),
    iso_preset("unc_9_16", "9/16\"-12 UNC", 14.2875, 25.4 / 12.0, 20.0),
    iso_preset("unc_5_8", "5/8\"-11 UNC", 15.875, 25.4 / 11.0, 22.0),
    iso_preset("unc_3_4", "3/4\"-10 UNC", 19.050, 25.4 / 10.0, 24.0),
    iso_preset("unc_7_8", "7/8\"-9 UNC", 22.225, 25.4 / 9.0, 28.0),
    iso_preset("unc_1", "1\"-8 UNC", 25.400, 25.4 / 8.0, 30.0),
];

pub fn lookup_preset(id: &str) -> Option<&'static ThreadPreset> {
    FIXED_PRESETS.iter().find(|p| p.id == id)
}

pub fn nominal_depth(p: &ThreadPreset) -> f64 {
    let tan_sum = p.upper_deg.to_radians().tan() + p.lower_deg.to_radians().tan();
    (p.pitch - p.crest_flat - p.root_flat) / tan_sum
}

pub fn tap_drill_diameter(p: &ThreadPreset) -> f64 {
    // Standard 75%-engagement tap-drill hole (~D - 1.04*P, matching e.g. 6.75 mm for M8x1.25 and 8.5 mm for M10x1.5).
    (p.major_diameter - 1.04 * p.pitch).max(0.5)
}

fn match_from_table(
    table: &[ThreadPreset],
    table_name: &str,
    diameter_mm: f64,
    is_internal: bool,
) -> Result<ThreadPreset, ValidationError> {
    let mut best_major: Option<(f64, &ThreadPreset)> = None;
    for entry in table {
        let diff = (diameter_mm - entry.major_diameter).abs();
        if best_major.is_none_or(|(bd, _)| diff < bd) {
            best_major = Some((diff, entry));
        }
    }

    let mut best_tap: Option<(f64, &ThreadPreset)> = None;
    if is_internal {
        for entry in table {
            let tap = tap_drill_diameter(entry);
            let diff = (diameter_mm - tap).abs();
            if best_tap.is_none_or(|(bd, _)| diff < bd) {
                best_tap = Some((diff, entry));
            }
        }
    }

    // Prefer exact/near nominal major diameter on integer/standard bores, otherwise
    // choose whichever match (nominal major vs. tap-drill minor) is closer.
    if let (Some((m_diff, m_entry)), Some((t_diff, t_entry))) = (best_major, best_tap) {
        let maj_tol = (m_entry.major_diameter * 0.04).clamp(0.18, 0.75);
        let tap_tol = (tap_drill_diameter(t_entry) * 0.05).clamp(0.20, 0.85);
        if m_diff < 0.06 && m_diff <= maj_tol {
            return Ok(*m_entry);
        }
        if t_diff <= tap_tol && t_diff < m_diff {
            return Ok(*t_entry);
        }
        if m_diff <= maj_tol {
            return Ok(*m_entry);
        }
    } else if let Some((diff, entry)) = best_major {
        let maj_tol = (entry.major_diameter * 0.04).clamp(0.18, 0.75);
        if diff <= maj_tol {
            return Ok(*entry);
        }
    }

    // Fallback to nearest major within generous 5% window, otherwise explain clearly.
    if let Some((diff, entry)) = best_major
        && diff <= (entry.major_diameter * 0.05).max(0.45)
    {
        return Ok(*entry);
    }

    Err(ValidationError::new(
        "preset",
        format!(
            "Picked cylinder diameter Ø{diameter_mm:.2} mm does not match a standard {table_name} coarse thread; pick a specific Preset or use Custom Thread."
        ),
    ))
}

pub fn match_iso_coarse(diameter_mm: f64, is_internal: bool) -> Result<ThreadPreset, ValidationError> {
    match_from_table(ISO_COARSE_TABLE, "ISO Metric Coarse (M3-M64)", diameter_mm, is_internal)
}

pub fn match_unc_coarse(diameter_mm: f64, is_internal: bool) -> Result<ThreadPreset, ValidationError> {
    match_from_table(UNC_COARSE_TABLE, "US UNC Coarse (#10-1\")", diameter_mm, is_internal)
}

pub fn resolve_preset_by_id(
    preset_id: &str,
    picked_diameter: Option<f64>,
    is_internal: bool,
) -> Result<ThreadPreset, ValidationError> {
    match preset_id {
        "auto_iso" => {
            let d = picked_diameter.ok_or_else(|| {
                ValidationError::new(
                    "face",
                    "Pick a cylindrical shaft or hole face to auto-match a Metric thread, or choose a specific size (e.g. M8x1.25).",
                )
            })?;
            match_iso_coarse(d, is_internal)
        }
        "auto_unc" => {
            let d = picked_diameter.ok_or_else(|| {
                ValidationError::new(
                    "face",
                    "Pick a cylindrical shaft or hole face to auto-match a US Inch thread, or choose a specific size (e.g. 1/4\"-20).",
                )
            })?;
            match_unc_coarse(d, is_internal)
        }
        other => lookup_preset(other).copied().ok_or_else(|| {
            ValidationError::new("preset", format!("Unknown thread preset '{other}'"))
        }),
    }
}

/// Rejects extreme diameter mismatches when applying a fixed preset to an existing cylinder
/// while allowing nominal major holes, tap-drill holes, and slightly oversized/undersized blanks.
pub fn validate_preset_face_match(
    picked_d: f64,
    preset: &ThreadPreset,
    is_internal: bool,
) -> Result<(), ValidationError> {
    let target_major = preset.major_diameter;
    let max_delta = (0.35 * target_major).max(4.0);
    let dist = if is_internal {
        let minor = tap_drill_diameter(preset);
        (picked_d - target_major).abs().min((picked_d - minor).abs())
    } else {
        (picked_d - target_major).abs()
    };

    if dist > max_delta {
        return Err(ValidationError::new(
            "preset",
            format!(
                "Picked cylinder Ø{picked_d:.2} mm is far from preset {} (nominal Ø{target_major:.2} mm). Pick a matching face, switch preset, or use Custom Thread.",
                preset.short_label
            ),
        ));
    }

    // Ensure profile itself stays valid (sanity check using nominal length).
    let _ = validate_and_depth(&ThreadProfileParams {
        major_diameter: preset.major_diameter,
        pitch: preset.pitch,
        length: preset.default_length,
        starts: preset.starts,
        left_hand: false,
        upper_flank_angle_deg: preset.upper_deg,
        lower_flank_angle_deg: preset.lower_deg,
        radial_clearance: 0.0,
        axial_clearance: 0.0,
        crest_flat: preset.crest_flat,
        root_flat: preset.root_flat,
        crest_fillet: 0.0,
        root_fillet: 0.0,
        end_chamfer: crate::math::EndChamfer::Both,
        chamfer_size: crate::math::ChamferSize::Regular,
    })?;
    Ok(())
}

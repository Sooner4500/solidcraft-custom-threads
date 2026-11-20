use crate::math::{
    ChamferSize, EndChamfer, MIN_AXIAL_GAP_MM, ThreadKind, ThreadProfileParams, ValidationError,
    validate_and_depth,
};
use crate::presets::ThreadPreset;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitTier {
    Tight,
    Regular,
    Wide,
    None,
}

impl FitTier {
    pub fn parse(raw: Option<&str>) -> Result<Self, ValidationError> {
        match raw.unwrap_or("regular") {
            "tight" => Ok(Self::Tight),
            "regular" => Ok(Self::Regular),
            "wide" => Ok(Self::Wide),
            "none" => Ok(Self::None),
            other => Err(ValidationError::new(
                "fdm_fit",
                format!("Unsupported clearance fit '{other}'"),
            )),
        }
    }

    pub fn base_clearances(self) -> (f64, f64) {
        match self {
            Self::Tight => (0.12, 0.06),
            Self::Regular => (0.20, 0.10),
            Self::Wide => (0.32, 0.16),
            Self::None => (0.0, 0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatingTarget {
    Printed,
    Metal,
}

impl MatingTarget {
    pub fn parse(raw: Option<&str>, field: &str, default: Self) -> Result<Self, ValidationError> {
        match raw {
            None => Ok(default),
            Some("printed") => Ok(Self::Printed),
            Some("metal") => Ok(Self::Metal),
            Some(other) => Err(ValidationError::new(
                field,
                format!("Unsupported mating target '{other}'"),
            )),
        }
    }

    pub fn applies_to(self, kind: ThreadKind) -> bool {
        match (self, kind) {
            (_, ThreadKind::Internal) => true,
            (Self::Printed, ThreadKind::External) => false,
            (Self::Metal, ThreadKind::External) => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChamferChoice {
    RegularBoth,
    WideBoth,
    RegularStart,
    WideStart,
    None,
}

impl ChamferChoice {
    pub fn parse(raw: Option<&str>) -> Result<Self, ValidationError> {
        match raw.unwrap_or("regular_both") {
            "regular_both" | "both" => Ok(Self::RegularBoth),
            "wide_both" => Ok(Self::WideBoth),
            "regular_start" | "start" => Ok(Self::RegularStart),
            "wide_start" => Ok(Self::WideStart),
            "none" => Ok(Self::None),
            other => Err(ValidationError::new(
                "opt_chamfer",
                format!("Unsupported end chamfer choice '{other}'"),
            )),
        }
    }

    pub fn to_end_and_size(self) -> (EndChamfer, ChamferSize) {
        match self {
            Self::RegularBoth => (EndChamfer::Both, ChamferSize::Regular),
            Self::WideBoth => (EndChamfer::Both, ChamferSize::Wide),
            Self::RegularStart => (EndChamfer::Start, ChamferSize::Regular),
            Self::WideStart => (EndChamfer::Start, ChamferSize::Wide),
            Self::None => (EndChamfer::None, ChamferSize::Regular),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FdmOptions {
    pub adjust: bool,
    pub fit: FitTier,
    pub nozzle_mm: f64,
    pub layer_mm: f64,
    pub mating: MatingTarget,
    pub chamfer: ChamferChoice,
}

pub fn validate_printer_inputs(nozzle_mm: f64, layer_mm: f64) -> Result<(), ValidationError> {
    if !nozzle_mm.is_finite() || !(0.10..=2.00).contains(&nozzle_mm) {
        return Err(ValidationError::new(
            "fdm_nozzle_mm",
            "Nozzle diameter must be between 0.10 and 2.00 mm",
        ));
    }
    if !layer_mm.is_finite() || !(0.04..=1.00).contains(&layer_mm) {
        return Err(ValidationError::new(
            "fdm_layer_mm",
            "Layer height must be between 0.04 and 1.00 mm",
        ));
    }
    Ok(())
}

fn compute_fdm_clearances(
    fit: FitTier,
    nozzle_mm: f64,
    layer_mm: f64,
    pitch: f64,
    crest_flat: f64,
    root_flat: f64,
) -> (f64, f64) {
    if fit == FitTier::None {
        return (0.0, 0.0);
    }
    let (base_r, base_a) = fit.base_clearances();
    let r_delta = 0.20 * (nozzle_mm - 0.40) + 0.25 * (layer_mm - 0.20);
    let a_delta = 0.10 * (nozzle_mm - 0.40) + 0.15 * (layer_mm - 0.20);

    let rad_clr = (base_r + r_delta).clamp(0.5 * base_r, base_r + 0.25);

    let max_flat_gap = (crest_flat.min(root_flat) - (MIN_AXIAL_GAP_MM + 0.02)).max(0.0);
    let max_ax = (0.14 * pitch).min(max_flat_gap);
    let ax_clr = if max_ax <= 0.0 {
        0.0
    } else {
        (base_a + a_delta)
            .clamp(0.5 * base_a, 0.25)
            .min(max_ax)
    };
    (rad_clr, ax_clr)
}

pub fn resolve_preset_profile(
    preset: &ThreadPreset,
    length_override: Option<f64>,
    starts_override: Option<u32>,
    left_hand: bool,
    fdm: &FdmOptions,
) -> Result<ThreadProfileParams, ValidationError> {
    validate_printer_inputs(fdm.nozzle_mm, fdm.layer_mm)?;
    let length = length_override.unwrap_or(preset.default_length);
    let starts = starts_override.unwrap_or(preset.starts);
    let (end_chamfer, chamfer_size) = fdm.chamfer.to_end_and_size();

    let (crest_flat, root_flat, radial_clearance, axial_clearance) = if fdm.adjust {
        let nozzle_flat = fdm.nozzle_mm.min(0.25 * preset.pitch);
        let mut cf = preset.crest_flat.max(nozzle_flat);
        let mut rf = preset.root_flat.max(nozzle_flat);

        let base_r = match fdm.fit {
            FitTier::None => 0.0,
            _ => {
                let (br, _) = fdm.fit.base_clearances();
                let r_delta = 0.20 * (fdm.nozzle_mm - 0.40) + 0.25 * (fdm.layer_mm - 0.20);
                (br + r_delta).clamp(0.5 * br, br + 0.25)
            }
        };

        // When mating with existing Metal / Factory Hardware, extra tip/root flat truncation must
        // stay within 85% of the radial clearance envelope so printed valleys stay deeper than the
        // metal counterpart's minor bore and printed teeth never bind axially against metal flanks.
        if fdm.mating == MatingTarget::Metal {
            let tan_sum =
                preset.upper_deg.to_radians().tan() + preset.lower_deg.to_radians().tan();
            let extra_budget = base_r * tan_sum * 0.85;
            let req_c = (cf - preset.crest_flat).max(0.0);
            let req_r = (rf - preset.root_flat).max(0.0);
            if req_c + req_r > extra_budget {
                let add_c = req_c.min(extra_budget);
                let add_r = req_r.min((extra_budget - add_c).max(0.0));
                cf = preset.crest_flat + add_c;
                rf = preset.root_flat + add_r;
            }
        }

        let (rc, ac) = compute_fdm_clearances(
            fdm.fit,
            fdm.nozzle_mm,
            fdm.layer_mm,
            preset.pitch,
            cf,
            rf,
        );
        (cf, rf, rc, ac)
    } else {
        (preset.crest_flat, preset.root_flat, 0.0, 0.0)
    };

    let prof = ThreadProfileParams {
        major_diameter: preset.major_diameter,
        pitch: preset.pitch,
        length,
        starts,
        left_hand,
        upper_flank_angle_deg: preset.upper_deg,
        lower_flank_angle_deg: preset.lower_deg,
        radial_clearance,
        axial_clearance,
        crest_flat,
        root_flat,
        crest_fillet: 0.0,
        root_fillet: 0.0,
        end_chamfer,
        chamfer_size,
    };
    let _ = validate_and_depth(&prof)?;
    Ok(prof)
}

pub fn resolve_custom_profile(
    major_diameter: f64,
    pitch: f64,
    length: f64,
    starts: u32,
    left_hand: bool,
    fdm: &FdmOptions,
) -> Result<ThreadProfileParams, ValidationError> {
    validate_printer_inputs(fdm.nozzle_mm, fdm.layer_mm)?;
    let (end_chamfer, chamfer_size) = fdm.chamfer.to_end_and_size();

    let (upper_deg, lower_deg, crest_flat, root_flat, radial_clearance, axial_clearance) =
        if fdm.adjust {
            let nozzle_flat = fdm.nozzle_mm.min(0.25 * pitch);
            let flat = nozzle_flat.max(pitch / 8.0).max(0.08);
            let (rc, ac) =
                compute_fdm_clearances(fdm.fit, fdm.nozzle_mm, fdm.layer_mm, pitch, flat, flat);
            (45.0, 45.0, flat, flat, rc, ac)
        } else {
            let flat = (pitch / 8.0).max(0.05);
            (30.0, 30.0, flat, flat, 0.0, 0.0)
        };

    let prof = ThreadProfileParams {
        major_diameter,
        pitch,
        length,
        starts,
        left_hand,
        upper_flank_angle_deg: upper_deg,
        lower_flank_angle_deg: lower_deg,
        radial_clearance,
        axial_clearance,
        crest_flat,
        root_flat,
        crest_fillet: 0.0,
        root_fillet: 0.0,
        end_chamfer,
        chamfer_size,
    };
    let _ = validate_and_depth(&prof)?;
    Ok(prof)
}

fn fmt_deg(v: f64) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{:.0}", v.round())
    } else {
        format!("{v:.1}")
    }
}

/// Formats a concise Timeline folder label showing the active geometry & clearances
/// (fits comfortably within SolidCraft's 128-char `timeline.group` / `timeline.rename_group` limit).
pub fn format_folder_name(
    base_label: &str,
    prefix: &str,
    prof: &ThreadProfileParams,
    apply_clearance: bool,
) -> String {
    let ang = format!(
        "{}°/{}°",
        fmt_deg(prof.upper_flank_angle_deg),
        fmt_deg(prof.lower_flank_angle_deg)
    );
    let clr = if apply_clearance && (prof.radial_clearance > 1e-6 || prof.axial_clearance > 1e-6) {
        format!("R{:.2} A{:.2}", prof.radial_clearance, prof.axial_clearance)
    } else {
        "Nominal".to_string()
    };
    let mut extra = String::new();
    if prof.starts > 1 {
        extra.push_str(&format!(" · {}st", prof.starts));
    }
    if prof.left_hand {
        extra.push_str(" · LH");
    }
    let full = format!("{base_label} · {ang}{extra} · {clr} [{prefix}]");
    if full.chars().count() <= 120 {
        full
    } else {
        let mut short: String = full.chars().take(117).collect();
        short.push_str("...");
        short
    }
}

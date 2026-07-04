//! Net-name heuristics shared by the model builder, the renderer and the
//! net-expansion ("Mycelium-style") tracer.

/// Markers various formats use for "this pin is not connected".
pub fn is_no_connect(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "NC" | "N/C" | "NOCONNECT" | "NO_CONNECT" | "UNCONNECTED" | "UNUSED" | "DUMMY" | "NONE"
    )
}

/// Ground-ish nets: rendered dimmer, excluded from net expansion (expanding
/// through GND floods the whole board).
pub fn is_ground(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n == "GND" || n == "AGND" || n == "DGND" || n == "PGND" || n == "SGND" || n.starts_with("GND_")
}

/// Power-rail guess, used only for ranking/labeling (never for correctness).
pub fn is_power_hint(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("PP") // Apple convention: PPBUS_G3H, PP3V3_S5, ...
        || n.starts_with("VCC")
        || n.starts_with("VDD")
        || n.starts_with("+")
        || n.starts_with("VBAT")
        || n.starts_with("PWR")
}

/// True when a reference designator looks like a low-impedance two-terminal
/// jumper candidate: resistors (0R), inductors/beads, fuses. Net expansion
/// follows a net through such parts to the net on their other pin.
pub fn is_jumper_refdes(refdes: &str) -> bool {
    let r = refdes.trim().to_ascii_uppercase();
    // FB1 (ferrite bead), FL7310 (filter), PR/PL/PF (Apple power-rail
    // resistor/inductor/fuse prefixes), JP (solder jumper)
    if r.len() >= 3
        && matches!(&r[..2], "FB" | "FL" | "PR" | "PL" | "PF" | "JP")
        && r.as_bytes()[2].is_ascii_digit()
    {
        return true;
    }
    let mut chars = r.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let second = chars.next();
    matches!(first, 'R' | 'L' | 'F') && second.map(|c| c.is_ascii_digit()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumper_refdes_heuristic() {
        for good in ["R1234", "L7100", "F6100", "FB2", "FL7310", "PR100", "JP3"] {
            assert!(is_jumper_refdes(good), "{good} should be a jumper candidate");
        }
        for bad in ["U5300", "C1234", "Q6001", "RN12", "FOO", "J1", "", "R"] {
            assert!(!is_jumper_refdes(bad), "{bad} must not be a jumper candidate");
        }
    }

    #[test]
    fn net_classes() {
        assert!(is_no_connect("nc"));
        assert!(is_ground("GND"));
        assert!(is_power_hint("PPBUS_G3H"));
        assert!(!is_ground("PPBUS_G3H"));
    }
}

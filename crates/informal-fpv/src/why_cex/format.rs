use super::WhyCexReport;

pub(super) fn format_bv(bits: &[bool]) -> String {
    if bits.len() <= 1 {
        return if bits.first().copied().unwrap_or(false) {
            "1'b1".into()
        } else {
            "1'b0".into()
        };
    }
    if bits.len() <= 64 {
        let mut v = 0u64;
        for (i, &b) in bits.iter().enumerate() {
            if b {
                v |= 1u64 << i;
            }
        }
        let nibbles = bits.len().div_ceil(4);
        return format!("{}'h{:0width$x}", bits.len(), v, width = nibbles);
    }
    let mut s = format!("{}'b", bits.len());
    for &b in bits.iter().rev() {
        s.push(if b { '1' } else { '0' });
    }
    s
}

pub fn format_text(r: &WhyCexReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "why_cex '{}' ({}, failure at frame {}/{}, active slice {}/{} static COI signals):",
        r.property,
        r.status,
        r.failure_frame,
        r.total_frames.saturating_sub(1),
        r.active_slice_signals,
        r.static_cone_signals,
    );
    for ev in &r.events {
        match &ev.prev_value {
            Some(prev) => {
                let _ = writeln!(
                    out,
                    "  @{:<3} {:<8} {:<28} {} -> {} ({})",
                    ev.frame, ev.kind, ev.signal, prev, ev.value, ev.reason
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "  @{:<3} {:<8} {:<28} = {} ({})",
                    ev.frame, ev.kind, ev.signal, ev.value, ev.reason
                );
            }
        }
    }
    out
}

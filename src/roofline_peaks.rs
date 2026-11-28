// Included with `include!` by `cli.rs` (also compiled by `build.rs`) and `roofline.rs`,
// so the parser exists without the `cli` feature and without duplicating it.
// `--roofline-peaks`: `None` for `observed`, else `(GFLOP/s, GB/s)`, both finite and above zero.
pub fn parse_roofline_peaks(value: &str) -> Result<Option<(f64, f64)>, String> {
    if value == "observed" {
        return Ok(None);
    }
    let bad = || {
        format!(
            "'{value}' is not valid for '--roofline-peaks': use 'GFLOPS,GBPS' (e.g. '48,20') or 'observed'"
        )
    };
    let (flops, bytes) = value.split_once(',').ok_or_else(bad)?;
    let number = |t: &str| {
        t.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
    };
    Ok(Some((
        number(flops).ok_or_else(bad)?,
        number(bytes).ok_or_else(bad)?,
    )))
}

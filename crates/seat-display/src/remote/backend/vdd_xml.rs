// SPDX-License-Identifier: GPL-3.0-only
//! Minimal, comment-preserving editor for Virtual-Display-Driver's `vdd_settings.xml`
//! (upstream `Virtual Display Driver (HDR)/vdd_settings.xml`, MIT). Only the elements this
//! backend owns are read or written:
//!
//! | Path | Meaning |
//! | --- | --- |
//! | `vdd_settings/monitors/count` | number of virtual monitors (global) |
//! | `vdd_settings/gpu/friendlyname` | render GPU by DXGI description, `default` = OS choice |
//! | `vdd_settings/resolutions/resolution{width,height,refresh_rate}` | offered modes |
//! | `vdd_settings/colour/SDR10bit`, `…/HDRPlus` | 10-bit SDR / 12-bit HDR toggles |
//!
//! Upstream note: the driver reads `HKLM\SOFTWARE\MikeTheTech\VirtualDisplayDriver` values
//! first and falls back to the XML, so a registry override there beats this file.

use crate::remote::error::DisplayError;

/// The state this backend wants in the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VddDesired {
    pub count: u32,
    /// DXGI description, `None` = leave as is.
    pub gpu: Option<String>,
    pub sdr10: Option<bool>,
    pub hdr_plus: Option<bool>,
    /// Modes that must be offered (added if missing; existing ones are kept).
    pub resolutions: Vec<(u32, u32, u32)>,
}

#[derive(Debug, Clone, Copy)]
struct Elem {
    open_start: usize,
    content_start: usize,
    content_end: usize,
    close_end: usize,
}

fn comment_ranges(xml: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(s) = xml[at..].find("<!--") {
        let s = at + s;
        let e = xml[s..].find("-->").map(|e| s + e + 3).unwrap_or(xml.len());
        out.push((s, e));
        at = e;
    }
    out
}

fn in_comment(c: &[(usize, usize)], pos: usize) -> bool {
    c.iter().any(|&(s, e)| pos >= s && pos < e)
}

/// Finds the next start tag `<name>` / `<name …>` at or after `from` (outside comments).
fn find_open(
    xml: &str,
    name: &str,
    from: usize,
    end: usize,
    c: &[(usize, usize)],
) -> Option<(usize, usize)> {
    let pat = format!("<{name}");
    let mut at = from;
    while at < end {
        let p = at + xml[at..end].find(&pat)?;
        let after = p + pat.len();
        let next = xml[after..].chars().next();
        if !in_comment(c, p)
            && matches!(
                next,
                Some('>') | Some(' ') | Some('\t') | Some('\r') | Some('\n')
            )
        {
            let gt = after + xml[after..].find('>')?;
            return Some((p, gt + 1));
        }
        at = after;
    }
    None
}

fn find_elem(xml: &str, name: &str, from: usize, end: usize, c: &[(usize, usize)]) -> Option<Elem> {
    let (open_start, content_start) = find_open(xml, name, from, end, c)?;
    let close = format!("</{name}>");
    let mut depth = 1;
    let mut at = content_start;
    loop {
        let next_close = at + xml[at..end].find(&close)?;
        if in_comment(c, next_close) {
            at = next_close + close.len();
            continue;
        }
        // nested same-name elements (e.g. <logging><logging>…</logging></logging>)
        while let Some((o, oe)) = find_open(xml, name, at, next_close, c) {
            depth += 1;
            at = oe;
            let _ = o;
        }
        depth -= 1;
        if depth == 0 {
            return Some(Elem {
                open_start,
                content_start,
                content_end: next_close,
                close_end: next_close + close.len(),
            });
        }
        at = next_close + close.len();
    }
}

fn find_path(xml: &str, path: &[&str]) -> Option<Elem> {
    let c = comment_ranges(xml);
    let (mut from, mut end) = (0, xml.len());
    let mut found = None;
    for name in path {
        let e = find_elem(xml, name, from, end, &c)?;
        from = e.content_start;
        end = e.content_end;
        found = Some(e);
    }
    found
}

/// Text of the element at `path`, trimmed.
pub fn get(xml: &str, path: &[&str]) -> Option<String> {
    find_path(xml, path).map(|e| xml[e.content_start..e.content_end].trim().to_string())
}

fn escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Sets the text of `path`; creates the last element inside its parent when missing.
pub fn set(xml: &str, path: &[&str], value: &str) -> Result<String, DisplayError> {
    let v = escape(value);
    if let Some(e) = find_path(xml, path) {
        return Ok(format!(
            "{}{}{}",
            &xml[..e.content_start],
            v,
            &xml[e.content_end..]
        ));
    }
    let (parent, last) = path.split_at(path.len() - 1);
    let p = find_path(xml, parent).ok_or_else(|| {
        DisplayError::Protocol(format!("vdd_settings.xml: <{}> missing", parent.join("/")))
    })?;
    let ins = format!("    <{0}>{1}</{0}>\n", last[0], v);
    Ok(format!(
        "{}{}{}",
        &xml[..p.content_end],
        ins,
        &xml[p.content_end..]
    ))
}

/// All `<resolution>` entries as (width, height, refresh).
pub fn resolutions(xml: &str) -> Vec<(u32, u32, u32)> {
    let c = comment_ranges(xml);
    let Some(list) = find_path(xml, &["vdd_settings", "resolutions"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut at = list.content_start;
    while let Some(e) = find_elem(xml, "resolution", at, list.content_end, &c) {
        let inner = &xml[e.open_start..e.close_end];
        let num = |n: &str| {
            get(inner, &["resolution", n])
                .and_then(|s| s.parse::<f64>().ok())
                .map(|f| f.round() as u32)
        };
        if let (Some(w), Some(h), Some(r)) = (num("width"), num("height"), num("refresh_rate")) {
            out.push((w, h, r));
        }
        at = e.close_end;
    }
    out
}

/// Adds a `<resolution>` entry when the mode is not offered yet.
pub fn ensure_resolution(
    xml: &str,
    w: u32,
    h: u32,
    hz: u32,
) -> Result<(String, bool), DisplayError> {
    if resolutions(xml).contains(&(w, h, hz)) {
        return Ok((xml.to_string(), false));
    }
    let list = find_path(xml, &["vdd_settings", "resolutions"])
        .ok_or_else(|| DisplayError::Protocol("vdd_settings.xml: <resolutions> missing".into()))?;
    let entry = format!(
        "    <resolution>\n            <width>{w}</width>\n            <height>{h}</height>\n            <refresh_rate>{hz}</refresh_rate>\n        </resolution>\n    "
    );
    Ok((
        format!(
            "{}{}{}",
            &xml[..list.content_end],
            entry,
            &xml[list.content_end..]
        ),
        true,
    ))
}

fn bool_text(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// Rewrites the owned elements to the desired state.
pub fn apply(xml: &str, d: &VddDesired) -> Result<String, DisplayError> {
    if find_path(xml, &["vdd_settings"]).is_none() {
        return Err(DisplayError::Protocol(
            "vdd_settings.xml: root <vdd_settings> missing".into(),
        ));
    }
    let mut x = set(
        xml,
        &["vdd_settings", "monitors", "count"],
        &d.count.to_string(),
    )?;
    if let Some(g) = &d.gpu {
        x = set(&x, &["vdd_settings", "gpu", "friendlyname"], g)?;
    }
    if let Some(b) = d.sdr10 {
        x = set(&x, &["vdd_settings", "colour", "SDR10bit"], bool_text(b))?;
    }
    if let Some(b) = d.hdr_plus {
        x = set(&x, &["vdd_settings", "colour", "HDRPlus"], bool_text(b))?;
    }
    for &(w, h, hz) in &d.resolutions {
        x = ensure_resolution(&x, w, h, hz)?.0;
    }
    Ok(x)
}

/// Does the file already hold the desired state? (drift check, OQ 19)
pub fn matches(xml: &str, d: &VddDesired) -> bool {
    let eq = |p: &[&str], v: &str| get(xml, p).is_some_and(|s| s.eq_ignore_ascii_case(v));
    if !eq(&["vdd_settings", "monitors", "count"], &d.count.to_string()) {
        return false;
    }
    if let Some(g) = &d.gpu
        && get(xml, &["vdd_settings", "gpu", "friendlyname"]).as_deref() != Some(escape(g).as_str())
    {
        return false;
    }
    if let Some(b) = d.sdr10
        && !eq(&["vdd_settings", "colour", "SDR10bit"], bool_text(b))
    {
        return false;
    }
    if let Some(b) = d.hdr_plus
        && !eq(&["vdd_settings", "colour", "HDRPlus"], bool_text(b))
    {
        return false;
    }
    let have = resolutions(xml);
    d.resolutions.iter().all(|r| have.contains(r))
}

#[cfg(test)]
pub(crate) const SAMPLE: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<!-- header comment with <count>9</count> inside -->
<vdd_settings>
    <monitors>
        <count>1</count>
    </monitors>
    <gpu>
        <friendlyname>default</friendlyname>
    </gpu>
    <resolutions>
        <resolution>
            <width>1920</width>
            <height>1080</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
    </resolutions>
    <logging>
        <SendLogsThroughPipe>true</SendLogsThroughPipe>
        <logging>false</logging>
    </logging>
    <colour>
        <SDR10bit>false</SDR10bit>
        <HDRPlus>false</HDRPlus>
        <ColourFormat>RGB</ColourFormat> <!-- Options: RGB -->
    </colour>
</vdd_settings>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_values_ignoring_comments() {
        assert_eq!(
            get(SAMPLE, &["vdd_settings", "monitors", "count"]).as_deref(),
            Some("1")
        );
        assert_eq!(
            get(SAMPLE, &["vdd_settings", "gpu", "friendlyname"]).as_deref(),
            Some("default")
        );
        assert_eq!(
            get(SAMPLE, &["vdd_settings", "logging", "logging"]).as_deref(),
            Some("false")
        );
        assert_eq!(resolutions(SAMPLE), vec![(1920, 1080, 60)]);
    }

    #[test]
    fn apply_and_matches() {
        let d = VddDesired {
            count: 3,
            gpu: Some("GPU <A&B>".into()),
            sdr10: Some(true),
            hdr_plus: None,
            resolutions: vec![(1920, 1080, 60), (2560, 1440, 144)],
        };
        assert!(!matches(SAMPLE, &d));
        let x = apply(SAMPLE, &d).unwrap();
        assert!(matches(&x, &d));
        assert_eq!(
            get(&x, &["vdd_settings", "monitors", "count"]).as_deref(),
            Some("3")
        );
        assert!(x.contains("<friendlyname>GPU &lt;A&amp;B&gt;</friendlyname>"));
        assert_eq!(resolutions(&x), vec![(1920, 1080, 60), (2560, 1440, 144)]);
        assert!(x.contains("<!-- header comment with <count>9</count> inside -->"));
        assert!(x.contains("<ColourFormat>RGB</ColourFormat> <!-- Options: RGB -->"));
        // idempotent
        assert_eq!(apply(&x, &d).unwrap(), x);
    }

    #[test]
    fn missing_leaf_is_created_missing_root_fails() {
        let x = set(
            "<vdd_settings><colour></colour></vdd_settings>",
            &["vdd_settings", "colour", "HDRPlus"],
            "true",
        )
        .unwrap();
        assert_eq!(
            get(&x, &["vdd_settings", "colour", "HDRPlus"]).as_deref(),
            Some("true")
        );
        assert!(apply("<other/>", &VddDesired::default()).is_err());
        assert!(
            set(
                "<vdd_settings></vdd_settings>",
                &["vdd_settings", "colour", "HDRPlus"],
                "x"
            )
            .is_err()
        );
    }
}

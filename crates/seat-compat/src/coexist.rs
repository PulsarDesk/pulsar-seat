// SPDX-License-Identifier: GPL-3.0-only
//! Coexistence detection (spec 90 §11 item 4, spec 00 §3.3, spec 50 §4 item 6).
//!
//! - **Reference product**: its service keys ([`REFERENCE_SERVICE_KEYS`]) under
//!   `HKLM\SYSTEM\CurrentControlSet\Services`. Install is **refused** while they exist.
//!   The names are interop identifiers used only here.
//! - **TermService**: a non-default `Parameters\ServiceDll` means another product wrapped
//!   the terminal service. The RDS loopback backend is refused there; the installer warns.
//! - **Foreign filters** in the keyboard / mouse / HID class lists and on device stacks:
//!   reported as a warning, **never edited** (no class-wide edits by Pulsar at all).
//!
//! Read-only: nothing in this module writes.

use crate::ops::{Hive, SystemOps};
use seat_core::consts::REFERENCE_SERVICE_KEYS;
use seat_core::pnp::{PnpTree, foreign_filters};
use seat_proto::messages::{Check, CheckOutcome};
use serde::{Deserialize, Serialize};

pub const SERVICES_KEY: &str = r"SYSTEM\CurrentControlSet\Services";
pub const TERMSERVICE_PARAMS: &str = r"SYSTEM\CurrentControlSet\Services\TermService\Parameters";
pub const CLASS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Class";

/// Setup classes whose class filter lists we inspect, with the Windows default upper
/// filter of each.
pub const INSPECTED_CLASSES: [(&str, &str, &[&str]); 3] = [
    (
        "Keyboard",
        "4d36e96b-e325-11ce-bfc1-08002be10318",
        &["kbdclass"],
    ),
    (
        "Mouse",
        "4d36e96f-e325-11ce-bfc1-08002be10318",
        &["mouclass"],
    ),
    ("HIDClass", "745a17a0-74d3-11d0-b6fe-00a0c90f57da", &[]),
];

/// Service names of our own filter drivers (never reported as foreign). Must match the
/// `AddService` / `AddFilter` names in `drivers/seat-input-filter/seat-input-filter.inf`
/// (one KMDF filter serves keyboards, mice and HID collections); the test
/// `our_filter_names_match_inf` parses that INF.
pub const OUR_FILTER_SERVICES: [&str; 1] = [seat_core::consts::INPUT_FILTER_SERVICE];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterPosition {
    Upper,
    Lower,
}

/// Non-default entries in a class filter list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassFilters {
    pub class_name: String,
    pub class_guid: String,
    pub position: FilterPosition,
    pub filters: Vec<String>,
}

/// Foreign filters on one device stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFilters {
    pub instance_id: String,
    pub filters: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coexistence {
    /// Reference product service keys found.
    pub reference_services: Vec<String>,
    /// `TermService\Parameters\ServiceDll` as stored (`None` = value absent).
    pub termservice_dll: Option<String>,
    pub termservice_stock: bool,
    pub class_filters: Vec<ClassFilters>,
    pub device_filters: Vec<DeviceFilters>,
}

impl Coexistence {
    /// The installer refuses to install / upgrade.
    pub fn blocks_install(&self) -> bool {
        !self.reference_services.is_empty()
    }

    /// The RDS loopback session backend must not run here.
    pub fn blocks_rds_loopback(&self) -> bool {
        !self.termservice_stock
    }

    pub fn checks(&self) -> Vec<Check> {
        let mut v = Vec::new();
        v.push(if self.reference_services.is_empty() {
            Check {
                id: "reference_product".into(),
                outcome: CheckOutcome::Pass,
                detail: "not installed".into(),
            }
        } else {
            Check {
                id: "reference_product".into(),
                outcome: CheckOutcome::Fail,
                detail: format!(
                    "another multiseat product is installed (service keys: {})",
                    self.reference_services.join(", ")
                ),
            }
        });
        v.push(if self.termservice_stock {
            Check {
                id: "termservice_stock".into(),
                outcome: CheckOutcome::Pass,
                detail: "TermService uses Windows' termsrv.dll".into(),
            }
        } else {
            Check {
                id: "termservice_stock".into(),
                outcome: CheckOutcome::Warn,
                detail: format!(
                    "the TermService ServiceDll value is not the Windows default: {}",
                    self.termservice_dll.as_deref().unwrap_or("(none)")
                ),
            }
        });
        let mut foreign: Vec<String> = self
            .class_filters
            .iter()
            .map(|c| {
                format!(
                    "{} ({}): {}",
                    c.class_name,
                    match c.position {
                        FilterPosition::Upper => "upper",
                        FilterPosition::Lower => "lower",
                    },
                    c.filters.join(", ")
                )
            })
            .collect();
        foreign.extend(
            self.device_filters
                .iter()
                .map(|d| format!("{}: {}", d.instance_id, d.filters.join(", "))),
        );
        v.push(if foreign.is_empty() {
            Check {
                id: "foreign_input_filters".into(),
                outcome: CheckOutcome::Pass,
                detail: "none".into(),
            }
        } else {
            Check {
                id: "foreign_input_filters".into(),
                outcome: CheckOutcome::Warn,
                detail: format!(
                    "other input filters present (left untouched): {}",
                    foreign.join("; ")
                ),
            }
        });
        v
    }
}

/// Is a `ServiceDll` value the stock `%SystemRoot%\System32\termsrv.dll`?
/// `system_root` is the expanded `%SystemRoot%` (for example `C:\Windows`).
pub fn is_stock_termservice_dll(value: &str, system_root: &str) -> bool {
    let v = value
        .trim()
        .trim_matches('"')
        .replace('/', "\\")
        .to_ascii_lowercase();
    let root = system_root.trim_end_matches('\\').to_ascii_lowercase();
    let expanded = v.replace("%systemroot%", &root).replace("%windir%", &root);
    expanded == format!("{root}\\system32\\termsrv.dll")
}

/// Detect everything. `system_root` defaults to the `SystemRoot` environment variable.
/// `tree` + `instance_ids`: device stacks to inspect (input devices of the inventory).
pub fn detect(ops: &dyn SystemOps, tree: Option<(&dyn PnpTree, &[String])>) -> Coexistence {
    let mut c = Coexistence::default();
    for name in REFERENCE_SERVICE_KEYS {
        if ops
            .reg_key_exists(Hive::Hklm, &format!(r"{SERVICES_KEY}\{name}"))
            .unwrap_or(false)
        {
            c.reference_services.push(name.to_string());
        }
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    c.termservice_dll = ops
        .reg_read(Hive::Hklm, TERMSERVICE_PARAMS, "ServiceDll")
        .ok()
        .flatten()
        .and_then(|d| d.as_str().map(str::to_string));
    // Absent value: TermService not installed in the usual way -> not stock.
    c.termservice_stock = c
        .termservice_dll
        .as_deref()
        .is_some_and(|d| is_stock_termservice_dll(d, &root));
    for (name, guid, defaults) in INSPECTED_CLASSES {
        let key = format!(r"{CLASS_KEY}\{{{guid}}}");
        for (pos, val) in [
            (FilterPosition::Upper, "UpperFilters"),
            (FilterPosition::Lower, "LowerFilters"),
        ] {
            let list = ops
                .reg_read(Hive::Hklm, &key, val)
                .ok()
                .flatten()
                .map(|d| d.as_multi())
                .unwrap_or_default();
            let foreign: Vec<String> = list
                .into_iter()
                .filter(|f| !f.trim().is_empty())
                .filter(|f| {
                    pos == FilterPosition::Lower
                        || !defaults.iter().any(|d| d.eq_ignore_ascii_case(f))
                })
                .filter(|f| {
                    !OUR_FILTER_SERVICES
                        .iter()
                        .any(|o| o.eq_ignore_ascii_case(f))
                })
                .collect();
            if !foreign.is_empty() {
                c.class_filters.push(ClassFilters {
                    class_name: name.into(),
                    class_guid: guid.into(),
                    position: pos,
                    filters: foreign,
                });
            }
        }
    }
    if let Some((tree, ids)) = tree {
        for id in ids {
            if let Some(n) = tree.node(id) {
                let f = foreign_filters(&n, &OUR_FILTER_SERVICES);
                if !f.is_empty() {
                    c.device_filters.push(DeviceFilters {
                        instance_id: id.clone(),
                        filters: f,
                    });
                }
            }
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{MemoryOps, RegData};

    #[test]
    fn stock_termsrv_forms() {
        assert!(is_stock_termservice_dll(
            r"%SystemRoot%\System32\termsrv.dll",
            r"C:\Windows"
        ));
        assert!(is_stock_termservice_dll(
            r"C:\WINDOWS\system32\TermSrv.dll",
            r"C:\Windows\"
        ));
        assert!(!is_stock_termservice_dll(
            r"%ProgramFiles%\Wrap\wrapper.dll",
            r"C:\Windows"
        ));
        assert!(!is_stock_termservice_dll(
            r"C:\Windows\System32\termsrv_patched.dll",
            r"C:\Windows"
        ));
    }

    #[test]
    fn detects_reference_and_wrapper_and_filters() {
        let ops = MemoryOps::new();
        let c = detect(&ops, None);
        assert!(!c.blocks_install());
        assert!(c.blocks_rds_loopback(), "absent ServiceDll is not stock");

        ops.add_key(
            Hive::Hklm,
            &format!(r"{SERVICES_KEY}\{}", REFERENCE_SERVICE_KEYS[0]),
        );
        ops.set(
            Hive::Hklm,
            TERMSERVICE_PARAMS,
            "ServiceDll",
            RegData::ExpandSz(r"%SystemRoot%\System32\termsrv.dll".into()),
        );
        let kb = format!(r"{CLASS_KEY}\{{{}}}", INSPECTED_CLASSES[0].1);
        ops.set(
            Hive::Hklm,
            &kb,
            "UpperFilters",
            RegData::MultiSz(vec![
                "kbdclass".into(),
                "VendorKbd".into(),
                "PulsarSeatInput".into(),
            ]),
        );
        let c = detect(&ops, None);
        assert!(c.blocks_install());
        assert!(!c.blocks_rds_loopback());
        assert_eq!(c.class_filters.len(), 1);
        assert_eq!(c.class_filters[0].filters, vec!["VendorKbd".to_string()]);
        let checks = c.checks();
        assert_eq!(
            checks
                .iter()
                .find(|x| x.id == "reference_product")
                .unwrap()
                .outcome,
            CheckOutcome::Fail
        );
        assert_eq!(
            checks
                .iter()
                .find(|x| x.id == "foreign_input_filters")
                .unwrap()
                .outcome,
            CheckOutcome::Warn
        );
    }

    #[test]
    fn device_stack_filters_via_tree() {
        use seat_core::mock::MockPnpTree;
        use seat_core::pnp::PnpNode;
        let tree = MockPnpTree::default();
        tree.add(PnpNode {
            instance_id: "HID\\X\\1".into(),
            upper_filters: vec!["kbdclass".into(), "OtherFlt".into()],
            ..Default::default()
        });
        let ops = MemoryOps::new();
        let ids = vec!["HID\\X\\1".to_string()];
        let c = detect(&ops, Some((&tree, &ids)));
        assert_eq!(c.device_filters[0].filters, vec!["OtherFlt".to_string()]);
    }

    /// Every name in [`OUR_FILTER_SERVICES`] is an `AddService` (and `AddFilter`) name of
    /// our input-filter INF, so our own filter is never reported as foreign.
    #[test]
    fn our_filter_names_match_inf() {
        let inf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../drivers/seat-input-filter/seat-input-filter.inf");
        let Ok(bytes) = std::fs::read(&inf) else {
            panic!("missing {}", inf.display());
        };
        let text = crate::drivers::decode_inf(&bytes);
        let names = |directive: &str| -> Vec<String> {
            text.lines()
                .filter_map(|l| {
                    let l = l.split(';').next().unwrap_or("").trim();
                    let (k, v) = l.split_once('=')?;
                    k.trim()
                        .eq_ignore_ascii_case(directive)
                        .then(|| v.split(',').next().unwrap_or("").trim().to_string())
                })
                .collect()
        };
        let services = names("AddService");
        let filters = names("AddFilter");
        for n in OUR_FILTER_SERVICES {
            assert!(
                services.iter().any(|s| s.eq_ignore_ascii_case(n)),
                "{n} not in AddService {services:?}"
            );
            assert!(
                filters.iter().any(|s| s.eq_ignore_ascii_case(n)),
                "{n} not in AddFilter {filters:?}"
            );
        }
    }
}

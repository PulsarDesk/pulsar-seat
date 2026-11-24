// SPDX-License-Identifier: GPL-3.0-only
//! Coexistence detection (spec 90 §11 item 4, spec 00 §3.3, spec 50 §4 item 6).
//!
//! - **Reference product**: its service keys
//!   ([`seat_core::consts::REFERENCE_SERVICE_KEYS`]) under
//!   `HKLM\SYSTEM\CurrentControlSet\Services`. Install is **refused** while they exist,
//!   except in the disabled state ([`seat_core::reference`], 2026-11-24): every existing
//!   key has `Start` = 4 and the service control manager reports none of them running /
//!   loaded. Then the check is a warning and install proceeds. The names are interop
//!   identifiers used only here; their keys are read, never written.
//! - **TermService**: a non-default `Parameters\ServiceDll` means another product wrapped
//!   the terminal service. The RDS loopback backend is refused there; the installer warns.
//! - **Foreign filters** in the keyboard / mouse / HID class lists and on device stacks:
//!   reported as a warning, **never edited** (no class-wide edits by Pulsar at all).
//!
//! Read-only: nothing in this module writes.

use crate::ops::{Hive, SystemOps};
use seat_core::pnp::{PnpTree, foreign_filters};
use seat_core::reference::{self, ReferenceKey, ReferenceState};
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
    /// `Start` and SCM state of each key in `reference_services`. Added 2026-11-24.
    #[serde(default)]
    pub reference_keys: Vec<ReferenceKey>,
    /// Every key in `reference_services` has `Start` = 4 and is not running / loaded
    /// ([`ReferenceState::Disabled`]). Added 2026-11-24.
    #[serde(default)]
    pub reference_disabled: bool,
    /// `TermService\Parameters\ServiceDll` as stored (`None` = value absent).
    pub termservice_dll: Option<String>,
    pub termservice_stock: bool,
    pub class_filters: Vec<ClassFilters>,
    pub device_filters: Vec<DeviceFilters>,
}

impl Coexistence {
    pub fn reference_state(&self) -> ReferenceState {
        if self.reference_services.is_empty() {
            ReferenceState::Absent
        } else if self.reference_disabled {
            ReferenceState::Disabled
        } else {
            ReferenceState::Active
        }
    }

    /// The installer refuses to install / upgrade (reference product present and not
    /// fully disabled).
    pub fn blocks_install(&self) -> bool {
        self.reference_state().blocks()
    }

    /// The RDS loopback session backend must not run here.
    pub fn blocks_rds_loopback(&self) -> bool {
        !self.termservice_stock
    }

    pub fn checks(&self) -> Vec<Check> {
        let mut v = Vec::new();
        let keys = if self.reference_keys.is_empty() {
            self.reference_services.join(", ")
        } else {
            reference::describe(&self.reference_keys)
        };
        v.push(match self.reference_state() {
            ReferenceState::Absent => Check {
                id: "reference_product".into(),
                outcome: CheckOutcome::Pass,
                detail: "not installed".into(),
            },
            ReferenceState::Disabled => Check {
                id: "reference_product".into(),
                outcome: CheckOutcome::Warn,
                detail: format!(
                    "{}: another multiseat product is installed but disabled (service keys: {keys}); keep it disabled while Pulsar Seat is installed",
                    reference::REFERENCE_PRODUCT_DISABLED
                ),
            },
            ReferenceState::Active => Check {
                id: "reference_product".into(),
                outcome: CheckOutcome::Fail,
                detail: format!(
                    "another multiseat product is installed (service keys: {keys})"
                ),
            },
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
    // Read-only: key existence, `Start`, SCM state. Never a write to these keys.
    c.reference_keys = reference::probe(
        |name| {
            Some(
                ops.reg_key_exists(Hive::Hklm, &format!(r"{SERVICES_KEY}\{name}"))
                    .unwrap_or(false),
            )
        },
        |name| {
            ops.reg_read(Hive::Hklm, &format!(r"{SERVICES_KEY}\{name}"), "Start")
                .ok()
                .flatten()
                .and_then(|d| d.as_u32())
        },
        |name| ops.service_active(name).ok(),
    );
    c.reference_services = c.reference_keys.iter().map(|k| k.name.clone()).collect();
    c.reference_disabled = reference::classify(&c.reference_keys) == ReferenceState::Disabled;
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
    use seat_core::consts::REFERENCE_SERVICE_KEYS;

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

    /// Both reference keys present with the given `Start` values (`None` = no value).
    fn reference_ops(starts: [Option<u32>; 2]) -> MemoryOps {
        let ops = MemoryOps::new();
        for (name, start) in REFERENCE_SERVICE_KEYS.iter().zip(starts) {
            let key = format!(r"{SERVICES_KEY}\{name}");
            ops.add_key(Hive::Hklm, &key);
            if let Some(s) = start {
                ops.set(Hive::Hklm, &key, "Start", RegData::Dword(s));
            }
        }
        ops
    }

    fn reference_check(c: &Coexistence) -> Check {
        c.checks().into_iter().find(|x| x.id == "reference_product").unwrap()
    }

    #[test]
    fn reference_fully_disabled_warns_instead_of_blocking() {
        let ops = reference_ops([Some(4), Some(4)]);
        let before = ops.reg.lock().unwrap().clone();
        let c = detect(&ops, None);
        assert_eq!(c.reference_state(), ReferenceState::Disabled);
        assert!(c.reference_disabled);
        assert_eq!(c.reference_services.len(), REFERENCE_SERVICE_KEYS.len());
        assert!(!c.blocks_install());
        let chk = reference_check(&c);
        assert_eq!(chk.outcome, CheckOutcome::Warn);
        assert!(chk.detail.starts_with("reference_product_disabled:"), "{}", chk.detail);
        assert!(chk.detail.contains("start 4, stopped"), "{}", chk.detail);
        // Pure detection: nothing written to the reference keys (or anywhere).
        assert_eq!(*ops.reg.lock().unwrap(), before);
        // Only one key left, disabled and stopped: still the disabled state.
        let ops = MemoryOps::new();
        let key = format!(r"{SERVICES_KEY}\{}", REFERENCE_SERVICE_KEYS[1]);
        ops.set(Hive::Hklm, &key, "Start", RegData::Dword(4));
        assert_eq!(detect(&ops, None).reference_state(), ReferenceState::Disabled);
    }

    #[test]
    fn reference_not_fully_disabled_still_blocks() {
        for starts in [[Some(4), Some(3)], [Some(2), Some(4)], [Some(4), Some(1)], [Some(4), None], [None, None]] {
            let c = detect(&reference_ops(starts), None);
            assert_eq!(c.reference_state(), ReferenceState::Active, "{starts:?}");
            assert!(c.blocks_install(), "{starts:?}");
            assert_eq!(reference_check(&c).outcome, CheckOutcome::Fail, "{starts:?}");
        }
        // Disabled in the registry but still running / loaded (a driver stays loaded
        // until the next restart).
        for (i, name) in REFERENCE_SERVICE_KEYS.iter().enumerate() {
            let ops = reference_ops([Some(4), Some(4)]);
            ops.active_services.lock().unwrap().insert(name.to_ascii_lowercase());
            let c = detect(&ops, None);
            assert!(c.blocks_install(), "{i}");
            assert!(c.reference_keys[i].active == Some(true));
            assert!(reference_check(&c).detail.contains("running"));
        }
        // SCM state unknown: fail closed.
        let ops = reference_ops([Some(4), Some(4)]);
        ops.scm_unknown.lock().unwrap().insert(REFERENCE_SERVICE_KEYS[0].to_ascii_lowercase());
        assert!(detect(&ops, None).blocks_install());
    }

    #[test]
    fn coexistence_without_new_fields_deserializes_as_active() {
        // A snapshot from before 2026-11-24 has no reference_keys / reference_disabled.
        let c: Coexistence = serde_json::from_str(
            r#"{"reference_services":["X"],"termservice_dll":null,"termservice_stock":true,"class_filters":[],"device_filters":[]}"#,
        )
        .unwrap();
        assert!(c.blocks_install());
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

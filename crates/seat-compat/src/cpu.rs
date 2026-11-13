// SPDX-License-Identifier: GPL-3.0-only
//! CPU cores per seat (spec 80 §4.1, §11.2, §10).
//!
//! Stored per seat as a list of **CPU-set ids** (`SeatConfig::cpu_sets`, empty = all),
//! never as a raw bit string. Soft limit = the process's default CPU sets; hard limit =
//! job group affinity (see [`crate::launcher`]). Both apply only to processes our seat
//! launcher starts — we never open arbitrary processes of a session (anti-cheat, PPL).
//!
//! Hybrid CPUs (most ARM64 SoCs, recent x64): every core shows its efficiency class and
//! the suggested split mixes classes evenly.

use seat_core::SeatNumber;
use seat_proto::messages::CpuSetInfo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuIssueCode {
    /// An id that does not exist on this PC (CPU changed); it is ignored.
    UnknownId,
    /// After dropping unknown ids nothing is left: the seat would have no core.
    NoCore,
    /// Hybrid CPU and the seat got only the lowest efficiency class.
    OnlyEfficiencyCores,
    /// Hard limit across processor groups: needs one GROUP_AFFINITY per group (supported
    /// by the job API; flagged so the manager can explain it).
    HardLimitMultiGroup,
    /// Two seats share a core (allowed; informational).
    SharedCore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuIssue {
    pub seat: SeatNumber,
    pub code: CpuIssueCode,
    pub ids: Vec<u32>,
    /// Blocks saving.
    pub error: bool,
}

/// One seat's selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatCpu<'a> {
    pub seat: SeatNumber,
    pub cpu_sets: &'a [u32],
    pub hard: bool,
}

/// Lowest efficiency class present (0 on hybrid parts, the only class otherwise).
fn min_class(infos: &[CpuSetInfo]) -> Option<u8> {
    infos.iter().map(|i| i.efficiency_class).min()
}

pub fn is_hybrid(infos: &[CpuSetInfo]) -> bool {
    let mut it = infos.iter().map(|i| i.efficiency_class);
    match it.next() {
        Some(f) => it.any(|c| c != f),
        None => false,
    }
}

/// Validate every seat's selection against the CPU sets of this PC.
pub fn validate_sets(seats: &[SeatCpu<'_>], infos: &[CpuSetInfo]) -> Vec<CpuIssue> {
    let mut out = Vec::new();
    let by_id: BTreeMap<u32, &CpuSetInfo> = infos.iter().map(|i| (i.id, i)).collect();
    let hybrid = is_hybrid(infos);
    let low = min_class(infos);
    let mut owners: BTreeMap<u32, Vec<SeatNumber>> = BTreeMap::new();
    for s in seats {
        if s.cpu_sets.is_empty() {
            continue; // all cores
        }
        let unknown: Vec<u32> = s
            .cpu_sets
            .iter()
            .copied()
            .filter(|id| !by_id.contains_key(id))
            .collect();
        if !unknown.is_empty() {
            out.push(CpuIssue {
                seat: s.seat,
                code: CpuIssueCode::UnknownId,
                ids: unknown,
                error: false,
            });
        }
        let known: Vec<&CpuSetInfo> = s
            .cpu_sets
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        if known.is_empty() {
            out.push(CpuIssue {
                seat: s.seat,
                code: CpuIssueCode::NoCore,
                ids: Vec::new(),
                error: true,
            });
            continue;
        }
        if hybrid && known.iter().all(|i| Some(i.efficiency_class) == low) {
            out.push(CpuIssue {
                seat: s.seat,
                code: CpuIssueCode::OnlyEfficiencyCores,
                ids: known.iter().map(|i| i.id).collect(),
                error: false,
            });
        }
        if s.hard {
            let mut groups: Vec<u16> = known.iter().map(|i| i.group).collect();
            groups.sort();
            groups.dedup();
            if groups.len() > 1 {
                out.push(CpuIssue {
                    seat: s.seat,
                    code: CpuIssueCode::HardLimitMultiGroup,
                    ids: Vec::new(),
                    error: false,
                });
            }
        }
        for i in known {
            owners.entry(i.id).or_default().push(s.seat);
        }
    }
    for (id, seats) in owners {
        if seats.len() > 1 {
            for s in seats {
                if let Some(x) = out
                    .iter_mut()
                    .find(|x| x.seat == s && x.code == CpuIssueCode::SharedCore)
                {
                    x.ids.push(id);
                } else {
                    out.push(CpuIssue {
                        seat: s,
                        code: CpuIssueCode::SharedCore,
                        ids: vec![id],
                        error: false,
                    });
                }
            }
        }
    }
    out
}

/// Balanced split of the PC's cores over `seats` seats: SMT siblings stay together, each
/// efficiency class is dealt round-robin so every seat gets a similar mix. With fewer
/// cores than seats, cores are shared.
pub fn suggest_split(seats: usize, infos: &[CpuSetInfo]) -> Vec<Vec<u32>> {
    if seats == 0 {
        return Vec::new();
    }
    // Physical cores: (group, core_index) -> ids.
    let mut cores: BTreeMap<(u8, u16, u8), Vec<u32>> = BTreeMap::new();
    for i in infos {
        // Sort key puts faster classes first.
        cores
            .entry((u8::MAX - i.efficiency_class, i.group, i.core_index))
            .or_default()
            .push(i.id);
    }
    let mut out = vec![Vec::new(); seats];
    let list: Vec<Vec<u32>> = cores.into_values().collect();
    if list.is_empty() {
        return out;
    }
    if list.len() < seats {
        for (s, slot) in out.iter_mut().enumerate() {
            slot.extend(&list[s % list.len()]);
        }
    } else {
        // Faster classes come first in `list`; one rotation across all classes spreads
        // each class evenly and does not give every class's leftover to seat 1.
        for (n, ids) in list.into_iter().enumerate() {
            out[n % seats].extend(ids);
        }
    }
    for v in &mut out {
        v.sort();
    }
    out
}

/// Group affinities for a hard limit: `(group, mask)` per processor group.
pub fn group_affinities(ids: &[u32], infos: &[CpuSetInfo]) -> Vec<(u16, u64)> {
    let mut m: BTreeMap<u16, u64> = BTreeMap::new();
    for id in ids {
        if let Some(i) = infos.iter().find(|i| i.id == *id)
            && i.logical_index < 64
        {
            *m.entry(i.group).or_default() |= 1u64 << i.logical_index;
        }
    }
    m.into_iter().collect()
}

/// Diagnostic-report form: one binary digit per logical processor, processor 0
/// **right-most** (our convention; the reference's order is an open question).
pub fn report_string(ids: &[u32], infos: &[CpuSetInfo]) -> String {
    let mut all: Vec<&CpuSetInfo> = infos.iter().collect();
    all.sort_by_key(|i| (i.group, i.logical_index));
    all.iter()
        .rev()
        .map(|i| {
            if ids.is_empty() || ids.contains(&i.id) {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

/// CPU sets of this PC (`GetSystemCpuSetInformation`).
#[cfg(windows)]
pub fn list_cpu_sets() -> Result<Vec<CpuSetInfo>, crate::ops::OpsError> {
    crate::win::cpu_sets()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(id: u32, core: u8, class: u8) -> CpuSetInfo {
        CpuSetInfo {
            id,
            group: 0,
            logical_index: (id - 256) as u8,
            core_index: core,
            efficiency_class: class,
            numa_node: 0,
        }
    }

    fn hybrid() -> Vec<CpuSetInfo> {
        // 2 performance cores with SMT, 4 efficiency cores.
        vec![
            cpu(256, 0, 1),
            cpu(257, 0, 1),
            cpu(258, 1, 1),
            cpu(259, 1, 1),
            cpu(260, 2, 0),
            cpu(261, 3, 0),
            cpu(262, 4, 0),
            cpu(263, 5, 0),
        ]
    }

    fn s(n: u8) -> SeatNumber {
        SeatNumber::new(n).unwrap()
    }

    #[test]
    fn split_mixes_classes() {
        let infos = hybrid();
        let sp = suggest_split(2, &infos);
        assert_eq!(sp[0], vec![256, 257, 260, 262]);
        assert_eq!(sp[1], vec![258, 259, 261, 263]);
        let v = validate_sets(
            &[
                SeatCpu {
                    seat: s(1),
                    cpu_sets: &sp[0],
                    hard: false,
                },
                SeatCpu {
                    seat: s(2),
                    cpu_sets: &sp[1],
                    hard: true,
                },
            ],
            &infos,
        );
        assert!(v.is_empty(), "{v:?}");
        // More seats than cores: shared.
        assert_eq!(suggest_split(8, &infos[..2]).len(), 8);
    }

    #[test]
    fn validation_codes() {
        let infos = hybrid();
        let only_e = [260, 261];
        let gone = [999];
        let v = validate_sets(
            &[
                SeatCpu {
                    seat: s(1),
                    cpu_sets: &only_e,
                    hard: false,
                },
                SeatCpu {
                    seat: s(2),
                    cpu_sets: &gone,
                    hard: false,
                },
                SeatCpu {
                    seat: s(3),
                    cpu_sets: &[260],
                    hard: false,
                },
            ],
            &infos,
        );
        let codes: Vec<_> = v.iter().map(|x| (x.seat.get(), x.code, x.error)).collect();
        assert!(codes.contains(&(1, CpuIssueCode::OnlyEfficiencyCores, false)));
        assert!(codes.contains(&(2, CpuIssueCode::UnknownId, false)));
        assert!(codes.contains(&(2, CpuIssueCode::NoCore, true)));
        assert!(codes.contains(&(1, CpuIssueCode::SharedCore, false)));
        assert!(codes.contains(&(3, CpuIssueCode::SharedCore, false)));
    }

    #[test]
    fn affinity_and_report() {
        let mut infos = hybrid();
        infos.push(CpuSetInfo {
            id: 300,
            group: 1,
            logical_index: 3,
            core_index: 0,
            efficiency_class: 1,
            numa_node: 1,
        });
        assert_eq!(
            group_affinities(&[256, 258, 300], &infos),
            vec![(0, 0b101), (1, 0b1000)]
        );
        assert_eq!(report_string(&[], &infos[..8]), "11111111");
        assert_eq!(report_string(&[256, 257], &infos[..8]), "00000011");
        let v = validate_sets(
            &[SeatCpu {
                seat: s(2),
                cpu_sets: &[256, 300],
                hard: true,
            }],
            &infos,
        );
        assert!(
            v.iter()
                .any(|x| x.code == CpuIssueCode::HardLimitMultiGroup)
        );
    }
}

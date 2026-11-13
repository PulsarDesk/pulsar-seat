// SPDX-License-Identifier: GPL-3.0-only
//! AppHelp custom database content (spec 50 §2.2, §2.5), as a tree. The file itself is
//! written by apphelp's own writer API (`SdbCreateDatabase`, `SdbBeginWriteListTag`, …;
//! see [`crate::win::AppHelpSdbWriter`]), so we never hand-encode the binary format.
//!
//! Tag ids are interop identifiers of the shim-database format as published in open
//! sources (ReactOS `sdbtagid.h`, Microsoft Learn "Shim database tags"). **Not verified
//! against a live apphelp on this dev box**: no AppHelp install was tested (see README).

use crate::error::ShimError;

/// Tag ids. High nibble = type (1 null, 4 dword, 6 stringref, 7 list, 9 binary).
pub mod tag {
    pub const DATABASE: u16 = 0x7001;
    pub const LIBRARY: u16 = 0x7002;
    pub const INEXCLUDE: u16 = 0x7003;
    pub const SHIM: u16 = 0x7004;
    pub const EXE: u16 = 0x7007;
    pub const MATCHING_FILE: u16 = 0x7008;
    pub const SHIM_REF: u16 = 0x7009;
    pub const LAYER: u16 = 0x700B;
    pub const INCLUDE: u16 = 0x1001;
    pub const OS_PLATFORM: u16 = 0x4023;
    pub const NAME: u16 = 0x6001;
    pub const MODULE: u16 = 0x6003;
    pub const APP_NAME: u16 = 0x6006;
    pub const DLLFILE: u16 = 0x600A;
    pub const PRODUCT_NAME: u16 = 0x6010;
    pub const FILE_DESCRIPTION: u16 = 0x6012;
    pub const EXE_ID: u16 = 0x9004;
    pub const DATABASE_ID: u16 = 0x9007;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdbNode {
    List(u16, Vec<SdbNode>),
    Str(u16, String),
    Dword(u16, u32),
    Binary(u16, Vec<u8>),
    Null(u16),
}

/// A target with the version strings read from the real file when the database is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    pub exe_name: String,
    pub product_name: Option<String>,
    pub file_description: Option<String>,
    pub exe_id: [u8; 16],
}

/// `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` -> 16 bytes in Windows `GUID` memory order.
pub fn guid_bytes(s: &str) -> Result<[u8; 16], ShimError> {
    let h: String = s.trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if h.len() != 32 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ShimError::InvalidValue(format!("not a GUID: {s}")));
    }
    let b = |i: usize| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0);
    let mut out = [0u8; 16];
    // Data1 (u32 LE), Data2 (u16 LE), Data3 (u16 LE), Data4 (8 bytes as written).
    out[0..4].copy_from_slice(&[b(3), b(2), b(1), b(0)]);
    out[4..6].copy_from_slice(&[b(5), b(4)]);
    out[6..8].copy_from_slice(&[b(7), b(6)]);
    for (i, o) in out.iter_mut().enumerate().skip(8) {
        *o = b(i);
    }
    Ok(out)
}

/// OS_PLATFORM value carried by the reference database (spec 50 §2.2). HYPOTHESIS: a
/// bitmask of x86 (1) and amd64 (4) so one database serves both module builds.
pub const OS_PLATFORM_X86_AMD64: u32 = 5;
pub const APP_NAME: &str = "Pulsar Split targets";

pub fn build_tree(db_guid: &[u8; 16], targets: &[ResolvedTarget]) -> SdbNode {
    use SdbNode::*;
    let shim_ref = || List(tag::SHIM_REF, vec![Str(tag::NAME, crate::SHIM_NAME.into())]);
    let mut db = vec![
        Dword(tag::OS_PLATFORM, OS_PLATFORM_X86_AMD64),
        Str(tag::NAME, crate::SHIM_DB_NAME.into()),
        Binary(tag::DATABASE_ID, db_guid.to_vec()),
        List(
            tag::LIBRARY,
            vec![List(
                tag::SHIM,
                vec![
                    Str(tag::NAME, crate::SHIM_NAME.into()),
                    Str(tag::DLLFILE, crate::MODULE_X64.into()),
                    List(tag::INEXCLUDE, vec![Null(tag::INCLUDE), Str(tag::MODULE, "*".into())]),
                ],
            )],
        ),
        List(tag::LAYER, vec![Str(tag::NAME, crate::LAYER_NAME.into()), shim_ref()]),
    ];
    for t in targets {
        let mut matching = vec![Str(tag::NAME, "*".into())];
        if let Some(p) = &t.product_name {
            matching.push(Str(tag::PRODUCT_NAME, p.clone()));
        }
        if let Some(d) = &t.file_description {
            matching.push(Str(tag::FILE_DESCRIPTION, d.clone()));
        }
        db.push(List(
            tag::EXE,
            vec![
                Str(tag::NAME, t.exe_name.clone()),
                Str(tag::APP_NAME, APP_NAME.into()),
                Binary(tag::EXE_ID, t.exe_id.to_vec()),
                List(tag::MATCHING_FILE, matching),
                shim_ref(),
            ],
        ));
    }
    List(tag::DATABASE, db)
}

/// Writes a tree to a `.sdb` file (real impl calls apphelp's writer).
pub trait SdbWriter: Send + Sync {
    fn write(&self, path: &std::path::Path, tree: &SdbNode) -> Result<(), ShimError>;
}

/// Flat list of writer calls a tree produces (what the apphelp writer receives); used by
/// tests and by the real writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOp {
    Begin(u16),
    End(u16),
    Str(u16, String),
    Dword(u16, u32),
    Binary(u16, Vec<u8>),
    Null(u16),
}

pub fn write_ops(n: &SdbNode, out: &mut Vec<WriteOp>) {
    match n {
        SdbNode::List(t, children) => {
            out.push(WriteOp::Begin(*t));
            for c in children {
                write_ops(c, out);
            }
            out.push(WriteOp::End(*t));
        }
        SdbNode::Str(t, s) => out.push(WriteOp::Str(*t, s.clone())),
        SdbNode::Dword(t, v) => out.push(WriteOp::Dword(*t, *v)),
        SdbNode::Binary(t, b) => out.push(WriteOp::Binary(*t, b.clone())),
        SdbNode::Null(t) => out.push(WriteOp::Null(*t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_layout() {
        let g = guid_bytes("{00112233-4455-6677-8899-aabbccddeeff}").unwrap();
        assert_eq!(g, [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert!(guid_bytes(crate::SHIM_DB_GUID).is_ok());
        assert!(guid_bytes("nope").is_err());
    }

    #[test]
    fn tag_types_match_values() {
        for t in [tag::DATABASE, tag::LIBRARY, tag::INEXCLUDE, tag::SHIM, tag::EXE, tag::MATCHING_FILE, tag::SHIM_REF, tag::LAYER] {
            assert_eq!(t & 0xF000, 0x7000);
        }
        for t in [tag::NAME, tag::MODULE, tag::APP_NAME, tag::DLLFILE, tag::PRODUCT_NAME, tag::FILE_DESCRIPTION] {
            assert_eq!(t & 0xF000, 0x6000);
        }
        assert_eq!(tag::INCLUDE & 0xF000, 0x1000);
        assert_eq!(tag::OS_PLATFORM & 0xF000, 0x4000);
        assert_eq!(tag::EXE_ID & 0xF000, 0x9000);
    }

    #[test]
    fn tree_shape() {
        let g = guid_bytes(crate::SHIM_DB_GUID).unwrap();
        let t = ResolvedTarget {
            exe_name: "explorer.exe".into(),
            product_name: Some("Microsoft® Windows® Operating System".into()),
            file_description: Some("Windows Explorer".into()),
            exe_id: [7; 16],
        };
        let tree = build_tree(&g, &[t]);
        let mut ops = Vec::new();
        write_ops(&tree, &mut ops);
        assert_eq!(ops.first(), Some(&WriteOp::Begin(tag::DATABASE)));
        assert_eq!(ops.last(), Some(&WriteOp::End(tag::DATABASE)));
        let begins = ops.iter().filter(|o| matches!(o, WriteOp::Begin(_))).count();
        let ends = ops.iter().filter(|o| matches!(o, WriteOp::End(_))).count();
        assert_eq!(begins, ends);
        assert!(ops.contains(&WriteOp::Str(tag::DLLFILE, "PulsarSplitShim.dll".into())));
        assert!(ops.contains(&WriteOp::Str(tag::NAME, "explorer.exe".into())));
        assert!(ops.contains(&WriteOp::Str(tag::FILE_DESCRIPTION, "Windows Explorer".into())));
        assert!(ops.contains(&WriteOp::Binary(tag::DATABASE_ID, g.to_vec())));
        // No svchost / dwm, and never the reference names.
        assert!(!ops.iter().any(|o| matches!(o, WriteOp::Str(_, s) if s.contains("AcRes") || s == "svchost.exe")));
    }
}

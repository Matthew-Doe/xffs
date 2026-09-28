use caseless::Caseless;
use unicode_normalization::UnicodeNormalization;
use xffs_core::{format::*, names::*};
#[test]
fn golden_and_checksum_vectors() {
    assert_eq!(crc32c::crc32c(b"123456789"), 0xe3069283);
    assert_eq!(crc32c::crc32c(b""), 0);
    let golden = include_bytes!("golden/clean-control.bin");
    let c = JournalControl {
        sequence: 1,
        committed: false,
        count: 0,
        checksum: 0,
    };
    assert_eq!(JournalControl::decode(golden, 1).unwrap(), c);
    assert_eq!(&c.encode(1).unwrap(), golden);
    for n in 0..4096 {
        let mut b = *golden;
        b[n] ^= 1;
        assert!(JournalControl::decode(&b, 1).is_err(), "{n}");
    }
    for n in 0..4096 {
        assert!(JournalControl::decode(&golden[..n], 1).is_err());
    }
}
#[test]
fn geometry_and_unsupported() {
    let l = VolumeLayout::new(16 * 1024 * 1024, None).unwrap();
    assert_eq!(
        (l.bitmap_blocks, l.table_blocks, l.data_start),
        (1, 18, 534)
    );
    let l = VolumeLayout::new(128_000_000_000, None).unwrap();
    assert_eq!(
        (l.inodes, l.bitmap_blocks, l.table_blocks, l.data_start),
        (1_953_125, 969, 130_209, 131_693)
    );
    assert!(VolumeLayout::new(u64::MAX, Some(u64::MAX)).is_err());
    let s = Superblock {
        revision: FormatRevision::One,
        uuid: [7; 16],
        layout: l,
    };
    let b = s.encode(0).unwrap();
    assert_eq!(Superblock::decode(&b, 0, 128_000_000_000).unwrap(), s);
    let mut p = b[64..144].to_vec();
    p[64] = 2;
    let b = encode_block(Kind::Super, 0, InodeId::default(), &p).unwrap();
    assert!(matches!(
        Superblock::decode(&b, 0, 128_000_000_000),
        Err(FsError::Unsupported)
    ));
}
#[test]
fn unicode_and_portability() {
    assert_eq!(caseless::UNICODE_VERSION, (16, 0, 0));
    assert_eq!(unicode_normalization::UNICODE_VERSION, (16, 0, 0));
    for (a, b) in [("Straße", "STRASSE"), ("Café", "CAFE\u{301}"), ("Σ", "ς")] {
        assert_eq!(
            comparison_key(a.as_bytes()).unwrap(),
            comparison_key(b.as_bytes()).unwrap()
        );
    }
    for s in [
        "",
        ".",
        "..",
        "CON",
        "con.txt",
        "COM¹.log",
        "Lpt9",
        "CON .txt",
        "a.",
        "a ",
        "a/b",
        "a\\b",
        "a\0b",
        "\u{85}",
        "<x>",
    ] {
        assert!(validate_name(s.as_bytes()).is_err(), "{s}");
    }
    assert!(validate_name(&[255]).is_err());
    assert!(validate_name(&[b'a'; 256]).is_err());
    assert!(validate_name(b"console.txt").is_ok());
}
#[test]
fn all_unicode_scalar_expansions_fit_key_budget() {
    // Canonical reorder changes order, not total length. Bounding each scalar's
    // expansion per source byte also bounds every possible 255-byte name.
    for scalar in 0..=0x10ffff {
        if let Some(c) = char::from_u32(scalar) {
            let s = c.to_string();
            let n = s
                .nfd()
                .default_case_fold()
                .nfd()
                .map(char::len_utf8)
                .sum::<usize>();
            assert!(n * 255 <= MAX_KEY * c.len_utf8(), "U+{scalar:04X}");
        }
    }
}
#[test]
fn invalid_records_and_extents() {
    for len in 1..32 {
        let b = encode_block(Kind::Directory, 600, ROOT, &vec![0; len]).unwrap();
        assert!(decode_directory(&b, 600, ROOT).is_err());
    }
    let d = DirectoryRecord {
        name: "Mixed.txt".into(),
        child: InodeId {
            index: 2,
            generation: 1,
        },
    };
    let b = encode_block(Kind::Directory, 600, ROOT, &d.encode().unwrap()).unwrap();
    assert_eq!(decode_directory(&b, 600, ROOT).unwrap(), [d]);
    assert!(decode_directory(&b, 601, ROOT).is_err());
    let e = Extent {
        logical: u64::MAX,
        physical: 600,
        length: 1,
    };
    let b = encode_extents(700, ROOT, 0, &[e]).unwrap();
    assert!(decode_extents(&b, 700, ROOT).is_err());
    let e = Extent {
        logical: 0,
        physical: 600,
        length: 1,
    };
    let b = encode_extents(700, ROOT, 700, &[e]).unwrap();
    assert!(
        decode_extents(
            &b,
            700,
            InodeId {
                index: 1,
                generation: 1
            }
        )
        .is_err()
    );
    assert_eq!(MAX_CHAIN, 393);
}
#[test]
fn control_selection_table() {
    let clean = JournalControl {
        sequence: 1,
        committed: false,
        count: 0,
        checksum: 0,
    };
    let commit = JournalControl {
        sequence: 2,
        committed: true,
        count: 1,
        checksum: 42,
    };
    let retired = JournalControl {
        sequence: 3,
        ..clean
    };
    assert_eq!(
        select_control(Ok(clean), Ok(clean)).unwrap(),
        (clean, false)
    );
    assert_eq!(select_control(Ok(clean), Ok(commit)).unwrap().0, commit);
    assert_eq!(select_control(Ok(retired), Ok(commit)).unwrap().0, retired);
    assert!(select_control(Ok(clean), Ok(retired)).is_err());
    assert!(
        select_control(
            Ok(commit),
            Ok(JournalControl {
                checksum: 43,
                ..commit
            })
        )
        .is_err()
    );
    assert_eq!(
        select_control(Err(FsError::Corrupt("torn")), Ok(commit)).unwrap(),
        (commit, true)
    );
    assert!(select_control(Err(FsError::Unsupported), Ok(clean)).is_err());
    assert!(select_control(Err(FsError::Corrupt("a")), Err(FsError::Corrupt("b"))).is_err());
}
#[test]
fn malformed_chains_are_bounded() {
    let l = VolumeLayout::new(16 * 1024 * 1024, None).unwrap();
    let mut i = Inode {
        cleanup_bound: 0,
        atime: 0,
        id: ROOT,
        kind: FileKind::File,
        state: InodeState::Linked,
        executable: false,
        size: 5 * 4096,
        allocated: 6,
        parent: ROOT,
        times: [0; 3],
        overflow: 600,
        extent_count: 5,
        inline: (0..4)
            .map(|n| Extent {
                logical: n,
                physical: 700 + n,
                length: 1,
            })
            .collect(),
    };
    let e = Extent {
        logical: 4,
        physical: 704,
        length: 1,
    };
    assert!(resolve_extents(&i, &l, |_| encode_extents(600, ROOT, 600, &[e])).is_err());
    assert!(resolve_extents(&i, &l, |_| encode_extents(600, ROOT, 0, &[e])).is_ok());
    i.extent_count = 6;
    assert!(resolve_extents(&i, &l, |_| encode_extents(600, ROOT, 0, &[e])).is_err());
    i.overflow = 1;
    assert!(resolve_extents(&i, &l, |_| panic!("must not read forbidden reference")).is_err());
}
#[test]
fn independently_encoded_root_table() {
    let b = include_bytes!("golden/root-table.bin");
    let h = decode_block(b, 516).unwrap();
    assert_eq!(h.kind, Kind::Inodes);
    assert_eq!(h.used, 3840);
    let i = Inode::decode(&b[64..320], 0).unwrap().unwrap();
    assert_eq!(i.id, ROOT);
    assert_eq!(i.parent, ROOT);
    assert_eq!(i.kind, FileKind::Directory);
    assert_eq!(i.times, [1700000000; 3]);
    assert_eq!(i.size, 0);
    assert_eq!(i.allocated, 0);
    assert_eq!(i.extent_count, 0);
    assert!(b[320..].iter().all(|&b| b == 0));
}
#[test]
fn checksum_valid_bad_fields_and_arbitrary_slices_are_rejected_safely() {
    let golden = include_bytes!("golden/root-table.bin");
    for (offset, value) in [(10, 3), (12, 1), (14, 1), (48, 1), (4095, 1)] {
        let mut b = *golden;
        b[offset] = value;
        b[40..44].fill(0);
        let c = crc32c::crc32c(&b);
        b[40..44].copy_from_slice(&c.to_le_bytes());
        assert!(decode_block(&b, 516).is_err());
    }
    for offset in [16, 17, 18, 19, 92, 192] {
        let mut b: [u8; 256] = golden[64..320].try_into().unwrap();
        b[offset] = 255;
        assert!(Inode::decode(&b, 0).is_err());
    }
    let mut seed = 7u64;
    for len in 0..=BLOCK + 1 {
        let mut b = vec![0; len];
        for value in &mut b {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            *value = (seed >> 32) as u8;
        }
        let _ = decode_block(&b, 0);
        let _ = Inode::decode(&b, 0);
        let _ = decode_directory(&b, 0, ROOT);
        let _ = decode_extents(&b, 0, ROOT);
        let _ = JournalControl::decode(&b, 1);
        let _ = Superblock::decode(&b, 0, u64::MAX);
    }
}

#[test]
fn revision_two_slots_and_cleanup() {
    let id = InodeId {
        index: 7,
        generation: 41,
    };
    let free = free_inode(id).unwrap();
    let mut golden = [0; 256];
    golden[0..8].copy_from_slice(&7u64.to_le_bytes());
    golden[8..16].copy_from_slice(&41u64.to_le_bytes());
    assert_eq!(free, golden);
    assert!(Inode::decode(&free, 7).is_err());
    assert_eq!(next_inode_id(&free, 7).unwrap().unwrap().generation, 42);
    assert_eq!(
        next_inode_id(
            &free_inode(InodeId {
                generation: u64::MAX,
                ..id
            })
            .unwrap(),
            7
        )
        .unwrap(),
        None
    );
    assert_eq!(next_inode_id(&[0; 256], 7).unwrap().unwrap().generation, 1);
    let mut bad = free;
    bad[200] = 1;
    assert!(next_inode_id(&bad, 7).is_err());
    let table = include_bytes!("golden/root-table.bin");
    let mut i = Inode::decode(&table[64..320], 0).unwrap().unwrap();
    i.kind = FileKind::File;
    i.size = 1;
    i.cleanup_bound = 8192;
    i.atime = 123;
    let bytes = i.encode_revision(FormatRevision::Two).unwrap();
    assert_eq!(&bytes[192..200], &8192u64.to_le_bytes());
    assert_eq!(&bytes[200..208], &123u64.to_le_bytes());
    assert!(Inode::decode(&bytes, 0).is_err());
    assert_eq!(
        Inode::decode_revision(&bytes, 0, FormatRevision::Two).unwrap(),
        Some(i.clone())
    );
    i.cleanup_bound = 1;
    assert!(i.encode_revision(FormatRevision::Two).is_err());
}

#[test]
fn independent_revision_two_golden_and_malformed_states() {
    let table = include_bytes!("golden/revision-two-table.bin");
    let header = decode_block(table, 516).unwrap();
    assert_eq!(header.revision, FormatRevision::Two);
    let root = Inode::decode_revision(&table[64..320], 0, header.revision)
        .unwrap()
        .unwrap();
    assert_eq!(root.id, ROOT);
    let i = Inode::decode_revision(&table[320..576], 1, header.revision)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            i.id.generation,
            i.size,
            i.allocated,
            i.cleanup_bound,
            i.atime
        ),
        (9, 1, 2, 8192, 123)
    );
    assert_eq!(i.times, [1700000000, 1700000001, 1700000002]);
    assert_eq!(
        i.encode_revision(FormatRevision::Two).unwrap(),
        table[320..576]
    );
    assert_eq!(next_inode_id(&table[576..832], 2).unwrap(), None);
    let mut payload = [0; 3840];
    payload[..256].copy_from_slice(&root.encode_revision(FormatRevision::Two).unwrap());
    payload[256..512].copy_from_slice(&i.encode_revision(FormatRevision::Two).unwrap());
    payload[512..768].copy_from_slice(
        &free_inode(InodeId {
            index: 2,
            generation: u64::MAX,
        })
        .unwrap(),
    );
    assert_eq!(
        &with_revision(
            encode_block(Kind::Inodes, 516, InodeId::default(), &payload).unwrap(),
            FormatRevision::Two
        ),
        table
    );
    for (offset, value) in [(192, 1), (207, 255), (208, 1), (255, 1)] {
        let mut raw = i.encode_revision(FormatRevision::Two).unwrap();
        if offset == 192 {
            raw[192..200].fill(0);
        }
        raw[offset] = value;
        assert!(Inode::decode_revision(&raw, 1, FormatRevision::Two).is_err());
    }
    for offset in [16, 17, 18, 24, 192, 200, 255] {
        let mut raw: [u8; 256] = table[576..832].try_into().unwrap();
        raw[offset] = 255;
        assert!(next_inode_id(&raw, 2).is_err());
    }
}

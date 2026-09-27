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

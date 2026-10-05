#![allow(clippy::unwrap_used)]
use super::*;
use std::os::unix::fs::{DirBuilderExt, symlink};

fn archive(root: &str, body: &[u8]) -> Vec<u8> {
    let name = format!("{root}/Document");
    let mut crc = !0u32;
    for byte in body {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc = !crc;
    let size = u32::try_from(body.len()).unwrap();
    let len = u16::try_from(name.len()).unwrap();
    let mut out = Vec::new();
    out.extend(0x04034b50u32.to_le_bytes());
    for v in [20u16, 0, 0, 0, 33] {
        out.extend(v.to_le_bytes());
    }
    for v in [crc, size, size] {
        out.extend(v.to_le_bytes());
    }
    for v in [len, 0] {
        out.extend(v.to_le_bytes());
    }
    out.extend(name.as_bytes());
    out.extend(body);
    let offset = u32::try_from(out.len()).unwrap();
    out.extend(0x02014b50u32.to_le_bytes());
    for v in [20u16, 20, 0, 0, 0, 33] {
        out.extend(v.to_le_bytes());
    }
    for v in [crc, size, size] {
        out.extend(v.to_le_bytes());
    }
    for v in [len, 0, 0, 0, 0] {
        out.extend(v.to_le_bytes());
    }
    for v in [0u32, 0] {
        out.extend(v.to_le_bytes());
    }
    out.extend(name.as_bytes());
    let central = u32::try_from(out.len()).unwrap() - offset;
    out.extend(0x06054b50u32.to_le_bytes());
    for v in [0u16, 0, 1, 1] {
        out.extend(v.to_le_bytes());
    }
    for v in [central, offset] {
        out.extend(v.to_le_bytes());
    }
    out.extend(0u16.to_le_bytes());
    out
}
fn source(path: PathBuf, root: &str, body: &[u8]) -> Result<Source> {
    let bytes = archive(root, body);
    std::fs::write(&path, &bytes)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
    Ok(Source {
        path,
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(&bytes)),
        root: root.into(),
    })
}
#[test]
fn editor_source_proof_computes_actual_distinct_a_and_b_without_state() -> Result<()> {
    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-numbers-browser-editor-{run}"));
    std::fs::DirBuilder::new().mode(0o700).create(&root)?;
    let a = source(
        root.join("source-a.numbers"),
        "Actual Apple A Export.numbers",
        b"alpha",
    )?;
    let b = source(
        root.join("source-b.numbers"),
        "Completely Different B Root.numbers",
        b"beta edited",
    )?;
    let m = std::fs::metadata(&root)?;
    let mut semantics = Vec::new();
    for (phase, s, expanded) in [("a", a, 5), ("b", b, 11)] {
        let r = Registration {
            version: 1,
            run,
            phase: phase.into(),
            session_directory: root.clone(),
            root_dev: m.dev(),
            root_ino: m.ino(),
            source: s.clone(),
        };
        let bytes = serde_json::to_vec(&r)?;
        let path = root.join(format!("editor-{phase}-source-registration.json"));
        std::fs::write(&path, &bytes)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let before = std::fs::read(&s.path)?;
        let proof = icloud_owned_editor_source_proof(&path, &hex::encode(Sha256::digest(&bytes)))?;
        assert!(!root.join("state").exists());
        assert_eq!(std::fs::read(&s.path)?, before);
        assert_eq!(std::fs::read(&path)?, bytes);
        assert_eq!(proof["phase"], phase);
        assert_eq!(proof["source"]["root"], s.root);
        assert_eq!(proof["source"]["size"], s.size);
        assert_eq!(proof["source"]["sha256"], s.sha256);
        assert_eq!(proof["source"]["semantic"]["version"], 2);
        assert_eq!(proof["source"]["semantic"]["entries"], 2);
        assert_eq!(proof["source"]["semantic"]["files"], 1);
        assert_eq!(proof["source"]["semantic"]["expanded_bytes"], expanded);
        assert_eq!(proof["offline_only"], true);
        assert_eq!(proof["provider_representation_verified"], false);
        assert_eq!(proof["current_content_verified"], false);
        assert_eq!(proof["cloud_mutated"], false);
        assert_eq!(
            std::fs::metadata(&s.path)?.permissions().mode() & 0o777,
            0o400
        );
        semantics.push(proof["source"]["semantic"].clone());
    }
    assert_ne!(
        semantics[0], semantics[1],
        "B semantic identity was copied or guessed from A"
    );
    Ok(())
}
#[test]
fn editor_source_proof_refuses_wrong_raw_root_and_all_symlink_components() -> Result<()> {
    let dir = tempfile::tempdir()?.keep();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let s = source(
        dir.join("raw.numbers"),
        "Observed Root.numbers",
        b"contents",
    )?;
    let before = std::fs::read(&s.path)?;
    scan(&s)?;
    for arm in 0..3 {
        let mut bad = s.clone();
        match arm {
            0 => bad.sha256 = "a".repeat(64),
            1 => bad.size += 1,
            _ => bad.root = "Assumed Root.numbers".into(),
        }
        assert!(scan(&bad).is_err(), "raw/root arm {arm}");
    }
    symlink(&s.path, dir.join("leaf-link"))?;
    let mut bad = s.clone();
    bad.path = dir.join("leaf-link");
    assert!(scan(&bad).is_err());
    symlink(&dir, dir.join("ancestor-link"))?;
    bad.path = dir.join("ancestor-link/raw.numbers");
    assert!(scan(&bad).is_err());
    std::fs::set_permissions(&s.path, std::fs::Permissions::from_mode(0o600))?;
    assert!(scan(&s).is_err());
    assert_eq!(std::fs::read(&s.path)?, before);
    Ok(())
}
#[test]
fn editor_source_proof_registration_refuses_changed_scope_and_expected_semantics() -> Result<()> {
    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-numbers-browser-editor-{run}"));
    let r = Registration {
        version: 1,
        run,
        phase: "b".into(),
        session_directory: root.clone(),
        root_dev: 31,
        root_ino: 123,
        source: Source {
            path: root.join("source-b.numbers"),
            size: 123,
            sha256: "a".repeat(64),
            root: "Actual B Name.numbers".into(),
        },
    };
    let bytes = serde_json::to_vec(&r)?;
    registered(&bytes, &hex::encode(Sha256::digest(&bytes)))?;
    assert!(registered(&bytes, &"b".repeat(64)).is_err());
    for arm in 0..5 {
        let mut bad = r.clone();
        match arm {
            0 => bad.run = Uuid::nil(),
            1 => bad.phase = "a".into(),
            2 => bad.session_directory = PathBuf::from("/var/tmp/other"),
            3 => bad.source.root = "nested/source.numbers".into(),
            _ => bad.source.size = LIMIT + 1,
        }
        let b = serde_json::to_vec(&bad)?;
        assert!(
            registered(&b, &hex::encode(Sha256::digest(&b))).is_err(),
            "registration arm {arm}"
        );
    }
    let mut guessed: serde_json::Value = serde_json::from_slice(&bytes)?;
    guessed["source"]["semantic"] = serde_json::json!({"version":2,"sha256":"a".repeat(64),"entries":2,"files":1,"expanded_bytes":3});
    let b = serde_json::to_vec(&guessed)?;
    assert!(
        registered(&b, &hex::encode(Sha256::digest(&b))).is_err(),
        "input cannot inject an A semantic identity"
    );
    Ok(())
}

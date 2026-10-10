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
        root: Some(root.into()),
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
        assert_eq!(proof["source"]["root"], serde_json::to_value(&s.root)?);
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
            _ => bad.root = Some("Assumed Root.numbers".into()),
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
            root: Some("Actual B Name.numbers".into()),
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
            3 => bad.source.root = Some("nested/source.numbers".into()),
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

#[test]
fn editor_source_proof_computes_separate_remount_b_and_refuses_foreign_path() -> Result<()> {
    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-numbers-browser-editor-{run}"));
    std::fs::DirBuilder::new().mode(0o700).create(&root)?;
    let export = source(
        root.join("source-b.numbers"),
        "Actual Export B.numbers",
        b"edited beta",
    )?;
    let capture = source(
        root.join("source-remount-b.numbers"),
        "Actual Mounted Wrapper.numbers",
        b"edited beta",
    )?;
    let m = std::fs::metadata(&root)?;
    let r = Registration {
        version: 1,
        run,
        phase: "remount-b".into(),
        session_directory: root.clone(),
        root_dev: m.dev(),
        root_ino: m.ino(),
        source: capture.clone(),
    };
    let bytes = serde_json::to_vec(&r)?;
    let path = root.join("editor-remount-b-source-registration.json");
    std::fs::write(&path, &bytes)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let raw_before = std::fs::read(&capture.path)?;
    let export_before = std::fs::read(&export.path)?;
    let result = icloud_owned_editor_source_proof(&path, &hex::encode(Sha256::digest(&bytes)));
    assert_eq!(std::fs::read(&capture.path)?, raw_before);
    assert_eq!(std::fs::read(&export.path)?, export_before);
    assert_eq!(std::fs::read(&path)?, bytes);
    assert!(!root.join("state").exists());
    let proof = result.expect("remount-b phase rejected before computing actual archive semantic");
    assert_eq!(proof["phase"], "remount-b");
    assert_eq!(
        proof["source"]["path"],
        capture.path.to_string_lossy().as_ref()
    );
    assert_eq!(
        proof["source"]["root"],
        serde_json::to_value(&capture.root)?
    );
    assert_eq!(proof["source"]["sha256"], capture.sha256);
    assert_ne!(
        capture.sha256, export.sha256,
        "different wrappers must retain distinct raw identity"
    );
    assert_eq!(
        proof["source"]["semantic"],
        serde_json::to_value(scan(&export)?)?
    );
    assert_eq!(proof["provider_representation_verified"], false);
    assert_eq!(proof["cloud_mutated"], false);
    for phase in ["a", "b", "remount-c"] {
        let mut bad = r.clone();
        bad.phase = phase.into();
        let b = serde_json::to_vec(&bad)?;
        assert!(
            registered(&b, &hex::encode(Sha256::digest(&b))).is_err(),
            "foreign phase/path accepted"
        );
    }
    for name in [
        "source-b.numbers",
        "source-a.numbers",
        "another-capture.numbers",
    ] {
        let mut bad = r.clone();
        bad.source.path = root.join(name);
        let b = serde_json::to_vec(&bad)?;
        assert!(
            registered(&b, &hex::encode(Sha256::digest(&b))).is_err(),
            "remount masquerading as export accepted"
        );
    }
    Ok(())
}

#[test]
fn editor_source_proof_scans_unmodified_flat_numbers_with_explicit_null_root() -> Result<()> {
    let run = Uuid::new_v4();
    let root = PathBuf::from(format!("/var/tmp/cirrove-numbers-browser-editor-{run}"));
    std::fs::DirBuilder::new().mode(0o700).create(&root)?;
    let path = root.join("source-b.numbers");
    let flat = archive("Index", b"actual edited bytes");
    std::fs::write(&path, &flat)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
    let m = std::fs::metadata(&root)?;
    let registration = serde_json::json!({"version":1,"run":run,"phase":"b",
        "session_directory":root,"root_dev":m.dev(),"root_ino":m.ino(),
        "source":{"path":path,"size":flat.len(),"sha256":hex::encode(Sha256::digest(&flat)),"root":null}});
    let bytes = serde_json::to_vec(&registration)?;
    let control = root.join("editor-b-source-registration.json");
    std::fs::write(&control, &bytes)?;
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o400))?;
    let result = icloud_owned_editor_source_proof(&control, &hex::encode(Sha256::digest(&bytes)));
    assert_eq!(std::fs::read(&path)?, flat);
    assert_eq!(std::fs::read(&control)?, bytes);
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o400
    );
    assert!(!root.join("state").exists());
    let proof = result.expect("flat Numbers source rejected at actual offline scanner");
    assert_eq!(proof["source"]["root"], serde_json::Value::Null);
    assert_eq!(proof["source"]["semantic"]["version"], 2);
    assert_eq!(proof["source"]["semantic"]["entries"], 3);
    assert_eq!(proof["source"]["semantic"]["files"], 1);
    assert_eq!(proof["source"]["semantic"]["expanded_bytes"], 19);
    assert_eq!(proof["offline_only"], true);
    assert_eq!(proof["provider_representation_verified"], false);
    assert_eq!(proof["current_content_verified"], false);
    assert_eq!(proof["cloud_mutated"], false);
    let wrapped_path = root.join("comparison-only.numbers");
    let wrapped = archive("Observed.numbers/Index", b"actual edited bytes");
    std::fs::write(&wrapped_path, &wrapped)?;
    std::fs::set_permissions(&wrapped_path, std::fs::Permissions::from_mode(0o400))?;
    let wrapped_source: Source = serde_json::from_value(serde_json::json!({"path":wrapped_path,
        "size":wrapped.len(),"sha256":hex::encode(Sha256::digest(&wrapped)),"root":"Observed.numbers"}))?;
    assert_eq!(
        proof["source"]["semantic"],
        serde_json::to_value(scan(&wrapped_source)?)?
    );
    assert_ne!(proof["source"]["sha256"], wrapped_source.sha256);
    for arm in 0..6 {
        let mut bad = registration.clone();
        match arm {
            0 => {
                bad["source"].as_object_mut().unwrap().remove("root");
            }
            1 => bad["source"]["root"] = "".into(),
            2 => bad["source"]["root"] = "wrong/Root.numbers".into(),
            3 => bad["source"]["semantic"] = proof["source"]["semantic"].clone(),
            4 => {
                bad["source"]["path"] = root
                    .join("source-a.numbers")
                    .to_string_lossy()
                    .as_ref()
                    .into()
            }
            _ => bad["phase"] = "foreign".into(),
        }
        let raw = serde_json::to_vec(&bad)?;
        assert!(
            registered(&raw, &hex::encode(Sha256::digest(&raw))).is_err(),
            "flat schema arm {arm}"
        );
    }
    Ok(())
}

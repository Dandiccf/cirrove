//! Representation benchmark, separate from kernel/provider capacity acceptance.
use super::*;

#[test]
#[ignore = "explicit in-process live-projection allocation benchmark; no kernel or cloud"]
fn shared_projection_payload_baseline() {
    let files =
        std::env::var("CIRROVE_PROJECTION_FILES").map_or(50_000, |s| s.parse::<usize>().unwrap());
    assert!((300..=500_000).contains(&files));
    let generator = GeneratedLibrary {
        files,
        per_directory: 1000,
        revision: AtomicU32::new(1),
        content_reads: AtomicU64::new(0),
        foreground_requests: AtomicU64::new(0),
    };
    let node = generator.node_at(0);
    let root = View {
        residency: Arc::default(),
        _parent_residency: None,
        inode: 1,
        parent: 1,
        scope: Scope {
            account: "synthetic-account-with-a-stable-identity".into(),
            provider: "synthetic-projection-fixture".into(),
            collection: "primary-synthetic-collection".into(),
        }
        .into(),
        ancestry: vec![("primary-synthetic-collection".into(), "root".into())].into(),
        node: node.into(),
        name: "root".into(),
        alias: vec![].into(),
        reference: false,
        entry: None,
    };
    let before = process_memory();
    let start = Instant::now();
    let mut views = NamespaceViews::new(root.clone());
    let mut inode = 2;
    let mut parents = Vec::new();
    for route in 0..3 {
        let mut parent = root.clone();
        if route > 0 {
            let mut link = generator.node_at(0);
            link.id = format!("shortcut-{route}");
            link.name = format!("Alias {route}");
            link.parent_id = Some("root".into());
            link.kind = NodeKind::Shortcut;
            link.target = Some(Box::new(cirrove_core::RemoteRef {
                collection: "shared-synthetic-collection".into(),
                item: "shared-root".into(),
                kind: Some(NodeKind::Folder),
            }));
            let mut view = Inner::project(&parent, link).unwrap();
            view.inode = inode;
            inode += 1;
            parent = views.insert(view).unwrap();
        }
        for depth in 0..12 {
            let mut node = generator.node_at(0);
            node.id = format!("directory-with-a-stable-identity-{depth}");
            node.name = format!("directory-{depth}");
            node.parent_id = Some(parent.node.id.clone());
            let mut view = Inner::project(&parent, node).unwrap();
            view.inode = inode;
            inode += 1;
            parent = views.insert(view).unwrap();
        }
        parents.push(parent);
    }
    let first_file = inode;
    let mut held = Vec::new();
    let mut keys = Vec::new();
    for file in 0..files {
        let parent = &parents[file % 3];
        let mut node = generator.node_at(generator.directories() + 1 + file / 3);
        node.parent_id = Some(parent.node.id.clone());
        let mut view = Inner::project(parent, node).unwrap();
        view.inode = inode;
        if file < 3 {
            keys.push(Inner::inode_key(&view, false).unwrap());
        }
        inode += 1;
        let view = views.insert(view).unwrap();
        views.acquire_lookup(view.inode).unwrap();
        if file < 32 {
            held.push(view);
        }
    }
    assert_ne!(keys[0], keys[1]);
    assert_ne!(keys[1], keys[2]);
    assert_eq!(views.len(), files + 39);
    let populated = process_memory();
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    for inode in first_file..inode {
        assert!(views.forget(inode, 1));
    }
    drop((held, parents, root));
    // A batch of stale queue candidates can remove zero entries before reaching
    // retained ancestors; zero removals is not the end of the candidate queue.
    for _ in 0..files.div_ceil(4096) + 64 {
        if views.len() == 1 {
            break;
        }
        views.collect(4096);
    }
    assert_eq!(views.len(), 1);
    println!(
        "CIRROVE_PROJECTION_PAYLOAD {}",
        serde_json::json!({
            "files":files,"routes":3,"directory_depth":12,"held_clones":32,
            "view_inline_bytes":std::mem::size_of::<View>(),"node_inline_bytes":std::mem::size_of::<Node>(),"before":before,
            "populated":populated,"after_retirement":process_memory(),
            "retained_views":views.len(),"populate_ms":elapsed_ms,
            "build":if cfg!(debug_assertions){"debug"}else{"release"},
            "scope":"in-process production projection/index/lifetime code; no kernel, SQLite or provider calls"
        })
    );
}

use std::{path::Path, time::Duration};

use atune::config::{Config, FileSync, Project};

fn setup(root: &Path) {
    let test_1 = root.join("test_1");
    let test_2 = root.join("test_2");

    std::fs::create_dir_all(&test_1).unwrap();
    std::fs::create_dir_all(&test_2).unwrap();

    for i in 0..10 {
        std::fs::write(test_1.join(format!("{i}.txt")), "some content").unwrap();
        std::fs::write(test_2.join(format!("{i}.txt")), "some content").unwrap();
    }
}

#[test]
fn test_watch_syncs() {
    let dir = tempfile::Builder::new().prefix("atune_").tempdir().unwrap();
    setup(dir.path());

    let out = dir.path().join("out");

    let config = Config {
        project: vec![Project {
            sync: vec![FileSync {
                src: dir.path().join("test_1"),
                dst: out.clone(),
                rsync_flags: Some(format!(
                    r#"-av --rsync-path "mkdir -p {} && rsync""#,
                    out.display()
                )),
                ..Default::default()
            }],
            ..Default::default()
        }],
        debounce: Duration::from_secs(0),
    };

    let (cancel_tx, cancel_rx) = crossbeam::channel::bounded(1);

    assert!(!out.exists());
    let watch = std::thread::spawn(move || atune::watch(config, cancel_rx));

    std::thread::sleep(Duration::from_millis(500));

    assert!(out.exists());
    assert!(out.is_dir());
    for i in 0..10 {
        let f = out.join(format!("test_1/{i}.txt"));
        assert!(f.exists());
        assert!(f.is_file());
    }
    cancel_tx.send(()).unwrap();
    let _ = watch.join().unwrap();
}

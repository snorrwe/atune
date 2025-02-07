use std::{io::Write, path::Path, process::Stdio, time::Duration};

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
fn test_watch_syncs_on_init() {
    let cli = std::env!("CARGO_BIN_EXE_atune");

    let cli = std::env::var("ATUNE_BIN").unwrap_or(cli.to_owned());

    let dir = tempfile::Builder::new().prefix("atune_").tempdir().unwrap();
    setup(dir.path());

    let out = dir.path().join("out");

    let config = format!(
        r#"
debounce: 0s
projects:
    test_1:
      sync:
        -
            src: {}
            dst: {}
            rsync_flags: -av --rsync-path "mkdir -p {} && rsync"
    "#,
        dir.path().join("test_1").display(),
        out.display(),
        out.display()
    );

    let config_file_path = dir.path().join("config.yaml");
    let mut config_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&config_file_path)
        .expect("Failed to open config");
    config_file.write_all(config.as_bytes()).unwrap();

    let mut proc = std::process::Command::new(&cli)
        .arg("-c")
        .arg(config_file_path.as_os_str())
        .arg("watch")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("Failed to spawn atune");

    std::thread::sleep(Duration::from_millis(200));

    assert!(out.exists());
    assert!(out.is_dir());
    for i in 0..10 {
        let f = out.join(format!("test_1/{i}.txt"));
        assert!(f.exists());
        assert!(f.is_file());
    }

    proc.kill().expect("Failed to kill atune");
}

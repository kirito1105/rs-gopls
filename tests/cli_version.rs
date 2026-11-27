use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn probe(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rs-gopls"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // VS Code's execFile leaves stdin open while waiting for the version.
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{args:?} did not exit while stdin was open");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    output
}

#[test]
fn json_version_reports_the_module_and_its_version() {
    let output = probe(&["version", "-json"]);
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["Main"]["Path"], env!("CARGO_PKG_NAME"));
    assert_eq!(info["Main"]["Version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn text_version_reports_the_server_and_its_version() {
    let output = probe(&["version"]);
    let expected = format!("{} {}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
}

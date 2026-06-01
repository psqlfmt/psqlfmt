use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn formats_stdin() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_psqlfmt"))
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"select a,b from t where a=1 and b=2;")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("SELECT\n  a,"));
    assert!(stdout.contains("WHERE\n  a = 1\n  AND b = 2;"));
}

#[test]
fn write_resolves_editorconfig_and_psqlfmt() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".editorconfig"),
        "root = true\n[*.sql]\nindent_style = space\nindent_size = 2\n",
    )
    .unwrap();
    fs::write(dir.path().join(".psqlfmt"), "keyword-case=1\ntype-case=1\n").unwrap();
    let file = dir.path().join("query.sql");
    fs::write(&file, "select a,b from t;").unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_psqlfmt"))
        .arg("--write")
        .arg(&file)
        .status()
        .unwrap();
    assert!(status.success());

    let formatted = fs::read_to_string(file).unwrap();
    assert_eq!(formatted, "select\n  a,\n  b\nfrom\n  t;\n");
}

#[test]
fn directory_write_respects_gitignore() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".gitignore"), "ignored.sql\n").unwrap();
    let ignored = dir.path().join("ignored.sql");
    let kept = dir.path().join("kept.sql");
    fs::write(&ignored, "select a from ignored;").unwrap();
    fs::write(&kept, "select a from kept;").unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_psqlfmt"))
        .arg("--write")
        .arg(dir.path())
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(
        fs::read_to_string(ignored).unwrap(),
        "select a from ignored;"
    );
    assert_eq!(
        fs::read_to_string(kept).unwrap(),
        "SELECT\n  a\nFROM\n  kept;\n"
    );
}

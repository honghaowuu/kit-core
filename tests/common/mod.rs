#![allow(dead_code)]

use assert_cmd::Command;
use std::path::Path;
use std::process::Command as StdCommand;

pub fn kit() -> Command {
    Command::cargo_bin("kit").expect("binary 'kit' built")
}

pub fn git_init(dir: &Path) {
    sh(dir, &["init", "-q", "-b", "main"]);
    sh(dir, &["config", "user.email", "test@local"]);
    sh(dir, &["config", "user.name", "test"]);
    sh(dir, &["config", "commit.gpgsign", "false"]);
}

pub fn sh(dir: &Path, args: &[&str]) -> String {
    let out = StdCommand::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git invocation");
    if !out.status.success() {
        panic!(
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).unwrap()
}

pub fn write(dir: &Path, rel: &str, contents: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, contents).unwrap();
}

pub fn commit_all(dir: &Path, subject: &str) {
    sh(dir, &["add", "-A"]);
    sh(dir, &["commit", "-q", "-m", subject]);
}

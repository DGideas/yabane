use std::{fs, path::PathBuf, process::Command};

fn main() {
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| PathBuf::from(value.trim()));

    if let Some(git_dir) = git_dir {
        let head = git_dir.join("HEAD");
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(contents) = fs::read_to_string(&head)
            && let Some(reference) = contents.trim().strip_prefix("ref: ")
        {
            println!(
                "cargo:rerun-if-changed={}",
                git_dir.join(reference).display()
            );
        }
    }

    let commit = git_output(&["rev-parse", "--short=8", "HEAD"]);
    let commit_time = git_output(&["show", "-s", "--format=%cI", "HEAD"]);
    println!("cargo:rustc-env=YABANE_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=YABANE_GIT_COMMIT_TIME={commit_time}");
}

fn git_output(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

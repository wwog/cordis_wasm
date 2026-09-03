use std::path::PathBuf;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    if let Some("build-guests") = command.as_deref() {
        build_guests()
    } else {
        eprintln!("usage: cargo xtask build-guests");
        ExitCode::FAILURE
    }
}

fn build_guests() -> ExitCode {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let target = "wasm32-wasip2";
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line == target)
        });
    if !installed {
        eprintln!("missing Rust target `{target}`; install it with `rustup target add {target}`");
        return ExitCode::FAILURE;
    }

    let status = Command::new("cargo")
        .current_dir(&root)
        .args([
            "build",
            "--target",
            target,
            "-p",
            "wasm-counter-provider",
            "-p",
            "wasm-counter-consumer",
        ])
        .status();
    match status {
        Ok(status) if status.success() => {
            let fixtures = root.join("target").join(target).join("debug");
            match Command::new("cargo")
                .current_dir(&root)
                .env("CORDIS_GUEST_FIXTURES", fixtures)
                .args(["test", "-p", "cordis-wasm", "artifacts"])
                .status()
            {
                Ok(status) if status.success() => ExitCode::SUCCESS,
                Ok(status) => status_exit_code(status),
                Err(error) => {
                    eprintln!("failed to verify guest artifacts: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(status) => status_exit_code(status),
        Err(error) => {
            eprintln!("failed to invoke cargo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn status_exit_code(status: std::process::ExitStatus) -> ExitCode {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .filter(|code| *code != 0)
        .map_or(ExitCode::FAILURE, ExitCode::from)
}

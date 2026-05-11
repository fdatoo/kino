use std::{path::Path, process::ExitCode};

fn main() -> ExitCode {
    println!("cargo:rerun-if-changed=web/dist");

    if !Path::new("web/dist/index.html").exists() {
        eprintln!(
            "missing web/dist/index.html; run `pnpm install && pnpm build` in crates/kino-admin/web before building kino-admin"
        );
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

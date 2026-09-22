// SPDX-License-Identifier: EUPL-1.2
use anyhow::{bail, Context, Result};
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};
fn checked(c: &mut Command) -> Result<()> {
    let status = c.status().context("start build command")?;
    if !status.success() {
        bail!("build command failed: {status}");
    }
    Ok(())
}
fn output(args: &[&str]) -> Result<String> {
    let o = Command::new("rustc").args(args).output()?;
    if !o.status.success() {
        bail!("rustc query failed");
    }
    Ok(String::from_utf8(o.stdout)?.trim().into())
}
fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    env::set_current_dir(root)?;
    let metadata = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()?;
    if !metadata.status.success() {
        bail!("cannot query Cargo target directory");
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)?;
    let target_dir = PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .context("Cargo target directory")?,
    );
    let args: Vec<_> = env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or("build");
    if !["build", "dist"].contains(&action) {
        bail!("usage: cargo xtask [build|dist] [--target TARGET]");
    }
    let rust_info = output(&["-vV"])?;
    let host = rust_info
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .context("rust host triple")?;
    let target = match args.get(1).map(String::as_str) {
        Some("--target") => args.get(2).context("missing --target value")?.as_str(),
        None => host,
        _ => bail!("usage: cargo xtask [build|dist] [--target TARGET]"),
    };
    if args.len() > 3 {
        bail!("unexpected build argument");
    }
    let sysroot = PathBuf::from(output(&["--print", "sysroot"])?);
    let lld = sysroot.join("lib/rustlib").join(host).join("bin/rust-lld");
    let android = [
        ("AARCH64", "aarch64-unknown-linux-musl"),
        ("X86_64", "x86_64-unknown-linux-musl"),
    ];
    let mut binaries = Vec::new();
    for (abi, triple) in android {
        checked(Command::new("rustup").args(["target", "add", triple]))?;
        let envkey = format!(
            "CARGO_TARGET_{}_LINKER",
            triple.replace('-', "_").to_uppercase()
        );
        checked(
            Command::new("cargo")
                .args([
                    "build",
                    "--locked",
                    "--release",
                    "-p",
                    "adb-input-agent",
                    "--target",
                    triple,
                ])
                .env(envkey, &lld),
        )?;
        binaries.push((
            format!("ADB_INPUT_AGENT_{abi}"),
            target_dir.join(triple).join("release/adb-input-agent"),
        ));
    }
    checked(Command::new("rustup").args(["target", "add", target]))?;
    let mut build = Command::new("cargo");
    build.args([
        "build",
        "--locked",
        "--release",
        "-p",
        "adb-input",
        "--target",
        target,
    ]);
    for (key, path) in binaries {
        build.env(key, path);
    }
    if target.ends_with("-musl") {
        build.env(
            format!(
                "CARGO_TARGET_{}_LINKER",
                target.replace('-', "_").to_uppercase()
            ),
            &lld,
        );
    }
    checked(&mut build)?;
    let binary = target_dir.join(target).join("release/adb-input");
    println!("Built {}", binary.display());
    if action == "dist" {
        if !target.contains("linux") {
            bail!("release packaging currently supports Linux");
        }
        let arch = target.split('-').next().unwrap();
        if !["x86_64", "aarch64"].contains(&arch) {
            bail!("unsupported release architecture: {arch}");
        }
        let stage = target_dir.join("package").join(target);
        std::fs::create_dir_all(&stage)?;
        std::fs::copy(binary, stage.join("adb-input"))?;
        for name in ["LICENSE", "THIRD_PARTY.md"] {
            std::fs::copy(root.join(name), stage.join(name))?;
        }
        let dist = root.join("dist");
        std::fs::create_dir_all(&dist)?;
        let filename = format!("adb-input-linux-{arch}.tar.gz");
        checked(
            Command::new("tar")
                .args([
                    "--sort=name",
                    "--mtime=@0",
                    "--owner=0",
                    "--group=0",
                    "--numeric-owner",
                ])
                .arg("-czf")
                .arg(dist.join(&filename))
                .arg("-C")
                .arg(stage)
                .args(["adb-input", "LICENSE", "THIRD_PARTY.md"]),
        )?;
        let hash = Command::new("sha256sum")
            .current_dir(&dist)
            .arg(&filename)
            .output()?;
        if !hash.status.success() {
            bail!("sha256sum failed");
        }
        std::fs::write(dist.join(format!("{filename}.sha256")), hash.stdout)?;
    }
    Ok(())
}

// SPDX-License-Identifier: EUPL-1.2
use std::{env, fs, path::PathBuf};
fn main() {
    for abi in ["AARCH64", "X86_64"] {
        let key = format!("ADB_INPUT_AGENT_{abi}");
        println!("cargo:rerun-if-env-changed={key}");
        let bytes = if let Ok(path) = env::var(&key) {
            println!("cargo:rerun-if-changed={path}");
            fs::read(path).expect("read Android agent")
        } else {
            Vec::new()
        };
        fs::write(
            PathBuf::from(env::var_os("OUT_DIR").unwrap()).join(format!("agent-{abi}")),
            bytes,
        )
        .unwrap();
    }
}

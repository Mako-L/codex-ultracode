use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const SETUP_BIN: &str = "codex-windows-sandbox-setup";
const SETUP_MANIFEST: &str = "codex-windows-sandbox-setup.manifest";

fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed={SETUP_MANIFEST}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| "CARGO_MANIFEST_DIR should be set for build scripts".to_string())?;
    let manifest_file = PathBuf::from(manifest_dir).join(SETUP_MANIFEST);
    let manifest_path = manifest_file.display();

    match (
        env::var("CARGO_CFG_TARGET_ENV").as_deref(),
        env::var("CARGO_CFG_TARGET_ABI").as_deref(),
    ) {
        (Ok("msvc"), _) => {
            if cfg!(windows) {
                println!("cargo:rustc-link-arg-bin={SETUP_BIN}=/MANIFEST:EMBED");
                println!("cargo:rustc-link-arg-bin={SETUP_BIN}=/MANIFESTINPUT:{manifest_path}");
            } else {
                let res_path = write_manifest_res(&manifest_file)?;
                println!("cargo:rustc-link-arg-bin={SETUP_BIN}={}", res_path.display());
            }
        }
        (Ok("gnu"), Ok("llvm")) => {
            println!("cargo:rustc-link-arg-bin={SETUP_BIN}=-Wl,-Xlink=/manifest:embed");
            println!(
                "cargo:rustc-link-arg-bin={SETUP_BIN}=-Wl,-Xlink=/manifestinput:{manifest_path}"
            );
        }
        _ => {}
    }

    Ok(())
}

fn write_manifest_res(manifest_file: &Path) -> Result<PathBuf, String> {
    let data = fs::read(manifest_file).map_err(|error| format!("read {SETUP_MANIFEST}: {error}"))?;
    let out_dir = env::var_os("OUT_DIR")
        .ok_or_else(|| "OUT_DIR should be set for build scripts".to_string())?;
    let res_path = PathBuf::from(out_dir).join("codex-windows-sandbox-setup.manifest.res");
    fs::write(&res_path, encode_rt_manifest_res(&data))
        .map_err(|error| format!("write manifest res: {error}"))?;
    Ok(res_path)
}

fn encode_rt_manifest_res(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    append_resource(&mut out, 0, 0, &[]);
    append_resource(&mut out, 24, 1, data);
    out
}

fn append_resource(out: &mut Vec<u8>, type_id: u16, name_id: u16, data: &[u8]) {
    let type_name = u16::to_le_bytes(0xFFFF)
        .into_iter()
        .chain(type_id.to_le_bytes())
        .chain(u16::to_le_bytes(0xFFFF))
        .chain(name_id.to_le_bytes())
        .collect::<Vec<_>>();
    let mut rest = Vec::new();
    rest.extend_from_slice(&0u32.to_le_bytes());
    rest.extend_from_slice(&0x1030u16.to_le_bytes());
    rest.extend_from_slice(&0x0409u16.to_le_bytes());
    rest.extend_from_slice(&0u32.to_le_bytes());
    rest.extend_from_slice(&0u32.to_le_bytes());
    let header_size = 8 + type_name.len() + rest.len();
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&(header_size as u32).to_le_bytes());
    out.extend_from_slice(&type_name);
    out.extend_from_slice(&rest);
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

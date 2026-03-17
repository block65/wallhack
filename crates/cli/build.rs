// NOTE: This WallhackBuildEnv impl is duplicated in crates/mcp/build.rs.
// build.rs files cannot share code via library crates.
use std::collections::BTreeMap;

use vergen_gitcl::{
    AddCustomEntries, BuildBuilder, CargoRerunIfChanged, CargoWarning, DefaultConfig, Emitter,
    GitclBuilder,
};

#[derive(Default)]
struct WallhackBuildEnv;

impl AddCustomEntries<&str, &str> for WallhackBuildEnv {
    fn add_calculated_entries(
        &self,
        _idempotent: bool,
        cargo_rustc_env_map: &mut BTreeMap<&str, &str>,
        _cargo_rerun_if_changed: &mut CargoRerunIfChanged,
        _cargo_warning: &mut CargoWarning,
    ) -> anyhow::Result<()> {
        let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
        cargo_rustc_env_map.insert(
            "WALLHACK_BUILD_PROFILE",
            Box::leak(profile.into_boxed_str()),
        );
        Ok(())
    }

    fn add_default_entries(
        &self,
        _config: &DefaultConfig,
        _cargo_rustc_env_map: &mut BTreeMap<&str, &str>,
        _cargo_rerun_if_changed: &mut CargoRerunIfChanged,
        _cargo_warning: &mut CargoWarning,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Emitter::default()
        .add_instructions(&BuildBuilder::default().build_timestamp(true).build()?)?
        .add_instructions(&GitclBuilder::default().sha(true).dirty(true).build()?)?
        .add_custom_instructions(&WallhackBuildEnv)?
        .emit()?;

    // vergen emits cargo:rerun-if-changed=.git/HEAD so the build script
    // only reruns on git state changes. Force it to always rerun so the
    // build timestamp reflects the actual compilation time.
    println!("cargo:rerun-if-changed=.build_timestamp_force");
    //
    let ts = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
        .trim()
        .to_string();
    if !ts.is_empty() {
        println!("cargo:rustc-env=VERGEN_BUILD_TIMESTAMP={ts}");
    }

    Ok(())
}

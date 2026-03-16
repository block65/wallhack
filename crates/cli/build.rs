use vergen_gitcl::{BuildBuilder, CargoBuilder, Emitter, GitclBuilder, RustcBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // PROFILE is a standard Cargo build env var; emit it for use via env!() in source.
    println!(
        "cargo:rustc-env=WALLHACK_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string())
    );

    Emitter::default()
        .add_instructions(&BuildBuilder::default().build_timestamp(true).build()?)?
        .add_instructions(&GitclBuilder::default().sha(true).dirty(true).build()?)?
        .add_instructions(
            &CargoBuilder::default()
                .target_triple(true)
                .features(true)
                .build()?,
        )?
        .add_instructions(&RustcBuilder::default().semver(true).build()?)?
        .emit()?;
    Ok(())
}

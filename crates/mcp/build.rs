use vergen_gitcl::{BuildBuilder, Emitter, GitclBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "cargo:rustc-env=WALLHACK_BUILD_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string())
    );
    Emitter::default()
        .add_instructions(&BuildBuilder::default().build_timestamp(true).build()?)?
        .add_instructions(&GitclBuilder::default().sha(true).dirty(true).build()?)?
        .emit()?;
    Ok(())
}

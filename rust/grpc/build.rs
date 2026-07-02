//! Compile the generated proto contract into the wire types and the server
//! trait. The proto is itself a projection of the model, so the wire schema is
//! drift-gated with the rest of the generated code.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let descriptors = protox::compile(["proto/theseus.proto"], ["proto"])?;
    tonic_prost_build::configure().compile_fds(descriptors)?;
    println!("cargo:rerun-if-changed=proto/theseus.proto");
    Ok(())
}

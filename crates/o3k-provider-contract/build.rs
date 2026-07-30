fn main() {
    println!("cargo:rerun-if-changed=../../proto/provider/v1/compute.proto");
    let result = tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&["../../proto/provider/v1/compute.proto"], &["../../proto"]);
    if let Err(error) = result {
        eprintln!("provider protobuf generation failed: {error}");
        std::process::exit(1);
    }
}

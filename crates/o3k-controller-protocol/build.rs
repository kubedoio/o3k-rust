fn main() {
    println!("cargo:rerun-if-changed=proto/o3k/controller/v1/controller.proto");
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&["proto/o3k/controller/v1/controller.proto"], &["proto"])
        .unwrap_or_else(|error| {
            eprintln!("controller protobuf generation failed: {error}");
            std::process::exit(1);
        });
}

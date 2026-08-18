fn main() {
    println!("cargo:rerun-if-changed=../../proto/network/v1/network_agent.proto");
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(
            &["../../proto/network/v1/network_agent.proto"],
            &["../../proto"],
        )
        .unwrap_or_else(|error| {
            eprintln!("network protobuf generation failed: {error}");
            std::process::exit(1);
        });
}

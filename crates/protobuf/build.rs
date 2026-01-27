fn main() -> Result<(), Box<dyn std::error::Error>> {
	let proto_file_paths = ["proto/command/v2.proto", "proto/control.proto"];

	let mut config = prost_build::Config::new();

	config.type_attribute("address_type", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddress4", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddress6", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddressPair4", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddressPair6", "#[derive(Eq, Hash)]");
	config.type_attribute("IpV6Address", "#[derive(Eq, Hash)]");
	config.type_attribute("IpV4Address", "#[derive(Eq, Hash)]");

	config.compile_protos(&proto_file_paths, &["."])?;

	for proto_file_path in proto_file_paths {
		println!("cargo:rerun-if-changed={proto_file_path}");
	}

	println!("cargo:rerun-if-changed=build.rs");

	Ok(())
}

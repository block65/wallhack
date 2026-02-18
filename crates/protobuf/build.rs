#[allow(unsafe_code)]
// REASON: build scripts are single-threaded; set_var is safe in this context
fn main() -> Result<(), Box<dyn std::error::Error>> {
	// SAFETY: build scripts are single-threaded, no other threads can race on env
	unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap()) };
	// Step 1: Compile existing protos (no extern_path needed)
	let base_protos = ["proto/command/v2.proto", "proto/control.proto"];

	let mut config = prost_build::Config::new();

	config.type_attribute("address_type", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddress4", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddress6", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddressPair4", "#[derive(Eq, Hash)]");
	config.type_attribute("SocketAddressPair6", "#[derive(Eq, Hash)]");
	config.type_attribute("IpV6Address", "#[derive(Eq, Hash)]");
	config.type_attribute("IpV4Address", "#[derive(Eq, Hash)]");

	// Use bytes::Bytes for data fields to enable O(1) broadcast channel clones
	config.bytes([
		".tunnel.command.v2.TcpSendInstruction.data",
		".tunnel.command.v2.TcpDataRecvResponse.data",
		".tunnel.command.v2.UdpSendInstruction.data",
		".tunnel.command.v2.UdpDataRecvResponse.data",
		".tunnel.command.v2.IcmpEchoRequest.data",
		".tunnel.command.v2.IcmpDataRecvResponse.data",
		".tunnel.command.v2.RawPacket.data",
	]);

	config.compile_protos(&base_protos, &["."])?;

	// Step 2: Compile tunnel_control.proto with extern_path mappings
	// so cross-package references resolve to our Rust module structure
	let mut control_config = prost_build::Config::new();
	control_config.extern_path(".tunnel.command.v2", "crate::v2");
	control_config.extern_path(".tunnel.control.v1", "crate::control");
	control_config.compile_protos(&["proto/tunnel_control.proto"], &["."])?;

	for proto_file_path in base_protos {
		println!("cargo:rerun-if-changed={proto_file_path}");
	}
	println!("cargo:rerun-if-changed=proto/tunnel_control.proto");
	println!("cargo:rerun-if-changed=build.rs");

	Ok(())
}

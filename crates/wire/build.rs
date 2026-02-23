#[allow(unsafe_code)]
// SAFETY: build scripts are single-threaded, no other threads can race on env
fn main() -> Result<(), Box<dyn std::error::Error>> {
	unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap()) };

	// Step 1: Compile data.proto (standalone data plane)
	let mut data_config = prost_build::Config::new();

	data_config.type_attribute("address_type", "#[derive(Eq, Hash)]");
	data_config.type_attribute("SocketAddress4", "#[derive(Eq, Hash)]");
	data_config.type_attribute("SocketAddress6", "#[derive(Eq, Hash)]");
	data_config.type_attribute("SocketAddressPair4", "#[derive(Eq, Hash)]");
	data_config.type_attribute("SocketAddressPair6", "#[derive(Eq, Hash)]");
	data_config.type_attribute("IpV6Address", "#[derive(Eq, Hash)]");
	data_config.type_attribute("IpV4Address", "#[derive(Eq, Hash)]");

	// Use bytes::Bytes for data fields to enable O(1) broadcast channel clones
	data_config.bytes([
		".wallhack.data.TcpSendInstruction.data",
		".wallhack.data.TcpDataRecvResponse.data",
		".wallhack.data.UdpSendInstruction.data",
		".wallhack.data.UdpDataRecvResponse.data",
		".wallhack.data.IcmpEchoRequest.data",
		".wallhack.data.IcmpDataRecvResponse.data",
		".wallhack.data.RawPacket.data",
	]);

	data_config.compile_protos(&["proto/data.proto"], &["."])?;

	// Step 2: Compile control.proto with extern_path for data plane types
	let mut control_config = prost_build::Config::new();
	control_config.extern_path(".wallhack.data", "crate::data");
	control_config.compile_protos(&["proto/control.proto"], &["."])?;

	// Step 3: Compile management.proto (standalone management plane)
	prost_build::Config::new().compile_protos(&["proto/management.proto"], &["."])?;

	println!("cargo:rerun-if-changed=proto/data.proto");
	println!("cargo:rerun-if-changed=proto/control.proto");
	println!("cargo:rerun-if-changed=proto/management.proto");
	println!("cargo:rerun-if-changed=build.rs");

	Ok(())
}

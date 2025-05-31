/// Helper function to convert Vec<u8> to a fixed-size array [u8; N]
/// Creates a zeroed array of size N and copies bytes from the vector into it.
/// If the vector is larger than N, the extra bytes are truncated.
/// If the vector is smaller than N, the remaining bytes in the array stay zero.
pub fn vec_to_sized_array<const N: usize>(vec: &[u8]) -> [u8; N] {
	let mut arr = [0u8; N]; // Create a zeroed array
	let len_to_copy = std::cmp::min(vec.len(), N); // Determine the number of bytes to copy
	arr[..len_to_copy].copy_from_slice(&vec[..len_to_copy]); // Copy bytes from the vector
	arr // Return the array
}

// Define a custom error type for TryFrom conversions using thiserror
#[derive(thiserror::Error, Debug)]
pub enum ConversionError {
	#[error("Invalid message type for conversion")]
	InvalidMessageType,
	#[error("Address type not set in protobuf message")]
	MissingAddressType,
	#[error("Incompatible IP address type for conversion")]
	IncompatibleIpAddressType,
	#[error("Missing ip address")]
	MissingIpAddress,
	#[error("Missing ip address pair")]
	MissingIpAddressPair,
	#[error("Missing addr")]
	MissingAddrInSocketV4Pair,
	#[error("Missing addr")]
	MissingAddrInSocketV6Pair,
	#[error("Invalid socket address pair")]
	InvalidSocketAddrPair,
}

/// Helper function to convert Vec<u8> to a fixed-size array [u8; N].
/// Returns `Err(ConversionError::InvalidLength)` if `vec.len() != N`.
pub fn vec_to_sized_array<const N: usize>(vec: &[u8]) -> Result<[u8; N], ConversionError> {
    if vec.len() != N {
        return Err(ConversionError::InvalidLength {
            expected: N,
            got: vec.len(),
        });
    }
    let mut arr = [0u8; N];
    arr.copy_from_slice(vec);
    Ok(arr)
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
    #[error("Invalid length: expected {expected}, got {got}")]
    InvalidLength { expected: usize, got: usize },
    #[error("Invalid port: value out of u16 range")]
    InvalidPort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_to_sized_array_wrong_length() {
        let result = vec_to_sized_array::<4>(&[1, 2, 3, 4, 5]);
        assert!(matches!(
            result,
            Err(ConversionError::InvalidLength {
                expected: 4,
                got: 5
            })
        ));
    }

    #[test]
    fn vec_to_sized_array_correct_length() {
        let result = vec_to_sized_array::<4>(&[1, 2, 3, 4]);
        assert_eq!(result.unwrap(), [1, 2, 3, 4]);
    }
}

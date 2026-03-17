//! Input validation for API endpoints.
//!
//! Validates user input to prevent injection attacks and ensure data integrity.

use wallhack_core::{Cidr, CidrParseError};

/// Maximum length for peer names.
const MAX_PEER_NAME_LEN: usize = 128;

/// Maximum length for CIDR strings.
const MAX_CIDR_LEN: usize = 64;

/// Validation errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// Input exceeds maximum allowed length.
    TooLong { max: usize, actual: usize },
    /// Input contains invalid characters.
    InvalidCharacters,
    /// CIDR notation is malformed.
    InvalidCidr,
    /// Prefix length is invalid for the IP version.
    InvalidPrefixLength,
    /// Input is empty.
    Empty,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLong { max, actual } => {
                write!(f, "input too long: {actual} bytes (max {max})")
            }
            Self::InvalidCharacters => write!(f, "input contains invalid characters"),
            Self::InvalidCidr => write!(f, "invalid CIDR notation"),
            Self::InvalidPrefixLength => write!(f, "invalid prefix length for IP version"),
            Self::Empty => write!(f, "input cannot be empty"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Validates a peer name.
///
/// Peer names must be:
/// - Non-empty
/// - At most 128 characters
/// - Alphanumeric, hyphens, underscores, colons, and periods only
///   (to support IP:port format and auto-generated names)
///
/// # Errors
///
/// Returns error if the peer name is invalid.
pub fn validate_peer_name(id: &str) -> Result<(), ValidationError> {
    if id.is_empty() {
        return Err(ValidationError::Empty);
    }

    if id.len() > MAX_PEER_NAME_LEN {
        return Err(ValidationError::TooLong {
            max: MAX_PEER_NAME_LEN,
            actual: id.len(),
        });
    }

    // Allow alphanumeric, hyphen, underscore, colon, period, brackets (for IPv6)
    let valid = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '[' | ']'));

    if !valid {
        return Err(ValidationError::InvalidCharacters);
    }

    Ok(())
}

/// Validates a CIDR notation string.
///
/// CIDR must be:
/// - Non-empty
/// - At most 64 characters
/// - Valid IP address followed by `/` and prefix length
/// - Prefix length valid for the IP version (0-32 for IPv4, 0-128 for IPv6)
///
/// # Errors
///
/// Returns error if the CIDR is invalid.
pub fn validate_cidr(cidr: &str) -> Result<(), ValidationError> {
    if cidr.is_empty() {
        return Err(ValidationError::Empty);
    }

    if cidr.len() > MAX_CIDR_LEN {
        return Err(ValidationError::TooLong {
            max: MAX_CIDR_LEN,
            actual: cidr.len(),
        });
    }

    cidr.parse::<Cidr>().map_err(|e| match e {
        CidrParseError::MissingSeparator | CidrParseError::InvalidAddr(_) => {
            ValidationError::InvalidCidr
        }
        CidrParseError::InvalidPrefixLen(_) | CidrParseError::PrefixLenTooLarge { .. } => {
            ValidationError::InvalidPrefixLength
        }
        _ => ValidationError::InvalidCidr,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_peer_names() {
        assert!(validate_peer_name("abc123").is_ok());
        assert!(validate_peer_name("192.168.1.1:8080").is_ok());
        assert!(validate_peer_name("[::1]:8080").is_ok());
        assert!(validate_peer_name("peer-1_test").is_ok());
    }

    #[test]
    fn test_invalid_peer_names() {
        assert_eq!(validate_peer_name(""), Err(ValidationError::Empty));
        assert_eq!(
            validate_peer_name("a; rm -rf /"),
            Err(ValidationError::InvalidCharacters)
        );
        assert_eq!(
            validate_peer_name("$(whoami)"),
            Err(ValidationError::InvalidCharacters)
        );
        assert_eq!(
            validate_peer_name("peer\nid"),
            Err(ValidationError::InvalidCharacters)
        );
        assert_eq!(
            validate_peer_name(&"a".repeat(200)),
            Err(ValidationError::TooLong {
                max: 128,
                actual: 200
            })
        );
    }

    #[test]
    fn test_valid_cidrs() {
        assert!(validate_cidr("10.0.0.0/8").is_ok());
        assert!(validate_cidr("192.168.1.0/24").is_ok());
        assert!(validate_cidr("0.0.0.0/0").is_ok());
        assert!(validate_cidr("255.255.255.255/32").is_ok());
        assert!(validate_cidr("::1/128").is_ok());
        assert!(validate_cidr("fe80::/10").is_ok());
        assert!(validate_cidr("::/0").is_ok());
    }

    #[test]
    fn test_invalid_cidrs() {
        assert_eq!(validate_cidr(""), Err(ValidationError::Empty));
        assert_eq!(validate_cidr("10.0.0.0"), Err(ValidationError::InvalidCidr));
        assert_eq!(
            validate_cidr("not-an-ip/24"),
            Err(ValidationError::InvalidCidr)
        );
        assert_eq!(
            validate_cidr("10.0.0.0/33"),
            Err(ValidationError::InvalidPrefixLength)
        );
        assert_eq!(
            validate_cidr("::1/129"),
            Err(ValidationError::InvalidPrefixLength)
        );
        assert_eq!(
            validate_cidr("10.0.0.0/abc"),
            Err(ValidationError::InvalidPrefixLength)
        );
        // Injection attempts - should fail at some validation point
        assert!(validate_cidr("10.0.0.0/8; rm -rf /").is_err());
        assert!(validate_cidr("$(cat /etc/passwd)/8").is_err());
    }
}

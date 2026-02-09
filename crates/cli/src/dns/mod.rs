mod resolvable;
mod resolver;

pub use resolvable::{ResolvableAddress, parse_str_to_addr};
pub use resolver::{Error as ResolveError, resolve};
//

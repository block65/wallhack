use protobuf::control::NodeRole as ProtoNodeRole;

/// Node role for configuration and identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
	Entry,
	Relay,
	Exit,
}

impl From<NodeRole> for ProtoNodeRole {
	fn from(role: NodeRole) -> Self {
		match role {
			NodeRole::Entry => ProtoNodeRole::RoleEntry,
			NodeRole::Relay => ProtoNodeRole::RoleRelay,
			NodeRole::Exit => ProtoNodeRole::RoleExit,
		}
	}
}

impl TryFrom<ProtoNodeRole> for NodeRole {
	type Error = String;

	fn try_from(role: ProtoNodeRole) -> Result<Self, Self::Error> {
		match role {
			ProtoNodeRole::RoleEntry => Ok(NodeRole::Entry),
			ProtoNodeRole::RoleRelay => Ok(NodeRole::Relay),
			ProtoNodeRole::RoleExit => Ok(NodeRole::Exit),
			ProtoNodeRole::RoleUnknown => Err("unknown node role".to_string()),
		}
	}
}

impl std::fmt::Display for NodeRole {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			NodeRole::Entry => write!(f, "entry"),
			NodeRole::Relay => write!(f, "relay"),
			NodeRole::Exit => write!(f, "exit"),
		}
	}
}

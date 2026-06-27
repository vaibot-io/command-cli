//! The set of governable components the CLI orchestrates. Each maps to a broker
//! credential key + a status key. Today the broker is god-key, so the broker key
//! is informational; it becomes load-bearing when scoped keys land (each
//! component gets a narrow credential minted for its `broker_key()` audience).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Component {
    Guard,
    Gateway,
    /// MCP connection to a named host. When wired, takes an API-key Bearer.
    Mcp(String),
    Api,
}

impl Component {
    /// The audience/scope key the scoped broker will mint a key for.
    pub fn broker_key(&self) -> String {
        match self {
            Component::Guard => "guard".into(),
            Component::Gateway => "gateway".into(),
            Component::Mcp(host) => format!("mcp:{host}"),
            Component::Api => "api".into(),
        }
    }

    /// The key used to report this component's status.
    pub fn status_key(&self) -> String {
        self.broker_key()
    }
}

impl fmt::Display for Component {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.broker_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_host_is_namespaced() {
        assert_eq!(Component::Mcp("openclaw".into()).broker_key(), "mcp:openclaw");
        assert_eq!(Component::Guard.status_key(), "guard");
    }
}

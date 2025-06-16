#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Address {
    pub network_id: u16,
    pub node_id: u8,
    pub port_id: u8,
}

// ---- impl Address ----

impl Address {
    pub const fn unknown() -> Self {
        Self {
            network_id: 0,
            node_id: 0,
            port_id: 0,
        }
    }

    #[inline]
    pub fn net_node_any(&self) -> bool {
        self.network_id == 0 && self.node_id == 0
    }

    #[inline]
    pub fn as_u32(&self) -> u32 {
        ((self.network_id as u32) << 16) | ((self.node_id as u32) << 8) | (self.port_id as u32)
    }

    #[inline]
    pub fn from_word(word: u32) -> Self {
        Self {
            network_id: (word >> 16) as u16,
            node_id: (word >> 8) as u8,
            port_id: word as u8,
        }
    }
}

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Address, AnyAllAppendix, DEFAULT_TTL, FrameKind, Header, Key,
    nash::NameHash,
    net_stack::{NetStackHandle, NetStackSendError},
    traits::Topic,
};

/// A proxy type usable for creating helper services
#[derive(Clone)]
pub struct Topics<NS: NetStackHandle> {
    pub(super) inner: NS,
}

impl<NS: NetStackHandle> Topics<NS> {
    pub fn single_receiver<T>(
        self,
        name: Option<&str>,
    ) -> crate::socket::topic::single::Receiver<T, NS>
    where
        T: Topic,
        T::Message: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::topic::single::Receiver::new(self.inner, name)
    }

    pub fn bounded_receiver<T, const N: usize>(
        self,
        name: Option<&str>,
    ) -> crate::socket::topic::stack_vec::Receiver<T, NS, N>
    where
        T: Topic,
        T::Message: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::topic::stack_vec::Receiver::new(self.inner, name)
    }

    #[cfg(feature = "std")]
    pub fn heap_bounded_receiver<T>(
        self,
        bound: usize,
        name: Option<&str>,
    ) -> crate::socket::topic::std_bounded::Receiver<T, NS>
    where
        T: Topic,
        T::Message: Serialize + DeserializeOwned + Clone,
    {
        crate::socket::topic::std_bounded::Receiver::new(self.inner, bound, name)
    }

    #[cfg(feature = "std")]
    pub fn heap_bounded_borrowed_receiver<T>(
        self,
        bound: usize,
        name: Option<&str>,
        mtu: u16,
    ) -> crate::socket::topic::stack_bor::Receiver<
        crate::interface_manager::utils::std::StdQueue,
        T,
        NS,
    >
    where
        T: Topic,
        T::Message: Serialize + Sized,
    {
        let queue = crate::interface_manager::utils::std::new_std_queue(bound);
        crate::socket::topic::stack_bor::Receiver::new(self.inner, queue, mtu, name)
    }

    /// Send a broadcast message for the topic `T`.
    ///
    /// This message will be sent to all matching local socket listeners, as well
    /// as on all interfaces, to be repeated outwards, in a "flood" style.
    pub fn broadcast<T>(self, msg: &T::Message, name: Option<&str>) -> Result<(), NetStackSendError>
    where
        T: Topic,
        T::Message: Serialize + Clone + DeserializeOwned + 'static,
    {
        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: 0,
            },
            dst: Address {
                network_id: 0,
                node_id: 0,
                port_id: 255,
            },
            any_all: Some(AnyAllAppendix {
                key: Key(T::TOPIC_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: DEFAULT_TTL,
        };
        let stack = self.inner.stack();
        stack.send_ty(&hdr, msg)?;
        Ok(())
    }

    /// Send a broadcast message for the topic `T`.
    ///
    /// This message will be sent to all matching local socket listeners, as well
    /// as on all interfaces, to be repeated outwards, in a "flood" style.
    ///
    /// The same as [Self::broadcast_topic], but accepts messages with borrowed contents.
    /// This may be less efficient when delivering to local sockets.
    pub fn broadcast_borrowed<T>(
        self,
        msg: &T::Message,
        name: Option<&str>,
    ) -> Result<(), NetStackSendError>
    where
        T: Topic + Sized,
        T::Message: Serialize + Sized,
    {
        let hdr = Header {
            src: Address {
                network_id: 0,
                node_id: 0,
                port_id: 0,
            },
            dst: Address {
                network_id: 0,
                node_id: 0,
                port_id: 255,
            },
            any_all: Some(AnyAllAppendix {
                key: Key(T::TOPIC_KEY.to_bytes()),
                nash: name.map(NameHash::new),
            }),
            seq_no: None,
            kind: FrameKind::TOPIC_MSG,
            ttl: DEFAULT_TTL,
        };
        let stack = self.inner.stack();
        stack.send_bor(&hdr, msg)?;
        Ok(())
    }
}

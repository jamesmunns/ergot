/// Generate a combined interface enum from multiple interface types.
///
/// This macro creates:
/// - A zero-sized struct implementing `Interface` (the "interface marker")
/// - An enum implementing `InterfaceSink` whose variants each wrap the
///   `Sink` of the corresponding interface type
///
/// # Example
///
/// ```rust,ignore
/// use ergot::multi_interface;
/// use ergot::interface_manager::interface_impls::embedded_io::IoInterface;
///
/// type Q = &'static Queue<2048, AtomicCoord>;
///
/// multi_interface! {
///     pub enum McSink for McInterface {
///         Usb(UsbInterface<Q>),
///         Uart(IoInterface<Q>),
///         Radio(IoInterface<Q>),
///     }
/// }
///
/// // Now use it:
/// type McStack = NetStack<CSRMutex, NoStdRouter<McInterface, 4>>;
///
/// // Register interfaces with the appropriate variant:
/// stack.manage_profile(|router| {
///     router.register_interface(McSink::Usb(usb_sink));
///     router.register_interface(McSink::Uart(uart_sink));
/// });
/// ```
#[macro_export]
macro_rules! multi_interface {
    (
        $vis:vis enum $sink_name:ident for $iface_name:ident {
            $( $variant:ident ( $iface_ty:ty ) ),+
            $(,)?
        }
    ) => {
        $vis struct $iface_name;

        $vis enum $sink_name {
            $(
                $variant(
                    <$iface_ty as $crate::interface_manager::Interface>::Sink
                ),
            )+
        }

        impl $crate::interface_manager::Interface for $iface_name {
            type Sink = $sink_name;
        }

        #[allow(clippy::result_unit_err)]
        impl $crate::interface_manager::InterfaceSink for $sink_name {
            fn mtu(&self) -> u16 {
                match self {
                    $( Self::$variant(s) => s.mtu(), )+
                }
            }

            fn send_ty<T: ::serde::Serialize>(
                &mut self,
                hdr: &$crate::HeaderSeq,
                body: &T,
            ) -> Result<(), ()> {
                match self {
                    $( Self::$variant(s) => s.send_ty(hdr, body), )+
                }
            }

            fn send_raw(
                &mut self,
                hdr: &$crate::HeaderSeq,
                body: &[u8],
            ) -> Result<(), ()> {
                match self {
                    $( Self::$variant(s) => s.send_raw(hdr, body), )+
                }
            }

            fn send_err(
                &mut self,
                hdr: &$crate::HeaderSeq,
                err: $crate::ProtocolError,
            ) -> Result<(), ()> {
                match self {
                    $( Self::$variant(s) => s.send_err(hdr, err), )+
                }
            }
        }
    };
}

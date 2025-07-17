//! # What can you do with ergot?
//!
//! Let's start by defining a postcard-rpc style `endpoint`: A server that accept requests of
//! one type, and responds with a second type. We can define an endpoint called "double", which will
//! take a `u32`, double it, and return it as a `u64` (to ensure there is no data loss if we pass
//! a large `u32`).
//!
//! ```rust
//! use ergot::{
//!     Address, NetStack, endpoint, interface_manager::null::NullInterfaceManager as NullIM,
//!     socket::endpoint::std_bounded::Server,
//! };
//! use core::pin::pin;
//! use mutex::raw_impls::cs::CriticalSectionRawMutex as CSRMutex;
//!
//! // We can use the `endpoint!` macro to define an endpoint
//! endpoint! {
//!     // The marker type we create for our endpoint, so we can name it
//!     DoubleEndpoint,
//!     // The type of the request, here, a u32
//!     u32,
//!     // The type of the response, here, a u64
//!     u64,
//!     // The name of our endpoint
//!     "double"
//! }
//!
//! // We can now create our network stack
//! static STACK: NetStack<CSRMutex, NullIM> = NetStack::new();
//!
//! // An async fn that serves one endpoint request, then returns
//! async fn serve_once() {
//!     // Create the socket for our request type, pin it, then attach to the netstack,
//!     // Making this socket available to receive requests. A port is automatically
//!     // assigned for this socket.
//!     let server_skt = STACK.std_bounded_endpoint_server::<DoubleEndpoint>(16, None);
//!     let server_skt = pin!(server_skt);
//!     let mut hdl = server_skt.attach();
//!
//!     // Serve one request, doubling the request and returning it. The async closure
//!     // is called when a request is successfully received. The response is automatically
//!     // sent back to the source address of the requestor. Normally, you'd want to do
//!     // this in a loop, for as long as you want your server to exist, potentially forever!
//!     hdl.serve(async |p: &u32| {
//!         let p: u64 = (*p) as u64;
//!         2 * p
//!     })
//!     .await
//!     .unwrap();
//!
//!     // When the socket falls out of scope, it is automatically detached from the network
//!     // stack, and packets to that address will no longer be accepted.
//! }
//!
//! // An async fn that makes one Endpoint request, and waits for the response
//! async fn client_once() {
//!     # // Make sure we let the server start first
//!     # tokio::time::sleep(core::time::Duration::from_millis(50)).await;
//!     // Send a request via the network stack. We set the destination address as
//!     // "unknown", which will cause `ergot` to see if there is one (and only one)
//!     // socket offering this endpoint on the local machine.
//!     let res = STACK
//!         .req_resp::<DoubleEndpoint>(Address::unknown(), &42u32, None)
//!         .await;
//!
//!     // Did it double?
//!     assert_eq!(res, Ok(84u64));
//! }
//!
//! // Run the test, wait for our client and server to complete
//! #[tokio::main]
//! async fn main() {
//!     let hdl_srv = tokio::task::spawn(serve_once());
//!     let hdl_cli = tokio::task::spawn(client_once());
//!     hdl_srv.await.unwrap();
//!     hdl_cli.await.unwrap();
//! }
//! ```
//!
//! ## Whew, that was a lot at once!
//!
//! It sure was! Let's explain that (later)

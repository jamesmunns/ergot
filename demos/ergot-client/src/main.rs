use ergot::{
    NetStack,
    interface_manager::std_tcp_client::{StdTcpClientIm, register_interface},
    socket::endpoint::StdBoundedEndpointSocket,
    well_known::ErgotPingEndpoint,
};
use mutex::raw_impls::cs::CriticalSectionRawMutex;
use tokio::net::TcpStream;

use std::{io, pin::pin, time::Duration};

// Client
static STACK: NetStack<CriticalSectionRawMutex, StdTcpClientIm> = NetStack::new();

#[tokio::main]
async fn main() -> io::Result<()> {
    let socket = TcpStream::connect("127.0.0.1:2025").await.unwrap();

    tokio::task::spawn(pingserver());

    let hdl = register_interface(STACK.base(), socket).unwrap();
    tokio::task::spawn(async move {
        hdl.run().await.unwrap();
    });
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn pingserver() {
    let server = StdBoundedEndpointSocket::<ErgotPingEndpoint, _, _>::new(STACK.base(), 16);
    let server = pin!(server);
    let mut server_hdl = server.attach();
    loop {
        server_hdl
            .serve(async |req| {
                println!("Serving ping {req}");
                req
            })
            .await
            .unwrap();
    }
}

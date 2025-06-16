use postcard_rpc::endpoint;

endpoint!(ErgotPingEndpoint, u32, u32, "ergot/.well-known/ping");

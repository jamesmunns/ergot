# Ergot Nusb Demo

This is a demo of running ergot on an std system while communicating to the rest
of the network over usb, when running this as a demo, the following logging
arguments are recommended

```
RUST_LOG=debug,nusb=info cargo run
```

Without them you worn't see the pings flying back and forth between the 2 nodes
in the network. And we don't let nusb be in debug mode cause its very very loud
in debug mode

## Partner demo workspaces

A good partner demo for this demo would be any of the MCU demo workspaces that
end in "-eusb"

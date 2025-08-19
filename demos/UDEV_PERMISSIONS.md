# Udev Permissions

## Demo Permissions

All of the demo projects in this repo uses VENDER_ID=16c0 and PRODUCT_ID=27dd,
this makes it incredible simple to setup dev udev rules like so

```
ACTION=="add|change", SUBSYSTEM=="usb|tty|hidraw", ATTRS{idVendor}=="16c0", ATTRS{idProduct}=="27dd", MODE="0666", GROUP="plugdev", TAG+="uaccess"
```

Remember for any real project where you might wanna change the vender and
product id to also make corresponding udev rules

## picotool

If you need to use `picotool` the follow udev rule makes that work without
needing sudo

```
SUBSYSTEM=="usb", ATTRS{idVendor}=="2e8a", ATTRS{idProduct}=="0003", MODE="0666"
```

## probe-rs

The current well-maintained list of udev rules for probe-rs can be found
[here](https://probe.rs/files/69-probe-rs.rules)

# Notes

Okay, I have the basics working.

I need two main groups of interfaces:

* Controlling/Monitoring the connection
    * Getting state
    * Setting configuration
    * notifications about state changes?
* Proxying the Device impl
    * Putting a packet (and maybe syncing the state?)
    * Getting a packet (and maybe syncing the state?)
    * Link state, getting and listening for changes
    * Capabilities (maybe doesn't change?)
    * Hardware address, getting, and maybe updating on link state change

For a lot of this we could maybe do it in a very dumb way, and just always assume there is space?
This could be bad and lead to dropped frames?

We also IDEALLY would synchronize across the two groups: the Device impl SHOULD be sessionful,
and we should drop our "session" if the connection is dropped.

For putting a packet, we should probably listen for "space available" notifications, maybe only if
we try sending and it fails. This might want to be an endpoint so we can check if it fails.

For getting a packet, we should probably listen for "packet available" notifications, maybe only if
we try recv'ing and it fails.

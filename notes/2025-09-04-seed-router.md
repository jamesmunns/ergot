* Seed Router
    * Net ID 1
        * Edge Device
    * Net ID 2
        * Bridge device
            * Net ID ???
                * Edge Device


* Net ID 1 comes online
    * Seed Router: 1.1
    * Edge Device: 1.2
* Net ID 2 comes online
    * Seed Router 2.1
    * Bridge Device: 2.2


* Bridge device comes online, both interfaces local only/down
    * Interface 1: Uplink
    * Interface 2: Downlink
* Interface 1 comes online, assigned Net ID 2
* Something (Interface 2's worker?, service?) notices this
* We request a net id assignment
    * Open topic sub for assignments on port 5
    * Send a BROADCAST like "please assign me a net id" (on loop?)
    * This gets sent out of interface 1, src is updated to 2.2:5
    * Delivered to Seed router at 2.1:255, src is 2.2:5
* Seed router accepts message
    * Send a UNICAST like:
        * To 2.2:5
        * Renewal service at 2.1:3
        * Assigning Net ID 3 to 2.2
        * Expires in 30s, max renewal 120s
    * Above details stored in routing table
* Bridge device gets message
    * Sets interface 2 to net id 3
    * immediately notices expiration is <1/2 max
    * Send renewal endpoint request to 2.1:3, requesting 120s
* Seed router accepts message
    * updates expiration to 120s
    * Sends accept message
* Bridge device gets message

* If Net ID 2 is dropped from the routing table, so is Net ID 3
    * This probably works now since 2 is direct
    * Does this also work if 2 wasn't direct?
* On expiration, we send some kind of "evict" message?

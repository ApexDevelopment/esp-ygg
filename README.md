# esp-ygg

Yggdrasil on ESP32 chips, with ESP-NOW as the link between nodes.

Each board is a full Yggdrasil node with its own key and address. Boards in radio range find each other and peer automatically, and route traffic for each other like any other Yggdrasil nodes. A board can also join a WiFi network and peer over TCP with a regular Yggdrasil node, which connects the ESP-NOW mesh to the rest of the network.

A board answers pings to its address and responds to `yggdrasilctl getNodeInfo` and the `debug_remoteGet*` requests. It does not run an IP stack beyond that.

It has been tested on two ESP32-S3 boards (M5Stack Cardputers), one acting as a gateway to a yggdrasil-rs node over WiFi. The other chips build but have not been tried.

## Layout

`node` is the Yggdrasil node itself, as a `no_std` library that does no I/O. It builds and runs on a desktop, which is how it is tested. `firmware` wraps it for the ESP32 using esp-hal, esp-radio and Embassy.

The node depends on `petrified`, `yggdrasil-rs` and `espnow-reliable` through relative paths, so clone those next to this repository.

## Building and flashing

The ESP32-S3 and the original ESP32 use Xtensa cores, which need Espressif's Rust toolchain. Install it once with `espup`, then load its environment in each shell you build from:

```sh
cargo install espup espflash
espup install
source ~/export-esp.sh
```

Then, with a board plugged in, run one of these from the `firmware` directory:

```sh
cargo s3     # ESP32-S3
cargo c3     # ESP32-C3
cargo c6     # ESP32-C6
cargo esp32  # original ESP32
```

Each builds a release image, flashes it with espflash and opens the serial monitor. The log shows the board's public key and address on startup.

On boards that use the ESP32-S3's built-in USB port, the reset after flashing sometimes leaves the chip waiting for another download, and the monitor shows `waiting for download`. Press the board's reset button to start the firmware. If the board has a button on GPIO0, such as the Cardputer's G0, do not hold it while resetting.

On first boot, the board creates a private key and saves it in the flash `nvs` partition. It keeps that key across reflashing. To give a board a new identity, erase its flash with `espflash erase-flash`.

## Settings

Settings are read from environment variables when the firmware is built.

| Variable | Default | Meaning |
|---|---|---|
| `ESP_YGG_CHANNEL` | `1` | WiFi channel for ESP-NOW |
| `ESP_YGG_NAME` | none | `name` field in the board's nodeinfo |
| `ESP_YGG_PASSWORD` | none | Link password. Boards only peer with others that use the same one. |
| `ESP_YGG_WIFI_SSID` | none | WiFi network to join for a TCP peering |
| `ESP_YGG_WIFI_PASSWORD` | none | Its WPA2 password. Leave unset for an open network. |
| `ESP_YGG_PEER` | none | Address of the node to peer with over TCP, as an IPv4 address and port |

A board peers over TCP only when both `ESP_YGG_WIFI_SSID` and `ESP_YGG_PEER` are set. For example:

```sh
ESP_YGG_WIFI_SSID=home ESP_YGG_WIFI_PASSWORD=secret ESP_YGG_PEER=192.168.1.10:9001 cargo s3
```

The node at that address needs a TCP listener in its config, such as `Listen: ["tcp://0.0.0.0:9001"]`.

A board on WiFi uses the access point's channel for ESP-NOW too, because the radio can only be on one channel at a time. Build the other boards with `ESP_YGG_CHANNEL` set to that channel, or they will not hear it.

## Trying it out

From a computer whose Yggdrasil node peers with a board, directly or through other boards:

```sh
ping <board address>
yggdrasilctl getNodeInfo key=<board public key>
```

The first request after the boards come up can take several seconds while routes form. After that, a ping across a gateway and one ESP-NOW hop typically takes 30 to 70 ms.

Radio trouble shows up in the serial log. Every 10 seconds in which some ESP-NOW sends failed, the board logs how many were sent, refused by the radio, not acknowledged, or not reported on in time, along with the lowest free heap. A few failures are normal and are resent.

## Limits

A board peers with at most six ESP-NOW neighbors at once.

The board has no real-time clock. Yggdrasil numbers some messages with the time, and other nodes ignore numbers that go backwards, so the board saves its clock to flash every hour and starts each boot an hour past the last save. Its clock runs ahead of real time as a result, which does no harm.

## Testing

Run `cargo test` in this directory. The tests simulate several nodes sharing a radio, including lost frames and nodes moving out of range. One test also peers a simulated board with a yggdrasil-rs node over TCP and pings across both links.

## License

The code is licensed under the GNU Lesser General Public License version 3, with the same exception for static linking that yggdrasil-go uses. See [LICENSE](LICENSE).

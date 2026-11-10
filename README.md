# esp-ygg

Yggdrasil on ESP32 chips, with ESP-NOW as the link between nodes.

Each board is a full Yggdrasil node with its own key and address. Boards in radio range find each other and peer automatically, and route traffic for each other like any other Yggdrasil nodes. A board can also join a WiFi network and peer over TCP with a regular Yggdrasil node, which connects the ESP-NOW mesh to the rest of the network.

Every board runs a few services on its Yggdrasil address:

- an HTTP status page on port 80, with its address, uptime, memory and peers
- a Gopher menu on port 70 with the same facts, linking to its peers' menus
- a UDP console on port 4242 that shows text it receives and sends text back
- the time over NTP on UDP port 123, once it knows it
- replies to pings, `yggdrasilctl getNodeInfo` and the `debug_remoteGet*` requests

On an M5Stack Cardputer or Cardputer ADV, the screen and keyboard give an interface for browsing the network, pinging nodes, using the console and changing settings.

The mesh, the gateway and the services have been tested on two ESP32-S3 boards (M5Stack Cardputers), one acting as a gateway to a yggdrasil-rs node over WiFi. The on-device interface has only run in the desktop preview so far. The other chips build but have not been tried.

## Layout

`node` is the Yggdrasil node itself, as a `no_std` library that does no I/O. `app` adds the services and the on-device interface on top, also without touching hardware. Both build and run on a desktop, which is how they are tested. `firmware` wraps them for the ESP32 using esp-hal, esp-radio and Embassy.

## Building and flashing

The ESP32-S3 and the original ESP32 use Xtensa cores, which need Espressif's Rust toolchain. Install it once with `espup`, then load its environment in each shell you build from:

```sh
cargo install espup espflash
espup install
source ~/export-esp.sh
```

Then, with a board plugged in, run one of these from the `firmware` directory:

```sh
cargo cardputer  # M5Stack Cardputer or Cardputer ADV
cargo s3     # ESP32-S3
cargo c3     # ESP32-C3
cargo c6     # ESP32-C6
cargo esp32  # original ESP32
```

Each builds a release image, flashes it with espflash and opens the serial monitor. The log shows the board's public key and address on startup.

On boards that use the ESP32-S3's built-in USB port, the reset after flashing sometimes leaves the chip waiting for another download, and the monitor shows `waiting for download`. Press the board's reset button to start the firmware. If the board has a button on GPIO0, such as the Cardputer's G0, do not hold it while resetting.

On first boot, the board creates a private key and saves it in the flash `nvs` partition. It keeps that key across reflashing. To give a board a new identity, erase its flash with `espflash erase-flash`.

## Choosing services

Each service costs memory while the board runs, so the firmware can be built with or without each one. They are Cargo features:

| Feature | Service | Default |
|---|---|---|
| `http` | HTTP status page | on |
| `console` | UDP console, and the Console screen | on |
| `ntp` | Time over NTP, and the clock | on |
| `gopher` | Gopher menu, and the Gopher screen | off |
| `irc` | The IRC screen | off |
| `gemini` | The Gemini screen | off |

To add the larger ones, list them along with the board:

```sh
cargo run --release --features cardputer,gopher,irc,gemini
```

To leave out default ones too, turn off the defaults and list everything you want. For example, a Cardputer with only the console:

```sh
cargo run --release --no-default-features --features cardputer,console
```

Clients take memory only while they are in use, so most of the cost of a service is its server, if it has one. A Cardputer ADV acting as a gateway with every service has about 70 KB free.

## On-device interface

On a Cardputer, the home screen shows the board's address and how many nodes it knows, with a menu below:

- Network lists every node the board knows, direct peers first, named from their nodeinfo. Open one to see its details, then press `p` to ping it or `m` to send it a console message.
- Radio lists ESP-NOW neighbors with their recent signal strength and the share of frames sent to them that went unacknowledged, for finding where boards still reach each other.
- Ping sends a ping every second to an address you type or pick, and shows each round trip.
- Gopher browses Gopher menus and text, starting from an address you type. Links to a board's peers make it easy to hop across the mesh. Yggdrasil has no standard way to look up names, so links work when they use an address or the name of the server they came from.
- Gemini browses Gemini capsules, starting from an address you type. Up and down move between links, scrolling past text where there are none, and Enter follows the selected one. It answers questions servers ask and follows redirects. Gemini servers mostly sign their own certificates, so the browser trusts the certificate a server shows first and refuses a different one until the board restarts. Going back fetches the page again, to save memory.
- IRC connects to an IRC server over Yggdrasil and joins a channel. Each channel and private conversation gets its own buffer, and Tab moves between them. The usual commands work, such as `/join`, `/part`, `/msg`, `/me`, `/nick` and `/quit`. Leaving the screen keeps the connection, and the home menu counts new lines.
- Console shows text received on UDP port 4242 and sends what you type, to an address or `[address]:port`.
- Settings changes the name, bridging, WiFi network, password, peer, channel and time zone, and turns the status pages on or off. Saving restarts the board to apply them.

Once the board knows the time, the title bar shows it. A bridged board gets the time from `pool.ntp.org`, and other boards ask their peers over Yggdrasil, so it spreads through the mesh. The time zone is a fixed offset from UTC, so it needs changing by hand for daylight saving time.

In fields that take an address, typing the start of a known node's address or name and pressing Tab fills in its address, and pressing Tab again moves to the next node that matches. Names come from nodeinfo, which the board looks up while these screens are open.

The arrow keys are Fn with `;` `.` `,` and `/`, and Escape is Fn with `` ` ``. In lists, the same keys work without Fn.

To try the interface without a board, run `cargo run -p esp-ygg-app --example preview`. It shows the interface in a terminal as one of three simulated boards in a line, where the farthest one echoes console messages back.

## Settings

A Cardputer stores the settings chosen in its interface in flash. Until then, and on boards without a screen, settings come from environment variables read when the firmware is built.

| Variable | Default | Meaning |
|---|---|---|
| `ESP_YGG_CHANNEL` | `1` | WiFi channel for ESP-NOW |
| `ESP_YGG_NAME` | none | `name` field in the board's nodeinfo |
| `ESP_YGG_PASSWORD` | none | Link password. Boards only peer with others that use the same one. |
| `ESP_YGG_WIFI_SSID` | none | WiFi network to join for a TCP peering |
| `ESP_YGG_WIFI_PASSWORD` | none | Its WPA2 password. Leave unset for an open network. |
| `ESP_YGG_PEER` | none | Address of the node to peer with over TCP, as an IPv4 address and port |
| `ESP_YGG_STATUS_PAGE` | `on` | Set to `off` to close the HTTP and Gopher ports |

A board peers over TCP only when both `ESP_YGG_WIFI_SSID` and `ESP_YGG_PEER` are set. For example:

```sh
ESP_YGG_WIFI_SSID=home ESP_YGG_WIFI_PASSWORD=secret ESP_YGG_PEER=192.168.1.10:9001 cargo s3
```

The WiFi password is stored in flash as plain text. The node at that address needs a TCP listener in its config, such as `Listen: ["tcp://0.0.0.0:9001"]`.

A board on WiFi uses the access point's channel for ESP-NOW too, because the radio can only be on one channel at a time. Build the other boards with `ESP_YGG_CHANNEL` set to that channel, or they will not hear it.

## Trying it out

From a computer whose Yggdrasil node peers with a board, directly or through other boards:

```sh
ping <board address>
yggdrasilctl getNodeInfo key=<board public key>
curl http://[<board address>]/
curl gopher://[<board address>]/
nc -6u <board address> 4242
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

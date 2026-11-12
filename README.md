# esp-ygg

Yggdrasil on ESP32 chips, with ESP-NOW as the link between nodes.

Each board is a full Yggdrasil node with its own key and address. Boards in radio range find each other and peer automatically, and route traffic for each other like any other Yggdrasil nodes. A board can also join a WiFi network and peer over TCP with a regular Yggdrasil node, which connects the ESP-NOW mesh to the rest of the network.

Every board runs a few services on its Yggdrasil address:

- an HTTP status page on port 80, with its address, uptime, memory and peers
- a Gopher menu on port 70 with the same facts, linking to its peers' menus
- a UDP console on port 4242 that shows text it receives and sends text back
- the time over NTP on UDP port 123, once it knows it
- replies to pings, `yggdrasilctl getNodeInfo` and the `debug_remoteGet*` requests

On an M5Stack Cardputer or Cardputer ADV, the screen and keyboard give an interface for browsing the network, pinging nodes, using the console and changing settings. The Waveshare ESP32-C6-Touch-LCD-2.8 shows the same interface on its touch screen.

The mesh, the gateway and the services have been tested on two ESP32-S3 boards (M5Stack Cardputers), one acting as a gateway to a yggdrasil-rs node over WiFi. The on-device interface has only run in the desktop preview so far. An ESP32-C3 and an ESP32-C6 have also booted and peered with each other over ESP-NOW, and the interface runs on the Waveshare board's touch screen. The original ESP32 builds but has not been tried.

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
cargo c6-touch  # Waveshare ESP32-C6-Touch-LCD-2.8
cargo c6-gui    # the same, with the graphical interface
cargo esp32  # original ESP32
```

Each builds a release image, flashes it with espflash and opens the serial monitor. The log shows the board's public key and address on startup.

`firmware/espflash.toml` sets the flash to quad I/O at 80 MHz, which the Stamp-S3 in the Cardputers supports. The firmware runs its code straight from flash, so this makes everything about four times faster than espflash's default of dual I/O at 40 MHz. Flash from the `firmware` directory so espflash finds it. A board whose flash cannot run that fast will not boot, so remove the file for such boards.

On boards that use the ESP32-S3's built-in USB port, the reset after flashing sometimes leaves the chip waiting for another download, and the monitor shows `waiting for download`. Press the board's reset button to start the firmware. If the board has a button on GPIO0, such as the Cardputer's G0, do not hold it while resetting.

On first boot, the board creates a private key and saves it in the flash `nvs` partition. It keeps that key across reflashing. To give a board a new identity, erase its flash with `espflash erase-flash`.

## Choosing an interface

A board's feature, `cardputer` or `c6-touch-lcd`, says which hardware it is, and `tui` or `gui` adds an interface on its screen: `tui` for the text one, and `gui` for the graphical one, which only the touch board has. The aliases above pick these for you. A board built without either runs as a node alone, with its screen off, which leaves the most memory for the network.

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
| `voice` | Voice calls, on the Cardputers | off |

To add the larger ones, list them along with the board:

```sh
cargo run --release --features cardputer,tui,gopher,irc,gemini
```

To leave out default ones too, turn off the defaults and list everything you want. For example, a Cardputer with only the console:

```sh
cargo run --release --no-default-features --features cardputer,tui,console
```

Clients take memory only while they are in use, so most of the cost of a service is its server, if it has one. A Cardputer ADV acting as a gateway with every service has about 70 KB free.

## On-device interface

On a board with a screen, the home screen shows the board's address and how many nodes it knows, with a menu below:

- Yggdrasil Nodes lists every node the board knows, direct peers first, named from their nodeinfo. Open one to see its details, then press `p` to ping it or `m` to send it a console message.
- ESP-NOW Neighbors lists the boards in radio range, with their recent signal strength and the share of frames sent to them that went unacknowledged, for finding where boards still reach each other.
- Ping sends a ping every second to an address you type or pick, and shows each round trip.
- Apps opens a menu of the apps below, and counts the new messages waiting in them:
  - Gopher browses Gopher menus and text, starting from an address you type. Links to a board's peers make it easy to hop across the mesh. Yggdrasil has no standard way to look up names, so links work when they use an address or the name of the server they came from.
  - Gemini browses Gemini capsules, starting from an address you type. Up and down move between links, scrolling past text where there are none, and Enter follows the selected one. It answers questions servers ask and follows redirects. Gemini servers mostly sign their own certificates, so the browser trusts the certificate a server shows first and refuses a different one until the board restarts. Going back fetches the page again, to save memory.
  - IRC connects to an IRC server over Yggdrasil and joins a channel. Each channel and private conversation gets its own buffer, and Tab moves between them. The usual commands work, such as `/join`, `/part`, `/msg`, `/me`, `/nick` and `/quit`. Leaving the screen keeps the connection, and the menu counts new lines.
  - UDP Console shows text received on UDP port 4242 and sends what you type, to an address or `[address]:port`.
- Settings changes the name, channel and time zone, turns the status pages on or off, and sets up the bridge and the hotspot. Voice builds add the ringer, with five ringtones and five volume steps, each played briefly as it changes. Saving restarts the board to apply them. Settings can also be exported to and imported from `esp-ygg/settings.txt` on a FAT32 microSD card, as `key = value` lines that are easy to edit on a computer, with passwords in plain text. A file may leave out settings it does not change, and importing fills in the settings screen without saving.

Once the board knows the time, the title bar shows it. A bridged board gets the time from `pool.ntp.org`, and other boards ask their peers over Yggdrasil, so it spreads through the mesh. The time zone is a fixed offset from UTC, so it needs changing by hand for daylight saving time.

In fields that take an address, typing the start of a known node's address or name and pressing Tab fills in its address, and pressing Tab again moves to the next node that matches. Names come from nodeinfo, which the board looks up while these screens are open.

The arrow keys are Fn with `;` `.` `,` and `/`, and Escape is Fn with `` ` ``. In lists, the same keys work without Fn.

On the touch screen, dragging up or down moves the selection a row at a time, following the finger. A tap is Enter, and a swipe to the right or the BOOT button is Escape. When a tap opens a text field, a keyboard comes up on the lower half of the screen, with `123` for digits and symbols and `▼` to put it away. Tapping anywhere above it while the field is still open brings it back. Fields that only take numbers, such as the channel, time zone and peer, get a number pad of two rows instead, which leaves more of the screen to see.

The interface can also be driven from a computer over the USB serial port, for testing without someone at the keyboard. `firmware/drive.py` sends lines such as `type 200::1`, `key down enter`, `tap 5 2`, which taps a touch screen at a column and row of characters, and `screen`, which prints what the screen shows, and it does so without resetting the board:

```sh
python3 firmware/drive.py /dev/ttyACM0 "key down down enter" "type 200::1" "key enter" "wait 3" screen
```

In voice builds, `talk on` and `talk off` hold and release push-to-talk, and `mute on` silences the speaker for quiet testing.

To try the interface without a board, run `cargo run -p esp-ygg-app --example preview`. It shows the interface in a terminal as one of three simulated boards in a line, where the farthest one echoes console messages back. Add `-- touch` for the touch board's larger screen, where mouse clicks are taps.

The preview can also draw the touch board's screen pixel for pixel, as the board does, and save pictures of it. It takes the same commands as `drive.py`, and saves a PNG at each `screen`:

```sh
cargo run -p esp-ygg-app --example preview -- png shots "key down down down down enter" "tap 5 2" screen
```

## Graphical interface

The touch board can show a graphical interface instead of the text one, made with [Slint](https://slint.dev) and its Material 3 components. The `tui` and `gui` features choose between them, as `cargo c6-touch` and `cargo c6-gui` do. It is at an early stage: a first screen, not yet connected to the node.

Slint is licensed under the GPLv3 on microcontrollers, so firmware built with `gui` is distributed under the GPLv3. The rest of the project, and every other build, stays under the LGPLv3. The Material components in `gui/material-1.1.0` are under the MIT license.

To see the interface without a board, draw it as the board does and save a picture:

```sh
cargo run -p esp-ygg-gui --example screenshot -- screen.png
```

## Voice calls

With the `voice` feature, the Voice Call (IAX2) app calls other boards by address, with Tab completion like the other address fields. Calls use IAX2, the protocol Asterisk speaks, on UDP port 4569, so other IAX2 software over Yggdrasil should be able to call in too. Yggdrasil already encrypts and authenticates traffic by address, so calls use no IAX2 passwords.

An incoming call rings and brings up the call screen from anywhere. Enter answers and Escape declines or hangs up. Talking is push-to-talk, like a walkie-talkie: hold G0 on the side of the board to talk, and let go to listen. Outside a call, holding G0 records a few seconds and plays them back on release, to check the speaker and microphone.

Calls prefer G.726 at 32 kbit/s, which fits each 20 ms of voice in a single ESP-NOW frame, and fall back to G.711 μ-law for software without it. The G.726 codec matches Sun's reference implementation, which Asterisk's is built on, bit for bit. Asterisk swapped the IAX2 format bits for G.726's two byte packings from what RFC 5456 lists, and the boards follow Asterisk, offering both packings.

A computer on the same Yggdrasil network can call boards and take their calls with the `phone` example. It reads and writes raw audio, so ALSA's tools make it a softphone:

```sh
arecord -q -t raw -f S16_LE -r 8000 -c 1 \
  | cargo run -p esp-ygg-app --features voice --example phone -- call <board address> \
  | aplay -q -t raw -f S16_LE -r 8000 -c 1
```

`phone answer` instead waits for a board to call the computer. It talks continuously rather than push-to-talk, so use headphones.

Asterisk can take the boards' calls too. They call without a username, so Asterisk treats them as its `guest` user. Asterisk's IAX2 dial strings cannot hold an IPv6 address, so call a board through a named peer:

```ini
; iax.conf
[general]
bindaddr=::
disallow=all
allow=g726
allow=ulaw

[guest]
type=user
context=boards

[kitchen]
type=peer
host=<board address>
```

Then `Dial(IAX2/kitchen/s)` rings the board.


A board with a screen stores the settings chosen in its interface in flash. Until then, and on boards without a screen, settings come from environment variables read when the firmware is built.

| Variable | Default | Meaning |
|---|---|---|
| `ESP_YGG_CHANNEL` | `1` | WiFi channel for ESP-NOW |
| `ESP_YGG_NAME` | none | `name` field in the board's nodeinfo |
| `ESP_YGG_PASSWORD` | none | Link password. Boards only peer with others that use the same one. |
| `ESP_YGG_WIFI_SSID` | none | WiFi network to join for a TCP peering |
| `ESP_YGG_WIFI_PASSWORD` | none | Its WPA2 password. Leave unset for an open network. |
| `ESP_YGG_PEER` | none | Address of the node to peer with over TCP, as an IPv4 address and port |
| `ESP_YGG_STATUS_PAGE` | `on` | Set to `off` to close the HTTP and Gopher ports |
| `ESP_YGG_HOTSPOT` | none | Name of a WiFi network for the board to run as a hotspot |
| `ESP_YGG_HOTSPOT_PASSWORD` | none | Its WPA2 password, 8 to 63 characters. Leave unset for an open network. |

A board peers over TCP only when both `ESP_YGG_WIFI_SSID` and `ESP_YGG_PEER` are set. For example:

```sh
ESP_YGG_WIFI_SSID=home ESP_YGG_WIFI_PASSWORD=secret ESP_YGG_PEER=192.168.1.10:9001 cargo s3
```

The WiFi password is stored in flash as plain text. The node at that address needs a TCP listener in its config, such as `Listen: ["tcp://0.0.0.0:9001"]`.

A board on WiFi uses the access point's channel for ESP-NOW too, because the radio can only be on one channel at a time. Build the other boards with `ESP_YGG_CHANNEL` set to that channel, or they will not hear it.

## Hotspot

The bridge and the hotspot both use WiFi, in opposite directions. The bridge joins an existing WiFi network to peer with a node on it. The hotspot is a WiFi network the board runs itself, so phones and laptops can reach Yggdrasil without installing anything. A board can do both at once.

Devices that join the hotspot get an address in the board's `300:` subnet and a route to the rest of Yggdrasil through the board. They can then reach any Yggdrasil address, such as the status page of a board across the mesh. The hotspot only carries Yggdrasil traffic. It has no IPv4 and no internet, so phones may warn that the network has no internet and need telling to stay connected. Laptops on another network as well keep using it for everything else.

The hotspot runs on the board's ESP-NOW channel, or on the bridge's network's channel when bridging. NetworkManager installs the route to `200::/7` that the board advertises. Where the Linux kernel handles advertisements itself, it ignores that route unless `net.ipv6.conf.<interface>.accept_ra_rt_info_max_plen` is at least 7, which matters on a laptop that has IPv6 from another network as well.

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

On a board with a screen, ESP-NOW Neighbors shows each neighbor's signal and the share of frames it did not acknowledge. A few failures are normal and are resent.

## Limits

A board peers with at most six ESP-NOW neighbors at once.

The board has no real-time clock. Yggdrasil numbers some messages with the time, and other nodes ignore numbers that go backwards, so the board saves its clock to flash every hour and starts each boot an hour past the last save. Its clock runs ahead of real time as a result, which does no harm.

## Testing

Run `cargo test` in this directory. The tests simulate several nodes sharing a radio, including lost frames and nodes moving out of range. One test also peers a simulated board with a yggdrasil-rs node over TCP and pings across both links.

## License

The code is licensed under the GNU Lesser General Public License version 3, with the same exception for static linking that yggdrasil-go uses. See [LICENSE](LICENSE).

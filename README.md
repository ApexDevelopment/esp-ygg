# esp-ygg

esp-ygg turns ESP32 boards into Yggdrasil nodes that link to each other over ESP-NOW, Espressif's protocol for sending WiFi frames straight between chips without an access point.

Flash it onto a few boards and they find each other in radio range, peer, and route traffic for each other like any other Yggdrasil nodes, each with its own key and address. To connect the mesh to the rest of Yggdrasil, have one board join your WiFi network and peer with a regular Yggdrasil node. A board can also run a WiFi hotspot, so your phone or laptop can reach Yggdrasil through it without installing anything.

You can reach these services on each board's Yggdrasil address:

- an HTTP status page on port 80, with the board's address, uptime, memory and peers
- a Gopher menu on port 70 with the same facts, linking to the menus of its peers
- a UDP console on port 4242, which shows text it receives and sends text back
- the time over NTP on UDP port 123, once the board knows it
- answers to ping, `yggdrasilctl getNodeInfo` and the `debug_remoteGet*` requests

On a board with a screen, you can also browse the network and ping nodes. Its apps let you chat over the console or IRC, read Gopher and Gemini pages, trade files over SFTP and, on the Cardputer, make voice calls.

esp-ygg has run on the M5Stack Cardputer and Cardputer ADV (ESP32-S3), the Waveshare ESP32-C6-Touch-LCD-2.8, an ESP32-C3 and an ESP32-C6. It builds for the original ESP32, but has not been tried on one.

## Build and flash

You need Espressif's Rust toolchain and espflash. Install them once:

```sh
cargo install espup espflash
espup install
```

The ESP32-S3 and the original ESP32 use Xtensa cores, whose linker comes from that toolchain. Before you build for them, load its environment in your shell:

```sh
source ~/export-esp.sh
```

Plug in your board, and from the `firmware` directory, run `build.py` with the name of your board:

```sh
cd firmware
python3 build.py cardputer --flash --monitor
```

| Name | Board |
|---|---|
| `cardputer` | M5Stack Cardputer or Cardputer ADV, with the text interface |
| `voice` | the same, with voice calls |
| `c6-touch` | Waveshare ESP32-C6-Touch-LCD-2.8, with the text interface |
| `c6-gui` | the same, with the graphical interface |
| `s3` | any ESP32-S3, as a node without a screen |
| `c3` | any ESP32-C3, as a node without a screen |
| `c6` | any ESP32-C6, as a node without a screen |
| `esp32` | any original ESP32, as a node without a screen |

`--flash` writes the build to the board, and `--monitor` then shows its log, which starts with the board's public key and address. Add `--port /dev/ttyACM0`, or whichever port the board is on, if espflash cannot tell. Without `--flash`, the script builds and nothing more.

Each build lands in `firmware/target/images/<name>`, with any extra features you add in the folder's name, such as `cardputer+gopher+irc`. The folder holds:

- `esp-ygg.elf`, which espflash flashes and which you need to decode a backtrace
- `esp-ygg.bin`, the app image, for launchers and other flashing tools
- `esp-ygg-merged.bin`, when you add `--merged`, with the bootloader and partition table as well, to write at address 0

If the monitor shows `waiting for download` after flashing a board that uses the ESP32-S3's built-in USB port, press the board's reset button. Do not hold a button on GPIO0, such as the Cardputer's G0, while you press reset.

`firmware/espflash.toml` runs the flash at quad I/O and 80 MHz, which makes the firmware about four times faster than espflash's default of dual I/O at 40 MHz. The Cardputers' Stamp-S3 supports this. If your board does not start after flashing, its flash may not, so delete the file and build and flash again.

Your board makes its private key on first boot, and keeps it with its settings and clock in the `esp-ygg` namespace of its `nvs` flash partition. Reflashing keeps them as long as that partition stays. Boards flashed with much older esp-ygg firmware stored these in another format, so they get a new identity and default settings when you upgrade them.

## Choose what to build in

The board name chooses the interface: `tui` for the text interface, or `gui` for the graphical one, which only the touch board supports. Builds without an interface, such as `s3` and `c6`, run as a node with the screen off, which leaves the most memory for the network.

To add services and apps, list their features after the board name:

```sh
python3 build.py cardputer gopher irc gemini
```

| Feature | What you get | Default |
|---|---|---|
| `http` | HTTP status page | on |
| `console` | UDP console, and its app | on |
| `ntp` | Time over NTP, and the clock | on |
| `gopher` | Gopher menu, and the Gopher app | off |
| `irc` | IRC app | off |
| `gemini` | Gemini app | off |
| `sftp` | SFTP app, on boards with a screen | off |
| `voice` | Voice calls, on the Cardputers | off |

To leave out the default ones, add `--minimal` and list the features you want. For example, a Cardputer with the console alone:

```sh
python3 build.py cardputer --minimal console
```

A service's server takes memory all the time, while its app takes memory only while you use it. A Cardputer ADV acting as a gateway with every service has about 70 KB free. SFTP also adds about 260 KB to the firmware.

## Settings for boards without a screen

A board with a screen keeps the settings you choose in its interface. Until you save some, and on boards without a screen, settings come from these environment variables, read when you build:

| Variable | Default | Meaning |
|---|---|---|
| `ESP_YGG_CHANNEL` | `1` | WiFi channel for ESP-NOW |
| `ESP_YGG_NAME` | none | `name` field in the board's nodeinfo |
| `ESP_YGG_PASSWORD` | none | Link password. Boards peer with others that use the same one. |
| `ESP_YGG_WIFI_SSID` | none | WiFi network to join for a TCP peering |
| `ESP_YGG_WIFI_PASSWORD` | none | Its WPA2 password. Leave unset for an open network. |
| `ESP_YGG_PEER` | none | Node to peer with over TCP, as an IPv4 address and port |
| `ESP_YGG_STATUS_PAGE` | `on` | Set to `off` to close the HTTP and Gopher ports |
| `ESP_YGG_HOTSPOT` | none | Name of a WiFi network for the board to run as a hotspot |
| `ESP_YGG_HOTSPOT_PASSWORD` | none | Its WPA2 password, 8 to 63 characters. Leave unset for an open network. |

For example:

```sh
ESP_YGG_WIFI_SSID=home ESP_YGG_WIFI_PASSWORD=secret ESP_YGG_PEER=192.168.1.10:9001 python3 build.py s3
```

## Connect the mesh to a Yggdrasil network

To connect the ESP-NOW mesh to the rest of Yggdrasil, have one board join your WiFi network and peer over TCP with a regular Yggdrasil node. Settings calls this the bridge. Set it up under Settings, Bridge, or with `ESP_YGG_WIFI_SSID`, `ESP_YGG_WIFI_PASSWORD` and `ESP_YGG_PEER` when you build. The board bridges once it has both a network and a peer.

Your Yggdrasil node needs a TCP listener in its config, such as `Listen: ["tcp://0.0.0.0:9001"]`.

A board's radio can use one channel at a time, so a bridging board uses your access point's channel for ESP-NOW too. Set your other boards to that channel, in Settings or with `ESP_YGG_CHANNEL`, or they will not hear it.

WiFi connection attempts time out after 15 seconds and retry after a five-second delay. The interface stays usable and warns after a failed attempt, clearing the warning when WiFi connects.

The board stores the WiFi password in flash as plain text.

## Hotspot

Turn on the hotspot under Settings, or build with `ESP_YGG_HOTSPOT`, and your phones and laptops can join the board's own WiFi network to reach Yggdrasil without installing anything. A board can bridge and run a hotspot at once.

A device that joins gets an address in the board's `300:` subnet and a route to the rest of Yggdrasil through the board. From it, you can reach any Yggdrasil address, such as the status page of a board across the mesh. The hotspot carries Yggdrasil traffic and nothing else. Phones may warn that it has no internet, and need telling to stay connected. A laptop also on another network keeps using that one for everything else.

The hotspot runs on the board's ESP-NOW channel, or on your WiFi network's channel when the board also bridges. NetworkManager installs the route to `200::/7` that the board advertises. Your Linux laptop may let the kernel handle router advertisements instead. In that case, set `net.ipv6.conf.<interface>.accept_ra_rt_info_max_plen` to 7 or more. Otherwise the kernel ignores the route when the laptop has IPv6 from another network as well.

## Use the text interface

On a Cardputer, or the touch board built as `c6-touch`, the home screen shows the board's address and how many nodes it knows, with a menu below:

- Yggdrasil Nodes lists every node the board knows, direct peers first, named from their nodeinfo. Open one to see its details, then press `p` to ping it or `m` to send it a console message.
- ESP-NOW Neighbors lists the boards in radio range, with their recent signal strength and the share of frames sent to them that went unacknowledged. Use it to find where boards stop reaching each other.
- Apps opens the apps, and counts the new messages waiting in them.
- Settings changes the board's settings, described below.

The apps are:

- Ping pings an address you type or pick, once a second, and shows each round trip.
- UDP Console shows text that arrives on UDP port 4242, and sends what you type to an address or `[address]:port`.
- IRC, with `irc`, connects to an IRC server over Yggdrasil and joins a channel. Each channel and private chat gets its own buffer, and Tab moves between them. The usual commands work, such as `/join`, `/part`, `/msg`, `/me`, `/nick` and `/quit`. You can leave the screen and stay connected, and the menu counts new lines.
- Gopher, with `gopher`, browses Gopher menus and text from an address you type. Links to a board's peers let you hop across the mesh. Yggdrasil has no standard way to look up names, so links work when they use an address or the name of the server they came from.
- Gemini, with `gemini`, browses Gemini capsules from an address you type. Up and down move between links, scrolling past text where there are none, and Enter follows the selected one. You can answer questions that servers ask, and redirects are followed. Gemini servers mostly sign their own certificates, so the app trusts the first certificate a server shows and refuses a different one until the board restarts. Going back fetches the page again, to save memory.
- SFTP, with `sftp`, sends and receives files. See [SFTP](#sftp).
- Voice Call (IAX2), with `voice`, calls other boards. See [Voice calls](#voice-calls).

In Settings, you can change the board's name, channel and time zone, turn the status pages on or off, and set up the bridge and the hotspot. Builds with SFTP add its password, and voice builds add the ringer. Saving applies most settings at once. Changing the bridge or the hotspot restarts the board, as does changing the channel while the hotspot is on. The Save button tells you when saving will restart the board.

You can also export your settings to `esp-ygg/settings.txt` on a FAT32 microSD card and import them back. The file holds `key = value` lines that you can edit on a computer, with passwords in plain text. A file can leave out settings it does not change, and importing fills in the settings screen without saving.

Once the board knows the time, the title bar shows it. A bridging board gets the time from `pool.ntp.org`, and other boards ask their peers, so the time spreads through the mesh. The time zone is a fixed offset from UTC, so change it by hand for daylight saving time.

In fields that take an address, type the start of a known node's address or name and press Tab to fill in its address. Press Tab again for the next node that matches. Names come from nodeinfo, which the board looks up while these screens are open.

On the Cardputer, the arrow keys are Fn with `;` `.` `,` and `/`, and Escape is Fn with `` ` ``. In lists, the same keys work without Fn.

On the touch screen, drag up or down to move the selection a row at a time. A tap is Enter, and a swipe to the right is Escape. Tapping a text field brings up a keyboard on the lower half of the screen, where `123` switches to digits and symbols and `▼` puts the keyboard away. Tap above it to bring it back while the field is open. Fields that take numbers alone, such as the channel, time zone and peer, get a two-row number pad that leaves more of the screen in view.

The touch board's screen dims after 30 seconds without a touch, and turns off 30 seconds later. Touch it to wake it. That touch does nothing else.

To lock a board against input, such as before putting it in a pocket, press the button on its side: G0 on a Cardputer, or BOOT on the touch board. The text interface says Locked, the graphical one dims behind a lock, and the screen turns off after 5 seconds. Press the button again to unlock. In voice builds, where G0 is also push-to-talk, the button locks the board outside calls alone, and an incoming call unlocks it so you can answer.

## Use the graphical interface

Build `c6-gui` to give the touch board a graphical interface instead, made with [Slint](https://slint.dev) and its Material 3 components. It has four pages:

- Home, a launcher for the apps. Console and IRC show counts of new messages on their icons.
- Nodes, every node the board knows, named once they answer.
- Radio, the boards in ESP-NOW range, with their signal.
- Settings, with the board's address and uptime, the settings for the node, the bridge and the hotspot, and export and import to the SD card. You can pick the bridge's network from a scan, or type one that does not show.

The apps work as they do in the text interface. Gopher and Gemini wrap pages to the screen, Gemini shows headings, lists, quotes and preformatted text, and IRC lists its channels and private chats for you to switch between.

The navigation runs down the side of a wide screen and along the bottom of a tall one, so you can hold the board either way. While an app is open, the navigation folds to a thin handle. Tap the handle to bring the navigation back, tap the app to put it away, and tap Home to go back to the launcher.

Tapping a setting opens a keyboard over the whole screen, with the setting at the top. Settings that take numbers, such as the channel and the peer, get a number pad. Enter checks the value with the same rules as the text interface, and tells you what is wrong with a value it cannot use.

The interface shows printable ASCII and the characters in its own text. Other characters, such as accented letters in a node's name, do not show yet.

## Voice calls

Build `voice` to call other boards by address from the Voice Call (IAX2) app, with Tab completion like the other address fields. Calls use IAX2, the protocol Asterisk speaks, on UDP port 4569, so other IAX2 software on Yggdrasil should be able to call in too. Calls use no IAX2 passwords, as Yggdrasil already encrypts traffic and authenticates it by address.

An incoming call rings and brings up the call screen wherever you are. Press Enter to answer, and Escape to decline or hang up. Calls are push-to-talk, like a walkie-talkie: hold G0 on the side of the board to talk, and let go to listen.

Under Settings, Ringer picks one of five ringtones and five volume steps, playing each briefly as you change it. To check the speaker and microphone outside a call, send `talk on` over the serial port to record a few seconds, and `talk off` to play them back (see [Development](#development)).

Calls use G.726 at 32 kbit/s, which fits 20 ms of voice in one ESP-NOW frame, and fall back to G.711 μ-law with software that lacks G.726. The boards offer both G.726 byte orders that Asterisk uses, so G.726 calls work with Asterisk.

To call boards from a computer on the same Yggdrasil network, and take their calls, use the `phone` example with ALSA's tools:

```sh
arecord -q -t raw -f S16_LE -r 8000 -c 1 \
  | cargo run -p esp-ygg-app --features voice --example phone -- call <board address> \
  | aplay -q -t raw -f S16_LE -r 8000 -c 1
```

Use `phone answer` instead of `phone call <board address>` to wait for a board to call the computer. The computer sends sound all the time instead of push-to-talk, so use headphones.

To take the boards' calls in Asterisk, allow its `guest` user, as boards call without a username. Asterisk's IAX2 dial strings cannot hold an IPv6 address, so define a named peer for each board you want to call:

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

## SFTP

Build with `sftp` to send and receive files with any SFTP client or server, whether another board or a computer.

Files go in `esp-ygg/sftp` on the SD card. Peers can list, download and upload files in that folder. They cannot delete or rename files, or make folders.

To let peers in without asking, set a password under Settings. Without one, or when a peer gives the wrong one, the board asks you on screen whether to let the peer in, and turns it away after a minute without an answer. From a computer on Yggdrasil, any user name works:

```sh
sftp 'me@[<board address>]'
```

To send a file, pick it in the SFTP app, then pick a node or type an address and the user to log in as. If the server asks for a password, the board asks you for it on screen. Boards cannot log in with a key.

The board's SSH host key is its Yggdrasil key, so your computer sees the same fingerprint for as long as the board keeps its identity. When sending, the board accepts any host key, as Yggdrasil already makes sure that the node at an address holds that address's key.

Received files get short names in the 8.3 style that Windows uses, such as `HOLIDA~1.JPE` for `holiday photo.jpeg`. The board can read long file names on the card, but cannot write them. When a name is taken, the new file gets the next number, and the old file stays.

Key exchange uses curve25519, without the post-quantum ML-KEM hybrid, so a client set to require ML-KEM cannot connect.

## Try it from a computer

From a computer whose Yggdrasil node peers with a board, directly or through other boards:

```sh
ping <board address>
yggdrasilctl getNodeInfo key=<board public key>
curl http://[<board address>]/
curl gopher://[<board address>]/
nc -6u <board address> 4242
```

The first request after the boards start can take several seconds while routes form. After that, a ping across a gateway and one ESP-NOW hop usually takes 30 to 70 ms.

If a board seems out of reach, check ESP-NOW Neighbors on a board with a screen. It shows each neighbor's signal and the share of frames it did not acknowledge. A few failures are normal, and the boards resend those frames.

## Limits

A board peers with at most six ESP-NOW neighbors at once.

The boards have no real-time clock. Yggdrasil numbers some messages with the time, and other nodes ignore numbers that go backwards, so each board saves its clock to flash every hour and starts each boot an hour past the last save. As a result, a board's clock runs ahead until it learns the time from NTP or its peers.

## Development

`node` is the Yggdrasil node, as a `no_std` library that does no I/O. `app` adds the services and the interface, also without touching hardware. Both build and run on a desktop. `firmware` runs them on the ESP32 with esp-hal, esp-radio and Embassy.

To test `node`, `app` and `gui` on your computer, run `cargo test` from the repository root.

To try the text interface without a board, run the preview. It shows the interface in a terminal as one of three simulated boards in a line, and the farthest board echoes console messages back. Add `-- touch` for the touch board's larger screen, where mouse clicks are taps:

```sh
cargo run -p esp-ygg-app --example preview
```

The preview can also draw the touch board's screen pixel for pixel and save it. It takes the same commands as `drive.py`, below, and saves a PNG at each `screen`:

```sh
cargo run -p esp-ygg-app --example preview -- png shots "key down down down enter" "tap 5 2" screen
```

To see the graphical interface without a board, draw each page for a small made-up mesh, wide and tall, into a folder:

```sh
cargo run -p esp-ygg-gui --example screenshot -- screens
```

To drive a board's interface from your computer over its USB serial port, without resetting it, use `firmware/drive.py`:

```sh
python3 firmware/drive.py /dev/ttyACM0 "key down down enter enter" "type 200::1" "key enter" "wait 3" screen
```

It sends commands such as `type <text>`, `key <names>`, `tap <column> <row>`, `drag <x> <y> <x> <y>`, `lock` and `screen`. On the graphical interface, `tap` and `drag` take pixels, and `screen` saves the screen as `screen.png` instead of printing it. Voice builds add `talk on`, `talk off` and `mute on`. Run it without commands to list them all.

For C6 WiFi and scheduling troubleshooting, add `diagnostics` to a `c6-gui` build. Its serial commands let you change radio settings, scan access points and probe DNS, TCP and NTP without reinstalling firmware. See [C6 serial diagnostics](firmware/DIAGNOSTICS.md).

## License

The code is licensed under the GNU Lesser General Public License version 3, with the same exception for static linking that yggdrasil-go uses. See [LICENSE](LICENSE).

Slint is licensed under the GPLv3 on microcontrollers, so firmware built with `gui` is distributed under the GPLv3. The rest of the project, and all other builds, stay under the LGPLv3.

The graphical interface bundles:

- the Material components in `gui/material-1.1.0`, under the MIT license
- the Roboto and Roboto Mono fonts in `gui/fonts`, cut down to the weights and characters the interface uses, under the SIL Open Font License
- icons from Google's Material Symbols in `gui/icons`, under the Apache License 2.0

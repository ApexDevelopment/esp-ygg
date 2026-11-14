# esp-ygg

esp-ygg turns your ESP32 boards into a Yggdrasil node. ESP32 boards running esp-ygg will pair with each other over ESP-NOW, Espressif's peer-to-peer protocol, and form their own Yggdrasil mesh.

To connect the ESP-NOW mesh to the public Yggdrasil network, have one board join your WiFi network and peer with a regular Yggdrasil node. A board can also run a WiFi hotspot, so your phone or laptop can join Yggdrasil through it without installing anything.

A board running esp-ygg makes the following services available, accessible via its Yggdrasil IP:

- an HTTP status page on port 80, with the board's address, uptime, memory and peers
- a Gopher menu on port 70 with the same facts, linking to the menus of its peers
- a raw UDP console on port 4242
- the time over NTP on UDP port 123, if the board knows it

On a board with a screen, you can also browse the network and ping nodes. Apps are available for sending arbitrary UDP messages, chatting via IRC, reading Gopher and Gemini pages, trading files over SFTP and, on the Cardputer, even making voice calls!

esp-ygg has been tested on the M5Stack Cardputer and Cardputer Adv (ESP32-S3), the Waveshare ESP32-C6-Touch-LCD-2.8, and the Waveshare ESP32-C3-LCD-1.47. It should build for other boards; please create an issue with your results if you try it on a new board.

## Build and flash

You will need Espressif's Rust toolchain and espflash:

```sh
cargo install espup espflash
espup install
```

If you are building for an Xtensa-based ESP, load the toolchain's environment in your shell:

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
| `cardputer` | M5Stack Cardputer (and Adv), with the text interface |
| `voice` | M5Stack Cardputer, with voice calls |
| `c6-touch` | Waveshare ESP32-C6-Touch-LCD-2.8, with the text interface |
| `c6-gui` | Waveshare ESP32-C6-Touch-LCD-2.8, with the graphical interface |
| `s3` | any ESP32-S3, headless node |
| `c3` | any ESP32-C3, headless node |
| `c6` | any ESP32-C6, headless node |
| `esp32` | any original ESP32, headless node |

`--flash` writes the build to the board and `--monitor` follows the serial log. Add `--port /dev/ttyACM0`, or whichever port the board is on, if espflash can't find it.

Builds will be written to `firmware/target/images/<name>`, with any extra features you add in the folder's name, for example: `cardputer+gopher+irc`. The folder will contain:

- `esp-ygg.elf`, which espflash flashes and which you need to decode a backtrace
- `esp-ygg.bin`, the app image, for launchers and other flashing tools
- `esp-ygg-merged.bin`, if you add `--merged`, which adds the bootloader and partition table

If the monitor shows `waiting for download` after flashing a board that uses the ESP32-S3's built-in USB port, press the board's reset button. Do not hold a button on GPIO0, such as the Cardputer's G0, while you press reset.

`firmware/espflash.toml` runs the flash at quad I/O and 80 MHz, which makes the firmware about four times faster than espflash's default of dual I/O at 40 MHz. The Cardputers' Stamp-S3 supports this. If your board does not start after flashing, its flash might not support this, so delete the file and build and flash again.

Your board generates its private key on first boot and keeps it along with its settings and clock in the `esp-ygg` namespace of its `nvs` flash partition. Reflashing will not change any of that as long as that `nvs` partition is untouched.

## Choose features

The board you choose determines which UI is used: `tui` for the text interface, or `gui` for the graphical one (only supported on the 2.8 inch touch board and future touch boards). Headless builds, of course, run with no screen and no UI, which leaves the most memory possible for the network stack.

To add services and apps, list their features after the board name:

```sh
python3 build.py cardputer gopher irc gemini
```

| Feature | What you get | Default |
|---|---|---|
| `http` | HTTP status page | on |
| `console` | UDP console and its app | on |
| `ntp` | Time over NTP and the clock | on |
| `gopher` | Gopher menu and the Gopher app | off |
| `irc` | IRC app | off |
| `gemini` | Gemini app | off |
| `sftp` | SFTP app | off |
| `voice` | Voice calls (Cardputers only) | off |

To leave out the default ones, add `--minimal` and list the features you want. For example, a Cardputer with just the UDP console:

```sh
python3 build.py cardputer --minimal console
```

A service's server takes memory all the time, while its app takes memory only while you use it. A Cardputer Adv acting as a gateway with every service has about 70 KB free. SFTP also adds about 260 KB to the firmware.

## Settings for boards without a screen

On boards without a screen, settings come from these environment variables:

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

To connect the ESP-NOW mesh to a Yggdrasil network on the internet, have one board join your WiFi network and peer over TCP with a regular Yggdrasil node. This is called "bridging" in the settings. Set it up under Settings -> Bridge, or with `ESP_YGG_WIFI_SSID`, `ESP_YGG_WIFI_PASSWORD` and `ESP_YGG_PEER` when you build.

If you're peering with your own Yggdrasil node, make sure it has a TCP listener in its .conf, such as `Listen: ["tcp://0.0.0.0:9001"]`.

A board's radio can use **one channel at a time**, so a bridging board uses your access point's channel for ESP-NOW too. Set your other boards to that channel, in Settings or with `ESP_YGG_CHANNEL`, or they will not peer with each other.

The board stores the WiFi password in flash as plain text.

## Using the UI

The TUI and GUI largely have the same options available, they're just laid out a little differently. The TUI presents options line-by-line, while the GUI gives you a more traditional "app" experience. Both have an on-screen keyboard for touch devices, so you can build the TUI version for a touch device if you prefer it over the GUI. The TUI supports tab completion in all address fields so you don't have to manually type out a long IPv6 address. Pressing G0/BOOT/GPIO0 (while not in a voice call) will lock the screen; press it again to unlock (all other inputs will be disabled).

## Hotspot

Turn on the hotspot under Settings, or build with `ESP_YGG_HOTSPOT`, and your devices can join the board's own WiFi network to reach Yggdrasil without installing anything. A board can bridge and run a hotspot at the same time.

Any device that joins gets an address in the board's `300:` subnet. From the hotspot, you should be able to reach any Yggdrasil address the board can reach. The hotspot will only carry Yggdrasil traffic and nothing else, so your device may warn that it has no internet; if prompted, tell it to stay connected.

The hotspot runs on the board's ESP-NOW channel, or on your WiFi network's channel when the board is in bridge mode.

## Voice calls

Build `voice` to call other boards by address from the Voice Call (IAX2) app. Calls use IAX2 on UDP port 4569, so other IAX2 software on Yggdrasil should be able to call in too. Calls use no IAX2 passwords, as Yggdrasil already encrypts traffic and authenticates it by address.

An incoming call rings and brings up the call screen anywhere in the UI. Press Enter to answer, and Escape to decline or hang up. Calls are push-to-talk, like a walkie-talkie: hold G0 on the side of the board to talk, and let go to listen.

Calls use G.726 at 32 kbit/s, which fits 20 ms of voice in one ESP-NOW frame, and fall back to G.711 μ-law if G.726 isn't available. The boards offer both G.726 byte orders that Asterisk uses, so G.726 calls work with Asterisk.

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

[cardputer]
type=peer
host=<board address>
```

Then `Dial(IAX2/cardputer/s)` rings the board.

## SFTP

Build with `sftp` to send and receive files with any SFTP client or server, whether another board or a computer.

Files go in `esp-ygg/sftp` on the SD card. Peers can list, download and upload files in that folder. They cannot delete or rename files, or make folders.

To let peers in without asking, set a password under Settings. Without one, or when a peer gives the wrong one, the board asks you on screen whether to let the peer in, and turns it away after a minute without an answer. From a computer on Yggdrasil, any user name works:

```sh
sftp 'me@[<board address>]'
```

To send a file, pick it in the SFTP app, then pick a node or type an address and the user to log in as. If the server asks for a password, the board asks you for it on screen. Boards cannot log in with a key.

The board's SSH host key is its Yggdrasil key, so your computer sees the same fingerprint as long as the board keeps its identity. When sending, the board accepts any host key, as Yggdrasil already ensures that the node at an address has that address's key.

Received files get short names in the 8.3 style that Windows uses, such as `HOLIDA~1.JPE` for `holiday photo.jpeg`. The board can read long file names on the card, but cannot write them. When a name is taken, the new file gets the next number.

Key exchange uses curve25519, without the post-quantum ML-KEM hybrid, so a client set to require ML-KEM cannot connect.

## Limits

A board can peer with at most six ESP-NOW neighbors at the same time.

Many ESP32 boards have no real-time clock. Yggdrasil numbers some messages with the time, and other nodes ignore numbers that go backwards, so each board saves its clock to flash every hour and starts each boot an hour past the last save. As a result, a board's clock runs ahead until it learns the time from NTP or its peers.

## Development

`node` is just the Yggdrasil node as a `no_std` library that does no I/O. `app` adds the services and the interface, also without touching hardware. Both will build and run on a desktop. `firmware` runs them on the ESP32 with esp-hal, esp-radio and Embassy.

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

## License

The code is licensed under the GNU Lesser General Public License version 3, with the same exception for static linking that yggdrasil-go uses. See [LICENSE](LICENSE).

Slint is licensed under the GPLv3 on microcontrollers, so firmware built with `gui` is distributed under the GPLv3. The rest of the project, and all other builds, are under the LGPLv3.

The GUI bundles:

- the Material components in `gui/material-1.1.0`, under the MIT license
- the Roboto and Roboto Mono fonts in `gui/fonts`, cut down to the weights and characters the interface uses, under the SIL Open Font License
- icons from Google's Material Symbols in `gui/icons`, under the Apache License 2.0

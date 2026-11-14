# C6 serial diagnostics

Build the graphical firmware with `diagnostics` to troubleshoot over USB while keeping the GUI and apps:

```sh
cd firmware
python3 build.py c6-gui gopher irc gemini diagnostics
```

Install `target/images/c6-gui+gopher+irc+gemini+diagnostics/esp-ygg.bin` through your launcher. The UI and network threads start at priority 1. The serial handler runs on the network thread, so it can receive diagnostic commands if the UI stops running.

Use a serial terminal, or send commands from the project root with `drive.py`:

```sh
python3 firmware/drive.py /dev/ttyACM0 "diag help" "diag status" "wait 20"
```

Use one serial reader at a time. The board acknowledges queued commands, then reports the result. WiFi commands run between operations; connections and scans have 15-second timeouts. DNS and TCP probes have five-second timeouts, and the NTP probe has a ten-second timeout.

The image reports its build timestamp, flash capacity and partition layout at boot. Every five seconds it reports node and network poll counts, UI loop counts, free heap, queue lengths and IP configuration. `diag watch off` stops these periodic reports. WiFi attempts report elapsed time and the driver's disconnect reason.

## Commands

| Command | Effect |
|---|---|
| `diag help` | List commands and argument ranges |
| `diag status` | Report activity, WiFi configuration, signal and IP configuration |
| `diag watch on` / `diag watch off` | Enable or disable periodic reports |
| `diag radio on` / `diag radio off` | Enable or suppress ESP-NOW transmissions while testing WiFi |
| `diag pause 100` | Suspend the network thread for 100 ms; accepts 1–1000 |
| `diag priority 1` | Set the network thread's priority; accepts 0–3; UI stays at 1 |
| `diag reboot` | Restart the board |
| `diag wifi status` | Report WiFi state and the selected station configuration |
| `diag wifi pause` / `diag wifi resume` | Stop or resume automatic WiFi connections |
| `diag wifi reconnect` | Disconnect and retry |
| `diag wifi reset` | Restore the WiFi configuration from boot and disable capture |
| `diag wifi scan passive auto 360` | Scan all channels, listening for 360 ms per channel |
| `diag wifi scan active 1 300` | Probe channel 1 for 300 ms |
| `diag wifi power 40` | Request a transmit-power limit in quarter-dBm units; accepts 8–84; bridge boot default is 60 (15 dBm) |
| `diag wifi ps none` | Select `none`, `min` or `max` power saving |
| `diag wifi protocol bgn` | Select `b`, `bg`, `bgn` or `bgnax` |
| `diag wifi bandwidth 20` | Request 20 or 40 MHz bandwidth |
| `diag wifi bssid 02:11:22:33:44:55` | Pin the station to an access point; `auto` clears the pin |
| `diag wifi channel 1` | Set the station's channel hint; `auto` clears it |
| `diag wifi method all` | Select `all` or `fast` scanning when connecting |
| `diag wifi auth wpa2` | Select `wpa2` or `mixed` (WPA/WPA2), using the saved password |
| `diag wifi sniff on` / `diag wifi sniff off` | Enable or disable management-frame metadata capture |
| `diag dns pool.ntp.org` | Resolve an IPv4 address through the configured DNS server |
| `diag tcp peer` | Try a TCP connection to the saved peer; an explicit `IPv4:port` also works |
| `diag ntp` | Query the configured NTP service and report its response |

Scans default to passive mode, all channels and 360 ms per channel. The channel argument accepts `auto` or 1–13; the dwell time accepts 10–1000 ms. Scan output includes each access point's BSSID, channel, signal, authentication method and country information when available.

Priority 2 reproduces the observed UI stall. Send `diag priority 1` to restore the working priority. `diag wifi reset` restores WiFi settings; it does not change thread priority.

Transmit power is a requested limit. The radio may round or cap it. To compare power levels, send `diag wifi power 40`, then `diag wifi reconnect` and inspect the next connection result. Higher power is not always better on this hardware.

Capture is off at boot. It reports received association responses, authentication frames and disconnect frames involving this station, with addresses, signal, channel and status or reason codes. It does not dump packet payloads or passwords. The subtype values are 1 (association response), 3 (reassociation response), 10 (disassociation), 11 (authentication) and 12 (deauthentication). Authentication and association responses carry status codes; disconnect frames carry reason codes. The counter `capture dropped` reports metadata lost when its queue fills.

Diagnostic overrides stay in RAM. Reboot restores the saved configuration. Normal UI settings still save normally. To try another network, enter it through Settings and restart the same image. The driver currently exposes WPA2 and WPA/WPA2 station authentication through this API, so there is no WPA3 command. DHCP and network probes require bridge mode at boot; enable it in Settings and restart if it was off. The NTP probe also requires the `ntp` feature, which the build above includes.

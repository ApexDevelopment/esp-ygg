"""Drives a board's interface over its USB serial port, without resetting it.

Usage: python3 drive.py <port> <command>...

Each command is one of:
	type <text>      types the text
	key <name>...    presses keys: enter, esc, tab, backspace, space, up, down, left, right
	tap <col> <row>  taps a touch screen at a character cell
	screen           prints the screen
	talk on|off      holds or releases push-to-talk
	mute on|off      silences the speaker
	wait <seconds>   waits, printing whatever the board logs meanwhile

For example:
	python3 drive.py /dev/ttyACM0 "key down down enter" "type 200::1" "key enter" "wait 3" screen
"""

import os
import select
import sys
import termios
import time

SCREEN_END = b"--- end ---"


def open_port(path):
	port = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
	attributes = termios.tcgetattr(port)
	attributes[0] = 0
	attributes[1] = 0
	# Raw bytes, and leave the control lines alone on close, which would
	# otherwise reset the board.
	attributes[2] = (attributes[2] | termios.CS8 | termios.CREAD | termios.CLOCAL) & ~termios.HUPCL
	attributes[3] = 0
	termios.tcsetattr(port, termios.TCSANOW, attributes)
	return port


def write_all(port, data):
	"""Writes everything, waiting while the board catches up, but not for
	a board that has stopped reading, such as after a crash."""
	while data:
		_, writable, _ = select.select([], [port], [], 2)
		if not writable:
			sys.exit("The board stopped reading the serial port")
		try:
			data = data[os.write(port, data):]
		except BlockingIOError:
			pass


def read_for(port, seconds, until=None):
	"""Reads for some seconds, or until `until` arrives and the board goes
	quiet. Writing while the board is still sending can lose a byte."""
	received = b""
	deadline = time.monotonic() + seconds
	while time.monotonic() < deadline:
		ready, _, _ = select.select([port], [], [], 0.05)
		if ready:
			try:
				received += os.read(port, 4096)
			except BlockingIOError:
				pass
		elif until and until in received:
			break
	return received.decode(errors="replace")


def main():
	if len(sys.argv) < 3:
		sys.exit(__doc__)
	port = open_port(sys.argv[1])
	for command in sys.argv[2:]:
		if command.startswith("wait "):
			print(read_for(port, float(command.split()[1])), end="")
			continue
		write_all(port, (command + "\n").encode())
		if command == "screen":
			print(read_for(port, 3, until=SCREEN_END), end="")
		else:
			print(read_for(port, 0.3), end="")


main()

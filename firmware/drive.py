"""Drives a Cardputer's interface over its USB serial port, without resetting it.

Usage: python3 drive.py <port> <command>...

Each command is one of:
	type <text>      types the text
	key <name>...    presses keys: enter, esc, tab, backspace, space, up, down, left, right
	screen           prints the screen
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
	"""Writes everything, waiting while the board catches up."""
	while data:
		select.select([], [port], [])
		try:
			data = data[os.write(port, data):]
		except BlockingIOError:
			pass


def read_for(port, seconds, until=None):
	received = b""
	deadline = time.monotonic() + seconds
	while time.monotonic() < deadline:
		ready, _, _ = select.select([port], [], [], 0.05)
		if ready:
			try:
				received += os.read(port, 4096)
			except BlockingIOError:
				pass
			if until and until in received:
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

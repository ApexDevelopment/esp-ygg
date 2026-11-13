"""Drives a board's interface over its USB serial port, without resetting it.

Usage: python3 drive.py <port> <command>...

Each command is one of:
	type <text>      types the text
	key <name>...    presses keys: enter, esc, tab, backspace, space, up, down, left, right
	tap <col> <row>  taps a touch screen at a character cell, or a pixel in
	                 the graphical interface
	drag <x> <y> <x> <y>
	                 drags across the graphical interface, in pixels
	screen           prints the screen, or saves the graphical interface's
	                 as screen.png
	talk on|off      holds or releases push-to-talk
	mute on|off      silences the speaker
	lock             presses the button that locks and unlocks the board
	wait <seconds>   waits, printing whatever the board logs meanwhile

For example:
	python3 drive.py /dev/ttyACM0 "key down down enter enter" "type 200::1" "key enter" "wait 3" screen
"""

import os
import re
import select
import struct
import sys
import termios
import time
import zlib

SCREEN_START = "--- screen ---"
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


def save_png(dump, path):
	"""Saves the graphical interface's screen, sent as a size line then a
	line of hexadecimal RGB565 pixels for each row, as a PNG."""
	lines = dump.split("\n")
	width, height = map(int, lines[0].split("x"))
	raw = bytearray()
	for line in lines[1:height + 1]:
		raw.append(0)
		for index in range(0, width * 4, 4):
			pixel = int(line[index:index + 4], 16)
			red, green, blue = pixel >> 11, (pixel >> 5) & 0x3F, pixel & 0x1F
			raw += bytes((red * 255 // 31, green * 255 // 63, blue * 255 // 31))

	def chunk(kind, data):
		body = kind + data
		return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

	header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
	png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(raw))) + chunk(b"IEND", b"")
	with open(path, "wb") as file:
		file.write(png)


def show_screen(received):
	"""Prints the screen, or saves it if it is the graphical interface's."""
	dump = received.split(SCREEN_START + "\n", 1)[-1].replace("\r", "")
	if re.match(r"\d+x\d+\n", dump):
		save_png(dump.split(SCREEN_END.decode())[0], "screen.png")
		print("Saved screen.png")
	else:
		print(received, end="")


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
			show_screen(read_for(port, 10, until=SCREEN_END))
		else:
			print(read_for(port, 0.3), end="")


main()

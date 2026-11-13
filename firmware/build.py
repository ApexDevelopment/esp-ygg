#!/usr/bin/env python3
"""Builds the firmware for a board, and saves its image beside the ELF.

Usage: python3 build.py <board> [feature...] [--minimal] [--merged] [--flash] [--monitor] [--port PORT]

Each build lands in target/images/<board>, or target/images/<board>+<feature>...
with extra features, holding:

	esp-ygg.elf         for espflash, and for reading backtraces
	esp-ygg.bin         the app image, which launchers and flashing tools take
	esp-ygg-merged.bin  with --merged: bootloader, partitions and app, for address 0

--minimal leaves out the standard services (http, console and ntp), keeping
only the features listed. --flash writes the build to a plugged-in board, and
--monitor then shows its log.

For example:
	python3 build.py cardputer gopher irc --flash --monitor
"""

import argparse
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

FIRMWARE = Path(__file__).resolve().parent
IMAGES = FIRMWARE / "target" / "images"


@dataclass
class Board:
	chip: str
	target: str
	features: list[str]
	flash_size: str
	about: str


BOARDS = {
	"cardputer": Board(
		"esp32s3", "xtensa-esp32s3-none-elf", ["cardputer", "tui", "standard"], "8mb",
		"M5Stack Cardputer or Cardputer ADV",
	),
	"voice": Board(
		"esp32s3", "xtensa-esp32s3-none-elf", ["voice", "standard"], "8mb",
		"the Cardputers, with voice calls",
	),
	"s3": Board("esp32s3", "xtensa-esp32s3-none-elf", ["esp32s3", "standard"], "4mb", "ESP32-S3"),
	"c3": Board("esp32c3", "riscv32imc-unknown-none-elf", ["esp32c3", "standard"], "4mb", "ESP32-C3"),
	"c6": Board("esp32c6", "riscv32imac-unknown-none-elf", ["esp32c6", "standard"], "4mb", "ESP32-C6"),
	"c6-touch": Board(
		"esp32c6", "riscv32imac-unknown-none-elf", ["c6-touch-lcd", "tui", "standard"], "16mb",
		"Waveshare ESP32-C6-Touch-LCD-2.8",
	),
	"c6-gui": Board(
		"esp32c6", "riscv32imac-unknown-none-elf", ["c6-touch-lcd", "gui", "standard"], "16mb",
		"the same, with the graphical interface",
	),
	"esp32": Board("esp32", "xtensa-esp32-none-elf", ["esp32", "standard"], "4mb", "original ESP32"),
}


def main():
	arguments = parse_arguments()
	board = BOARDS[arguments.board]
	features = [f for f in board.features if not (arguments.minimal and f == "standard")]
	features += [f for f in arguments.features if f not in features]
	name = arguments.board + ("-minimal" if arguments.minimal else "")
	folder = IMAGES / "+".join([name, *arguments.features])
	elf = build(board, features, folder)
	save_image(board, elf, folder / "esp-ygg.bin")
	if arguments.merged:
		save_image(board, elf, folder / "esp-ygg-merged.bin", "--merge")
	print(f"Built {folder.relative_to(FIRMWARE)}")
	if arguments.flash:
		flash(board, elf, arguments.port, arguments.monitor)


def parse_arguments():
	boards = "\n".join(f"  {name:<10} {board.about}" for name, board in BOARDS.items())
	parser = argparse.ArgumentParser(
		description=__doc__.split("\n")[0],
		epilog=f"boards:\n{boards}",
		formatter_class=argparse.RawDescriptionHelpFormatter,
	)
	parser.add_argument("board", choices=BOARDS, metavar="board")
	parser.add_argument("features", nargs="*", help="extra features, such as gopher or sftp")
	parser.add_argument("--minimal", action="store_true", help="leave out http, console and ntp")
	parser.add_argument("--merged", action="store_true", help="also save an image for address 0")
	parser.add_argument("--flash", action="store_true", help="write the build to a board")
	parser.add_argument("--monitor", action="store_true", help="show the board's log after flashing")
	parser.add_argument("--port", help="the board's serial port, if espflash cannot tell")
	return parser.parse_args()


def build(board, features, folder):
	"""Builds a release ELF and copies it into `folder`, as each build overwrites Cargo's."""
	run([
		"cargo", "build", "--release", "--no-default-features",
		"--features", ",".join(features), "--target", board.target,
	])
	folder.mkdir(parents=True, exist_ok=True)
	elf = folder / "esp-ygg.elf"
	shutil.copyfile(FIRMWARE / "target" / board.target / "release" / "esp-ygg", elf)
	return elf


def save_image(board, elf, path, *options):
	"""Saves an image with espflash.toml's flash settings, as flashing would write it."""
	run([
		"espflash", "save-image", "--chip", board.chip, "--flash-size", board.flash_size,
		*options, str(elf), str(path),
	])


def flash(board, elf, port, monitor):
	command = ["espflash", "flash", "--chip", board.chip]
	if port:
		command += ["--port", port]
	if monitor:
		command.append("--monitor")
	run(command + [str(elf)])


def run(command):
	"""Runs a command in the firmware folder, where espflash finds its settings."""
	result = subprocess.run(command, cwd=FIRMWARE)
	if result.returncode != 0:
		sys.exit(result.returncode)


main()

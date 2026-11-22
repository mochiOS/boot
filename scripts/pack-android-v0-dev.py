#!/usr/bin/env python3
"""Prepare a single-use Fastboot Image for a board using Android v0 + appended DTB."""

import argparse
import ctypes
import gzip
import hashlib
import mmap
import os
from pathlib import Path
import struct
import tempfile
import zlib


FDT_MAGIC = b"\xd0\x0d\xfe\xed"
MAX_DTB = 2 * 1024 * 1024
MAX_IMAGE = 128 * 1024 * 1024
MAX_DISK = 256 * 1024 * 1024
MAX_ARENA = 1024 * 1024 * 1024


def prepare_dtb(dtb):
    if len(dtb) < 40 or len(dtb) > MAX_DTB or dtb[:4] != FDT_MAGIC:
        raise ValueError("the template needs one appended FDT")
    if struct.unpack_from(">I", dtb, 4)[0] != len(dtb):
        raise ValueError("multiple or truncated appended DTBs are unsupported")

    lib = ctypes.CDLL("libfdt.so.1")
    buffer = ctypes.create_string_buffer(len(dtb) + 1024)

    def checked(result, operation):
        if result < 0:
            raise ValueError(f"libfdt error {result} while {operation}")
        return result

    checked(lib.fdt_open_into(dtb, buffer, len(buffer)), "copying the template DTB")
    chosen = lib.fdt_path_offset(buffer, b"/chosen")
    if chosen == -1:  # FDT_ERR_NOTFOUND
        chosen = lib.fdt_add_subnode(buffer, 0, b"chosen")
    checked(chosen, "resolving /chosen")
    # The boot image is private to the local development host and used once.
    # Each launch gets new OS-generated entropy, including retries.
    checked(lib.fdt_setprop(buffer, chosen, b"rng-seed", os.urandom(32), 32),
            "adding a fresh RNG seed")
    checked(lib.fdt_pack(buffer), "packing the DTB")
    size = struct.unpack_from(">I", buffer, 4)[0]
    if size > MAX_DTB:
        raise ValueError("the prepared DTB exceeds the AArch64 handoff limit")
    return buffer.raw[:size]


def pack(template, image, disk=b""):
    """Return the Android header and padded kernel, followed separately by disk."""
    if len(template) < 1632 or template[:8] != b"ANDROID!":
        raise ValueError("not an Android boot image")
    kernel_size, _, ramdisk_size, _, second_size, _, _, page, version = struct.unpack_from(
        "<9I", template, 8)
    if version != 0 or second_size != 0 or page not in (2048, 4096, 8192, 16384):
        raise ValueError("only Android v0 with no second stage is supported")

    def padded_size(length):
        return (length + page - 1) & -page

    if len(template) != page + padded_size(kernel_size) + padded_size(ramdisk_size):
        raise ValueError("template sections do not match the Android v0 header")
    decoder = zlib.decompressobj(31)
    decoder.decompress(memoryview(template)[page:page + kernel_size], MAX_IMAGE)
    if not decoder.eof:
        raise ValueError("template must use a complete gzip kernel followed by an FDT")
    if len(image) < 64 or image[56:60] != b"ARMd":
        raise ValueError("the kernel must be a raw AArch64 Image")
    memory_size = struct.unpack_from("<Q", image, 16)[0]
    if not len(image) <= min(memory_size, MAX_IMAGE):
        raise ValueError("invalid AArch64 Image size")
    if not len(image) + len(disk) <= memory_size <= (MAX_ARENA if disk else MAX_IMAGE):
        raise ValueError("invalid AArch64 Image memory span")
    if disk and (len(disk) > MAX_DISK or len(disk) % 4096 or disk[512:520] != b"EFI PART"):
        raise ValueError("the boot disk must be a page-aligned GPT image up to 256 MiB")
    kernel = gzip.compress(image, mtime=0) + prepare_dtb(decoder.unused_data)

    header = bytearray(template[:page])
    struct.pack_into("<I", header, 8, len(kernel))
    struct.pack_into("<I", header, 16, len(disk))
    digest = hashlib.sha1()
    for section in (kernel, disk, b""):
        digest.update(section)
        digest.update(struct.pack("<I", len(section)))
    header[576:608] = digest.digest().ljust(32, b"\0")
    return header + kernel.ljust(padded_size(len(kernel)), b"\0")


def write_image(args, disk=b""):
    if len(disk) > args.ramdisk_limit:
        raise ValueError("the boot disk exceeds the loader's mapped ramdisk limit")
    data = pack(args.template.read_bytes(), args.image.read_bytes(), disk)
    # Publish the private file only after a complete write, without replacing
    # any existing image or boot template.
    with tempfile.NamedTemporaryFile(dir=args.output.parent) as output:
        output.write(data)
        output.write(disk)
        page = struct.unpack_from("<I", data, 36)[0]
        output.write(bytes(-len(disk) % page))
        output.flush()
        os.link(output.name, args.output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("template", type=Path)
    parser.add_argument("image", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--boot-disk", type=Path, help="raw GPT image for arm64-external-boot-disk")
    parser.add_argument("--ramdisk-limit", type=int, default=0, metavar="BYTES",
                        help="maximum ramdisk size that the target loader can map")
    args = parser.parse_args()
    if args.boot_disk is not None and args.ramdisk_limit <= 0:
        parser.error("--boot-disk requires the target loader's --ramdisk-limit")
    try:
        if args.boot_disk is None:
            write_image(args)
        else:
            with args.boot_disk.open("rb") as disk_file:
                with mmap.mmap(disk_file.fileno(), 0, access=mmap.ACCESS_READ) as disk:
                    write_image(args, disk)
    except (OSError, ValueError, zlib.error) as error:
        parser.exit(1, f"cannot prepare the development boot image: {error}\n")


if __name__ == "__main__":
    main()

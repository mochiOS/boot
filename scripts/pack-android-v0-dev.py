#!/usr/bin/env python3
"""Prepare a single-use Fastboot Image for a board using Android v0 + appended DTB."""

import argparse
import ctypes
import gzip
import hashlib
import os
from pathlib import Path
import struct
import tempfile
import zlib


FDT_MAGIC = b"\xd0\x0d\xfe\xed"
MAX_DTB = 2 * 1024 * 1024
MAX_IMAGE = 128 * 1024 * 1024


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


def pack(template, image):
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
    if not len(image) <= memory_size <= MAX_IMAGE:
        raise ValueError("invalid AArch64 Image memory span")
    kernel = gzip.compress(image, mtime=0) + prepare_dtb(decoder.unused_data)

    header = bytearray(template[:page])
    struct.pack_into("<I", header, 8, len(kernel))
    struct.pack_into("<I", header, 16, 0)  # The native Image contains its own boot module.
    digest = hashlib.sha1(kernel + struct.pack("<3I", len(kernel), 0, 0)).digest()
    header[576:608] = digest.ljust(32, b"\0")
    return header + kernel.ljust(padded_size(len(kernel)), b"\0")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("template", type=Path)
    parser.add_argument("image", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        data = pack(args.template.read_bytes(), args.image.read_bytes())
        # Publish the private file only after a complete write, without replacing
        # any existing image or boot template.
        with tempfile.NamedTemporaryFile(dir=args.output.parent) as output:
            output.write(data)
            output.flush()
            os.link(output.name, args.output)
    except (OSError, ValueError, zlib.error) as error:
        parser.exit(1, f"cannot prepare the development boot image: {error}\n")


if __name__ == "__main__":
    main()

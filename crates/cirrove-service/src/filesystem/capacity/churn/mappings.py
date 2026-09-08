"""Synthetic FUSE client; controlled by the combined namespace fixture."""
import mmap
import os
import pathlib
import sys
import time

mode, revision = sys.argv[1], int(sys.argv[2])
paths = [pathlib.Path(path) for path in sys.argv[3:6]]
size = 8192


def expected(route, version):
    bias = 0 if route == 0 else 71
    return bytes((offset + 17 * version + bias) % 251 for offset in range(size))


def open_maps(version):
    maps, inodes = [], []
    for route, path in enumerate(paths):
        with open(path, "rb") as file:
            assert file.read() == expected(route, version)
            view = mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ)
            assert view[:] == expected(route, version)
            maps.append(view)
            inodes.append(os.fstat(file.fileno()).st_ino)
        # The mapping alone must keep its kernel file and ancestor route alive.
    assert len(set(inodes)) == 3
    return maps, inodes


if mode == "offline":
    wanted = [int(value) for value in sys.argv[6:9]]
    assert len(wanted) == 3
    views, inodes = open_maps(revision)
    assert inodes == wanted
    for view in views:
        view.close()
    print("offline", flush=True)
else:
    assert mode == "hold"
    old, old_inodes = open_maps(revision)
    print("mapped", flush=True)
    assert sys.stdin.readline().strip() == "update"
    deadline = time.monotonic() + 4
    while any(os.stat(path).st_ino == inode for path, inode in zip(paths, old_inodes)):
        assert time.monotonic() < deadline, "updated mapping inode did not become visible"
        time.sleep(0.005)
    new, new_inodes = open_maps(revision + 1)
    assert all(a != b for a, b in zip(old_inodes, new_inodes))
    for route, view in enumerate(old):
        assert view[:] == expected(route, revision)
    print("updated", flush=True)
    assert sys.stdin.readline().strip() == "finish"
    for route, path in enumerate(paths):
        assert old[route][:] == expected(route, revision)
        assert new[route][:] == expected(route, revision + 1)
        with open(path, "rb") as file:
            assert os.fstat(file.fileno()).st_ino == new_inodes[route]
            assert file.read() == expected(route, revision + 1)
    for view in old + new:
        view.close()
    print("released", flush=True)

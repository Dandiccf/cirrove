#!/usr/bin/env python3
"""Every crate the binaries are built from must be under a licence compatible
with distributing those binaries under Apache-2.0 -- which for a Rust tree
means permissive ones. A crate offering a choice (`A OR B`) is fine if any
choice is on the list; a crate requiring all of several (`A AND B`) needs
each. Fails, naming the crate, on anything else, so a copyleft dependency
arriving through a transitive bump is a CI failure and not a surprise at
release time.

Prints the summary docs/distribution.md quotes.
"""
import collections
import json
import re
import subprocess
import sys

PERMISSIVE = {
    "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause",
    "BSD-3-Clause", "BSD-1-Clause", "ISC", "Zlib", "Unlicense", "CC0-1.0",
    "MIT-0", "Unicode-3.0", "Unicode-DFS-2016", "CDLA-Permissive-2.0",
    "BSL-1.0", "0BSD", "OpenSSL",
}


def acceptable(expression):
    """SPDX expression -> bool, for the `OR`/`AND`/parentheses subset crates use."""
    expression = expression.replace("/", " OR ")
    tokens = re.findall(r"\(|\)|AND|OR|[A-Za-z0-9.+-]+(?: WITH [A-Za-z0-9.+-]+)?", expression)

    def parse_or(pos):
        value, pos = parse_and(pos)
        while pos < len(tokens) and tokens[pos] == "OR":
            right, pos = parse_and(pos + 1)
            value = value or right
        return value, pos

    def parse_and(pos):
        value, pos = parse_atom(pos)
        while pos < len(tokens) and tokens[pos] == "AND":
            right, pos = parse_atom(pos + 1)
            value = value and right
        return value, pos

    def parse_atom(pos):
        if tokens[pos] == "(":
            value, pos = parse_or(pos + 1)
            return value, pos + 1  # the ")"
        return tokens[pos] in PERMISSIVE, pos + 1

    value, pos = parse_or(0)
    return value and pos == len(tokens)


def main():
    metadata = json.loads(
        subprocess.check_output(["cargo", "metadata", "--format-version", "1", "--locked"])
    )
    workspace = {p["id"] for p in metadata["packages"] if p["source"] is None}
    summary = collections.Counter()
    rejected = []
    for package in metadata["packages"]:
        if package["id"] in workspace:
            continue
        licence = package.get("license")
        if not licence:
            rejected.append((package["name"], package["version"], "no licence expression"))
            continue
        summary[licence] += 1
        if not acceptable(licence):
            rejected.append((package["name"], package["version"], licence))
    print(f"{sum(summary.values())} crates outside the workspace")
    for licence, count in summary.most_common():
        print(f"{count:4}  {licence}")
    if rejected:
        print("\nnot acceptable:")
        for name, version, licence in rejected:
            print(f"  {name} {version}: {licence}")
        return 1
    print("\nevery crate is under a licence the binaries may be distributed under")
    return 0


if __name__ == "__main__":
    sys.exit(main())

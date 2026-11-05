#!/usr/bin/env python3
"""Record the source and fixture identity associated with a local SDK run."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


INCLUDES = [
    "Cargo.toml",
    "Cargo.lock",
    "crates/**/*.rs",
    "crates/**/*.sql",
    "crates/**/Cargo.toml",
    "tests/**/*.py",
    "tests/**/*.json",
    "tests/requirements-sdk.txt",
    "tests/fixtures/*",
]


def source_manifest(root: Path) -> dict:
    files = sorted({path for pattern in INCLUDES for path in root.glob(pattern) if path.is_file()})
    digest = hashlib.sha256()
    hashes = {}
    for path in files:
        relative = path.relative_to(root).as_posix()
        content = path.read_bytes()
        digest.update(relative.encode("utf-8") + b"\0" + content + b"\0")
        hashes[relative] = hashlib.sha256(content).hexdigest()
    return {
        "schema_version": 1,
        "source_sha256": digest.hexdigest(),
        "algorithm": "SHA-256 over each sorted repository-relative POSIX path encoded UTF-8, NUL, file bytes, NUL",
        "includes": INCLUDES,
        "file_count": len(files),
        "files": list(hashes),
        "file_sha256": hashes,
        "qualification": "Source and fixture snapshot associated with the local integration run; this is not remote binary attestation.",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path, help="Write JSON here instead of standard output")
    args = parser.parse_args()
    serialized = json.dumps(source_manifest(args.root.resolve()), indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(serialized)
    else:
        print(serialized, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

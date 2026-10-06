"""Packs each directory beside this script into the bundle the fixture
server serves, example/fixtures/bundles/<directory>.zip, and prints the
digest an XR pins for it.

The archives are deterministic for a given zlib (sorted entries, a fixed
timestamp and mode), but the committed .zip files are the fixtures: rerun
this only after changing a dashboard, then pin the printed digests in the
XRs. `make bundles` runs it.
"""

import hashlib
import pathlib
import zipfile

here = pathlib.Path(__file__).resolve().parent
out = here.parent / "fixtures" / "bundles"
out.mkdir(parents=True, exist_ok=True)

for src in sorted(p for p in here.iterdir() if p.is_dir()):
    dst = out / f"{src.name}.zip"
    with zipfile.ZipFile(dst, "w") as bundle:
        for f in sorted(p for p in src.rglob("*") if p.is_file()):
            entry = zipfile.ZipInfo(f.relative_to(src).as_posix(), date_time=(2026, 1, 1, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.external_attr = 0o644 << 16
            bundle.writestr(entry, f.read_bytes(), compresslevel=9)
    digest = hashlib.sha256(dst.read_bytes()).hexdigest()
    print(f"example/fixtures/bundles/{dst.name}  sha256:{digest}")

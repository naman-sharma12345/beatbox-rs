#!/usr/bin/env python3
"""Prepare the recorded-instrument regression inputs on a clean machine.

Only the existing approved VCSL drum/Yamaha piano selections are fetched.
Every asset is pinned by upstream commit, byte length, and SHA-256.
"""
import concurrent.futures
import hashlib
import json
from pathlib import Path
import tempfile
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests/fixtures/recorded"
manifest = json.loads((FIXTURES / "manifest.json").read_text())
cache = ROOT / "target/test-fixture-assets"
cache.mkdir(parents=True, exist_ok=True)


def fetch(asset):
    target = cache / (asset["sha256"] + ".wav")
    def valid(data):
        return (len(data) == asset["bytes"]
                and hashlib.sha256(data).hexdigest() == asset["sha256"])
    if target.exists() and valid(target.read_bytes()):
        return
    url = ("https://raw.githubusercontent.com/" + manifest["repository"] + "/"
           + manifest["commit"] + "/" + urllib.parse.quote(asset["path"]))
    with urllib.request.urlopen(url, timeout=90) as response:
        data = response.read()
    if not valid(data):
        raise RuntimeError("Fixture asset integrity failure: " + asset["path"])
    target.write_bytes(data)


with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
    list(pool.map(fetch, manifest["assets"]))
for name, destination in [
        ("recorded-groove.json", Path(tempfile.gettempdir()) / "recorded-beat-project/recorded-groove.json"),
        ("keys-demo.json", Path(tempfile.gettempdir()) / "keys-demo.json"),
        ("v202-gui-after.json", Path(tempfile.gettempdir()) / "v202-gui-after.json"),
        ("roll-empty-fixture.json", Path(tempfile.gettempdir()) / "roll-empty-fixture.json"),
        ("room-demo.json", Path(tempfile.gettempdir()) / "room-demo.json")]:
    project = json.loads((FIXTURES / name).read_text())
    for sample in project["samples"]:
        sample["path"] = str(cache / Path(sample["path"]).name)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(project, indent=2) + "\n")
print("Prepared", len(manifest["assets"]), "verified VCSL assets and five recorded test projects.")
